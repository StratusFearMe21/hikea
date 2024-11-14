use std::{
    net::SocketAddr,
    ops::Deref,
    sync::{atomic::AtomicU64, Arc},
};

use arc_swap::ArcSwap;
use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    routing::{get, post},
    Json, Router,
};
use color_eyre::eyre::{self, eyre, Context, OptionExt};
use ed25519_dalek::{Signature, Verifier, VerifyingKey, SIGNATURE_LENGTH};
use error::WithStatusCode;
use oauth2::{ClientId, ClientSecret, RedirectUrl};
use serde::{de::Error, Deserialize};
use serenity::{
    all::{
        CreateInteractionResponse, CreateInteractionResponseFollowup,
        CreateInteractionResponseMessage,
    },
    http::{Http, HttpBuilder},
    model::{application::*, id::*},
};
use tokio::signal::unix::SignalKind;
use tower_http::trace::TraceLayer;
use tracing::*;
use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod commands;
mod error;
mod web_interface;

mod ed25519_serde {
    use ed25519_dalek::{VerifyingKey, PUBLIC_KEY_LENGTH};
    use serde::de::Error;
    use serde::Deserializer;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<VerifyingKey, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: [u8; PUBLIC_KEY_LENGTH] = hex::serde::deserialize(deserializer)?;
        VerifyingKey::from_bytes(&bytes).map_err(|e| D::Error::custom(e))
    }
}

mod uom_units {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer};
    use uom::si::length::Units;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Units, D::Error>
    where
        D: Deserializer<'de>,
    {
        let unit_single = String::deserialize(deserializer)?;

        for unit in uom::si::length::units() {
            if unit.singular() == unit_single {
                return Ok(unit);
            }
        }

        let mut units = String::from("[");

        for unit in uom::si::length::units() {
            units.push('`');
            units.push_str(unit.singular());
            units.push_str("`, ");
        }

        units.push(']');

        return Err(D::Error::invalid_value(
            serde::de::Unexpected::Str(&unit_single),
            &units.as_str(),
        ));
    }
}

#[derive(Deserialize)]
struct Config {
    address: SocketAddr,
    #[serde(with = "ed25519_serde")]
    public_key: VerifyingKey,
    token: String,
    application_id: ApplicationId,
    guild_id: GuildId,
    admin_roles: Vec<RoleId>,
    client_id: ClientId,
    client_secret: ClientSecret,
    redirect_url: RedirectUrl,
    hostname: String,
    #[serde(with = "uom_units")]
    long_units: uom::si::length::Units,
    #[serde(with = "uom_units")]
    short_units: uom::si::length::Units,
    avg_speed: f64,
}

impl Config {
    fn from_toml() -> Result<Self, toml::de::Error> {
        let config = toml::from_str::<Config>(
            &std::fs::read_to_string(
                std::env::var("CONFIG").unwrap_or_else(|_| String::from("./config.toml")),
            )
            .map_err(|e| toml::de::Error::custom(e))?,
        );
        debug!(target: "config",  "Initialized config");
        config
    }
}

type ConfigSwap = ArcSwap<Config>;

struct AppState {
    config: ConfigSwap,
    http: ArcSwap<Http>,
    keys: web_interface::Keys,
    alltrails_message_on: Arc<(AtomicU64, AtomicU64)>,
}

impl AppState {
    pub async fn derive() -> Self {
        let config = Config::from_toml().unwrap();
        AppState {
            http: ArcSwap::new(Arc::new(
                HttpBuilder::new(config.token.clone())
                    .application_id(config.application_id)
                    .build(),
            )),
            config: ArcSwap::new(Arc::new(config)),
            keys: web_interface::Keys::new().unwrap(),
            alltrails_message_on: Arc::new(Default::default()),
        }
    }

    pub async fn refresh(&self) {
        let config = Arc::new(Config::from_toml().unwrap());

        self.http.store(Arc::new(
            HttpBuilder::new(config.token.clone())
                .application_id(config.application_id)
                .build(),
        ));
        self.config.store(config);
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    magick_rust::magick_wand_genesis();
    tracing_subscriber::registry()
        .with(ErrorLayer::default())
        .with(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("info"))
                .unwrap(),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let state = Arc::new(AppState::derive().await);
    Command::set_global_commands(
        state.http.load().as_ref(),
        vec![
            commands::ping::create_command(),
            commands::suggest::create_command(),
            commands::inject::create_command(),
        ],
    )
    .await
    .wrap_err("Failed to set commands on Discord")?;

    let app = Router::new()
        .route("/hikea/discord", post(discord_interaction))
        .route("/hikea/oauth2", get(web_interface::initiate_oauth2))
        .route("/hikea/redirect", get(web_interface::redirect_oauth2))
        .route(
            "/hikea/upload_gpx/:channel_id/:message_id",
            get(web_interface::upload_gpx::page),
        )
        .route("/hikea/upload_gpx", post(web_interface::upload_gpx::post))
        .route("/hikea", get(web_interface::home_page::page))
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::clone(&state));

    let state_t = Arc::clone(&state);
    tokio::spawn(async move {
        let mut stream = tokio::signal::unix::signal(SignalKind::hangup()).unwrap();
        loop {
            stream.recv().await;
            state_t.refresh().await;
        }
    });
    let listener = tokio::net::TcpListener::bind(state.config.load().address)
        .await
        .wrap_err("Failed to bind TCP listener")?;
    axum::serve(listener, app)
        .await
        .wrap_err("Axum server failure")
}

#[instrument(skip_all)]
async fn discord_interaction(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<Json<CreateInteractionResponse>, error::DiscordError> {
    let config = state.config.load();
    let mut sig_body = headers
        .get("X-Signature-Timestamp")
        .ok_or_eyre("Failed to find `X-Signature-Timestamp` in headers")
        .with_status_code(StatusCode::BAD_REQUEST)?
        .to_str()
        .wrap_err("`X-Signature-Timestamp` was not valid UTF-8")
        .with_status_code(StatusCode::BAD_REQUEST)?
        .to_owned();

    sig_body.push_str(&body);
    let mut signature: [u8; SIGNATURE_LENGTH] = [0; SIGNATURE_LENGTH];
    hex::decode_to_slice(
        headers
            .get("X-Signature-Ed25519")
            .ok_or_eyre("Failed to find `X-Signature-Ed25519` in headers")
            .with_status_code(StatusCode::BAD_REQUEST)?
            .as_bytes(),
        &mut signature,
    )
    .wrap_err("`X-Signature-Ed25519` was not valid hex")
    .with_status_code(StatusCode::BAD_REQUEST)?;

    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));

    let signature = Signature::from_bytes(&signature);
    config
        .public_key
        .verify(sig_body.as_bytes(), &signature)
        .wrap_err("Failed to verify public key with Discord")
        .with_status_code(StatusCode::UNAUTHORIZED)?;

    let interaction_body: Interaction = serde_json::from_str(&body)
        .wrap_err("Failed to deserialize Interaction")
        .with_status_code(StatusCode::BAD_REQUEST)?;

    match interaction_body {
        Interaction::Ping(_) => return Ok(Json(CreateInteractionResponse::Pong)),
        Interaction::Command(command) => match command.data.name.as_str() {
            "ping" => Ok(Json(commands::ping::respond())),
            "suggest" => {
                let options = command.data.options();
                let suggestion_command =
                    commands::suggest::SuggestionCommand::from_options(&options)
                        .wrap_err("Failed to initialize `suggest` command")
                        .interaction_response()?;

                Ok(Json(
                    suggestion_command
                        .respond(&command, Arc::clone(&state))
                        .await
                        .wrap_err("Failed to respond to `suggest` command")
                        .interaction_response()?,
                ))
            }
            "Inject hike into recent event" => {
                let state = Arc::clone(&state);

                tokio::spawn(async move {
                    let response = commands::inject::respond(&command, Arc::clone(&state))
                        .await
                        .wrap_err("Failed to respond to `inject_hike` command")
                        .interaction_response();

                    // TODO: Handle these errors
                    match response {
                        Ok(r) => command.create_followup(state.http.load().deref(), r).await,
                        Err(e) => {
                            command
                                .create_followup(
                                    state.http.load().deref(),
                                    CreateInteractionResponseFollowup::new()
                                        .ephemeral(true)
                                        .embed(e.create_embed()),
                                )
                                .await
                        }
                    }
                });

                Ok(Json(CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(true),
                )))
            }
            name => {
                return Err(eyre!("Command `{:?}` not implemented", name)).interaction_response()?
            }
        },
        i => {
            return Err(eyre!("Interaction type `{:?}` not implemented", i.kind()))
                .with_status_code(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
