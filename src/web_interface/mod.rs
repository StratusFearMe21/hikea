use std::{sync::Arc, time::Duration};

use axum::{
    async_trait,
    extract::{FromRequestParts, OriginalUri, Query, State},
    http::{request::Parts, StatusCode},
    response::Redirect,
};
use axum_extra::extract::{cookie::Cookie, CookieJar};
use color_eyre::eyre::{eyre, Context};
use jsonwebtoken::{get_current_timestamp, DecodingKey, EncodingKey, Validation};
use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, CsrfToken, PkceCodeChallenge, PkceCodeVerifier,
    Scope, TokenResponse, TokenUrl,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use serenity::all::PartialMember;
use tracing::instrument;

use crate::{
    error::{PropogateRequest, WithStatusCode},
    AppState,
};

pub mod home_page;
pub mod upload_gpx;

pub struct Keys {
    pub encoding: EncodingKey,
    pub decoding: DecodingKey,
}

impl Keys {
    pub fn new() -> Result<Self, ring::error::Unspecified> {
        let doc = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())?;
        let encoding_key = EncodingKey::from_ed_der(doc.as_ref());

        let pair = Ed25519KeyPair::from_pkcs8(doc.as_ref())?;
        let decoding_key = DecodingKey::from_ed_der(pair.public_key().as_ref());

        Ok(Self {
            encoding: encoding_key,
            decoding: decoding_key,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "claims")]
pub enum Claims {
    Authenticated {
        member: PartialMember,
        exp: u64,
    },
    Unauthenticated {
        csrf_token: CsrfToken,
        pkce_verifier: PkceCodeVerifier,
        redirect_to: Option<String>,
        exp: u64,
    },
}

// #[derive(Debug, Serialize, Deserialize)]
// pub struct AlltrailsClaims<'a> {
//     pub link: String,
//     pub command_token: Cow<'a, str>,
//     pub exp: u64,
// }

// impl<'a> AlltrailsClaims<'a> {
//     #[instrument(skip(encoding_key))]
//     pub async fn encode(
//         link: String,
//         encoding_key: &EncodingKey,
//         command_token: Cow<'a, str>,
//     ) -> eyre::Result<String> {
//         let mut encoded = Vec::new();
//         async_compression::tokio::bufread::BrotliEncoder::with_quality_and_params(
//             Cursor::new(
//                 jsonwebtoken::encode(
//                     &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
//                     &AlltrailsClaims {
//                         link,
//                         command_token,
//                         exp: u64::MAX,
//                     },
//                     encoding_key,
//                 )
//                 .wrap_err("Failed to encode JWT Claims")?,
//             ),
//             async_compression::Level::Best,
//             EncoderParams::default().text_mode(),
//         )
//         .read_to_end(&mut encoded)
//         .await
//         .wrap_err("Failed to compress AllTrails claims")?;

//         Ok(base64::prelude::BASE64_URL_SAFE.encode(encoded))
//     }

//     pub async fn decode(
//         claims: String,
//         decoding_key: &DecodingKey,
//     ) -> eyre::Result<TokenData<AlltrailsClaims<'static>>> {
//         let mut decoded = Vec::new();

//         async_compression::tokio::bufread::BrotliDecoder::new(Cursor::new(
//             base64::prelude::BASE64_URL_SAFE
//                 .decode(claims)
//                 .wrap_err("Failed to decode base64 AllTrails claim")?,
//         ))
//         .read_to_end(&mut decoded)
//         .await
//         .wrap_err("Failed to decompress AllTrails claims")?;

//         jsonwebtoken::decode::<AlltrailsClaims>(
//             std::str::from_utf8(&decoded)
//                 .wrap_err("Decompressed AllTrails claims contained invalid UTF-8")?,
//             decoding_key,
//             &Validation::new(jsonwebtoken::Algorithm::EdDSA),
//         )
//         .wrap_err("Failed to decode JWT for AllTrails link")
//     }
// }

#[derive(Deserialize, Serialize)]
pub struct OauthQuery {
    redirect: Option<String>,
}

#[instrument(skip_all)]
pub async fn initiate_oauth2(
    Query(query): Query<OauthQuery>,
    State(state): State<Arc<AppState>>,
) -> (CookieJar, Redirect) {
    let config = state.config.load();
    let client = BasicClient::new(
        config.client_id.clone(),
        Some(config.client_secret.clone()),
        AuthUrl::new("https://discord.com/oauth2/authorize".to_owned()).unwrap(),
        Some(TokenUrl::new("https://discord.com/api/oauth2/token".to_owned()).unwrap()),
    )
    .set_redirect_uri(config.redirect_url.clone());

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scopes([Scope::new("guilds.members.read".to_owned())])
        .set_pkce_challenge(pkce_challenge)
        .url();

    let jar = CookieJar::new().add(Cookie::new(
        "jwt_session",
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
            &Claims::Unauthenticated {
                csrf_token,
                pkce_verifier,
                exp: get_current_timestamp() + 60 * 15,
                redirect_to: query.redirect.map(|r| format!("{}{}", config.hostname, r)),
            },
            &state.keys.encoding,
        )
        .unwrap(),
    ));

    (jar, Redirect::to(auth_url.as_str()))
}

#[derive(Deserialize)]
pub struct Oauth2Response {
    code: AuthorizationCode,
    state: CsrfToken,
}

#[instrument(skip_all)]
pub async fn redirect_oauth2(
    State(state): State<Arc<AppState>>,
    claims: Claims,
    Query(response): Query<Oauth2Response>,
) -> Result<(CookieJar, Redirect), super::error::HtmlError> {
    let config = state.config.load();
    let client = BasicClient::new(
        config.client_id.clone(),
        Some(config.client_secret.clone()),
        AuthUrl::new("https://discord.com/oauth2/authorize".to_owned()).unwrap(),
        Some(TokenUrl::new("https://discord.com/api/oauth2/token".to_owned()).unwrap()),
    )
    .set_redirect_uri(config.redirect_url.clone());

    let (redirect_to, pkce_verifier) = match claims {
        Claims::Unauthenticated {
            csrf_token,
            pkce_verifier,
            redirect_to,
            ..
        } => {
            if response.state.secret() != csrf_token.secret() {
                return Err(eyre!("CSRF token in cookie does not match token in state"))
                    .with_status_code_html(StatusCode::UNAUTHORIZED)?;
            }
            (redirect_to, pkce_verifier)
        }
        _ => {
            return Err(eyre!("Already authenticated"))
                .with_status_code_html(StatusCode::BAD_REQUEST)?;
        }
    };

    let token_result = client
        .exchange_code(response.code)
        .set_pkce_verifier(pkce_verifier)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .wrap_err("Failed to obtain token from Discord")
        .with_status_code_html(StatusCode::UNAUTHORIZED)?;

    let member: PartialMember = reqwest::Client::new()
        .get(format!(
            "https://discord.com/api/users/@me/guilds/{}/member",
            config.guild_id
        ))
        .header(
            "Authorization",
            format!("Bearer {}", token_result.access_token().secret()),
        )
        .send()
        .await
        .wrap_err_with(|| format!("Failed to obtain user from guild `{}`", config.guild_id))
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?
        .propogate_request_if_err()?
        .json()
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to deserialize user from guild `{}`",
                config.guild_id
            )
        })
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?;

    if member
        .roles
        .iter()
        .any(|role| config.admin_roles.contains(role))
    {
        let jar = CookieJar::new().add(Cookie::new(
            "jwt_session",
            jsonwebtoken::encode(
                &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
                &Claims::Authenticated {
                    member,
                    exp: get_current_timestamp()
                        + token_result
                            .expires_in()
                            .unwrap_or_else(|| Duration::from_secs(3600))
                            .as_secs(),
                },
                &state.keys.encoding,
            )
            .wrap_err("Failed to encode JWT Claims")
            .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?,
        ));
        Ok((
            jar,
            Redirect::to(redirect_to.as_ref().map(|r| r.as_str()).unwrap_or("/hikea")),
        ))
    } else {
        Err(eyre!("You do not have any admin role"))
            .with_status_code_html(StatusCode::UNAUTHORIZED)?
    }
}

#[async_trait]
impl FromRequestParts<Arc<AppState>> for Claims {
    type Rejection = Redirect;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let jar: (CookieJar, OriginalUri) = FromRequestParts::from_request_parts(parts, state)
            .await
            .unwrap();

        if let Some(jwt) = jar.0.get("jwt_session").and_then(|jwt| {
            jsonwebtoken::decode::<Claims>(
                jwt.value(),
                &state.keys.decoding,
                &Validation::new(jsonwebtoken::Algorithm::EdDSA),
            )
            .ok()
        }) {
            Ok(jwt.claims)
        } else {
            Err(Redirect::to(&format!(
                "/hikea/oauth2?redirect={}",
                jar.1 .0.path()
            )))
        }
    }
}
