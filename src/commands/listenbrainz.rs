use std::{
    borrow::Cow,
    num::NonZeroU64,
    time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::eyre::{self, eyre, Context, OptionExt};
use serde::{Deserialize, Serialize};
use serenity::all::{
    Color, CommandOptionType, CreateButton, CreateCommand, CreateCommandOption, CreateEmbed,
    CreateEmbedAuthor, CreateInteractionResponse, CreateInteractionResponseMessage, ResolvedOption,
    ResolvedValue,
};
use tracing::instrument;

use crate::ComponentId;

pub fn create_command() -> CreateCommand {
    CreateCommand::new("listenbrainz")
        .description("Start tracking listens from ListenBrainz user for car")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "user",
                "A username on ListenBrainz",
            )
            .required(true),
        )
}

#[derive(Debug)]
pub struct ListenbrainzCommand<'a> {
    user: &'a str,
}

impl<'a> ListenbrainzCommand<'a> {
    #[instrument]
    pub fn from_options(options: &[ResolvedOption<'a>]) -> eyre::Result<Self> {
        match options.get(0).ok_or_eyre("No arguments were passed")? {
            ResolvedOption {
                value: ResolvedValue::String(user),
                ..
            } => Ok(ListenbrainzCommand { user }),
            _ => Err(eyre!("Option passed was not the right type")),
        }
    }

    #[instrument]
    pub fn respond(self) -> eyre::Result<CreateInteractionResponse> {
        let component = ComponentId::Listenbrainz {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .wrap_err("Failed to get SystemTime unix timestamp")?
                .as_secs(),
            user: std::borrow::Cow::Borrowed(self.user),
        };

        Ok(CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new().embed(
                CreateEmbed::new()
                    .title("See what's playing!")
                    .description(
                        "Riding in Richard's Navy Blue Chrysler? See what's playing on the aux!",
                    )
                    .image("https://listenbrainz.org/static/img/listenbrainz_logo_icon.svg")
                    .color(Color::PURPLE).url(format!("https://listenbrainz.org/user/{}", self.user)),
            )
            .button(CreateButton::new(
                serde_json::to_string(&component)
                    .wrap_err("Failed to serialize component ID")?
            )
            .label("We're there!")
        )))
    }
}

#[derive(Serialize)]
pub struct ListenbrainzBody {
    min_ts: u64,
}

#[derive(Deserialize, Debug)]
struct ListenbrainzListens<'a> {
    payload: Payload<'a>,
}

#[derive(Deserialize, Debug, Default)]
struct Payload<'a> {
    listens: Vec<Listen<'a>>,
}

#[derive(Deserialize, Debug)]
struct Listen<'a> {
    listened_at: Option<NonZeroU64>,
    track_metadata: TrackMetadata<'a>,
}

#[derive(Deserialize, Debug)]
struct TrackMetadata<'a> {
    additional_info: Option<AdditionalInfo<'a>>,
    artist_name: Cow<'a, str>,
    track_name: Cow<'a, str>,
    release_name: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Debug)]
struct AdditionalInfo<'a> {
    // media_player: Cow<'a, str>,
    // submission_client: Cow<'a, str>,
    // submission_client_version: Cow<'a, str>,
    release_mbid: Option<Cow<'a, str>>,
    // artist_mbids: Vec<Cow<'a, str>>,
    // recording_mbid: Cow<'a, str>,
    // duration_ms: u64,
}

pub async fn update_message(
    time: u64,
    user: &str,
) -> eyre::Result<CreateInteractionResponseMessage> {
    let mut listens: ListenbrainzListens = reqwest::Client::new()
        .get(format!(
            "https://api.listenbrainz.org/1/user/{}/listens",
            user
        ))
        .query(&ListenbrainzBody { min_ts: time })
        .send()
        .await
        .wrap_err("Failed to obtain ListenBrainz listens")?
        .json()
        .await
        .wrap_err("Failed to get JSON from ListenBrainz listens response")?;

    listens.payload.listens.sort_by_key(|p| {
        p.listened_at
            .unwrap_or_else(|| NonZeroU64::new(u64::MAX).unwrap())
    });

    let embeds = listens
        .payload
        .listens
        .into_iter()
        .map(|listen| {
            CreateEmbed::new()
                .author(CreateEmbedAuthor::new(listen.track_metadata.artist_name))
                .title(listen.track_metadata.track_name)
                .description(listen.track_metadata.release_name.unwrap_or_default())
                .image(
                    listen
                        .track_metadata
                        .additional_info
                        .as_ref()
                        .and_then(|ai| {
                            ai.release_mbid.as_ref().map(|rmbid| {
                                format!("https://coverartarchive.org/release/{}/front-500", rmbid)
                            })
                        })
                        .unwrap_or_default(),
                )
                .url(
                    listen
                        .track_metadata
                        .additional_info
                        .and_then(|ai| {
                            ai.release_mbid
                                .map(|rmbid| format!("https://listenbrainz.org/album/{}", rmbid))
                        })
                        .unwrap_or_default(),
                )
                .color(Color::PURPLE)
        })
        .collect::<Vec<_>>();

    Ok(CreateInteractionResponseMessage::new()
        .embeds(embeds)
        .components(Vec::new()))
}
