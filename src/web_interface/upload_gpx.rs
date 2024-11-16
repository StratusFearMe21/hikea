use std::{
    io::Cursor,
    ops::Deref,
    sync::{atomic::Ordering, Arc},
};

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::Redirect,
};
use color_eyre::eyre::{self, eyre, Context, OptionExt};
use gpx::Gpx;
use maud::DOCTYPE;
use serenity::all::{ChannelId, Color, CreateEmbed, EditMessage, MessageId};
use tracing::instrument;

use crate::{error::WithStatusCode, AppState};

#[instrument(skip(state, claims))]
pub async fn page(
    State(state): State<Arc<AppState>>,
    Path((channel_id, message_id)): Path<(ChannelId, MessageId)>,
    claims: super::Claims,
) -> Result<Redirect, crate::error::HtmlError> {
    match claims {
        super::Claims::Authenticated { .. } => {}
        super::Claims::Unauthenticated { .. } => {
            return Err(eyre!("You are not authenticated")).with_redirect(std::borrow::Cow::Owned(
                format!(
                    "/hikea/oauth2?redirect=/hikea/upload_gpx/{}/{}",
                    channel_id.get(),
                    message_id.get()
                ),
            ));
        }
    }

    let response = state
        .http
        .load()
        .get_message(channel_id, message_id)
        .await
        .wrap_err("Failed to get Discord interaction response")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?;

    let link = response
        .embeds
        .get(0)
        .ok_or_eyre("No embeds in passed Discord response")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?
        .url
        .as_ref()
        .ok_or_eyre("No URL in passed embed in Discord response")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?;

    state
        .alltrails_message_on
        .0
        .store(channel_id.get(), Ordering::Release);

    state
        .alltrails_message_on
        .1
        .store(message_id.get(), Ordering::Release);

    Ok(Redirect::to(&link))
}

pub struct UploadForm {
    pub title: String,
    pub difficulty: String,
    pub rating: String,
    pub image: String,
    pub description: String,
    pub gpx_file: Gpx,
}

impl UploadForm {
    #[instrument(skip_all)]
    async fn try_from_multipart(mut multipart: Multipart) -> Result<Self, eyre::Report> {
        let trail_title = multipart
            .next_field()
            .await
            .wrap_err("Failed to decode multipart field")?
            .ok_or_eyre("Multipart form missing fields")?;

        let trail_title = trail_title
            .text()
            .await
            .wrap_err("Failed to obtain text for multipart field")?;

        if trail_title.is_empty() {
            return Err(eyre!("Title for trail was not present"));
        }

        let trail_difficulty = multipart
            .next_field()
            .await
            .wrap_err("Failed to decode multipart field")?
            .ok_or_eyre("Multipart form missing fields")?;

        let trail_difficulty = trail_difficulty
            .text()
            .await
            .wrap_err("Failed to obtain text for multipart field")?;

        if trail_difficulty.is_empty() {
            return Err(eyre!("difficulty for trail was not present"));
        }

        let trail_rating = multipart
            .next_field()
            .await
            .wrap_err("Failed to decode multipart field")?
            .ok_or_eyre("Multipart form missing fields")?;

        let trail_rating = trail_rating
            .text()
            .await
            .wrap_err("Failed to obtain text for multipart field")?;

        if trail_rating.is_empty() {
            return Err(eyre!("rating for trail was not present"));
        }

        let trail_image = multipart
            .next_field()
            .await
            .wrap_err("Failed to decode multipart field")?
            .ok_or_eyre("Multipart form missing fields")?;

        let trail_image = trail_image
            .text()
            .await
            .wrap_err("Failed to obtain text for multipart field")?;

        if trail_image.is_empty() {
            return Err(eyre!("image for trail was not present"));
        }

        let trail_description = multipart
            .next_field()
            .await
            .wrap_err("Failed to decode multipart field")?
            .ok_or_eyre("Multipart form missing fields")?;

        let trail_description = trail_description
            .text()
            .await
            .wrap_err("Failed to obtain text for multipart field")?;

        if trail_description.is_empty() {
            return Err(eyre!("description for trail was not present"));
        }

        let gpx_file = multipart
            .next_field()
            .await
            .wrap_err("Failed to decode multipart field")?
            .ok_or_eyre("Multipart form contained no fields")?;
        let gpx_file_bytes = gpx_file
            .bytes()
            .await
            .wrap_err("Failed to obtain bytes for multipart field")?;

        Ok(Self {
            title: trail_title,
            difficulty: trail_difficulty,
            rating: trail_rating,
            image: trail_image,
            description: trail_description,
            gpx_file: gpx::read(Cursor::new(gpx_file_bytes)).wrap_err("Failed to read GPX file")?,
        })
    }
}

#[instrument(skip(state, claims))]
pub async fn post(
    State(state): State<Arc<AppState>>,
    claims: super::Claims,
    multipart: Multipart,
) -> Result<maud::Markup, crate::error::HtmlError> {
    let config = state.config.load();
    let (channel_id, message_id): (ChannelId, MessageId) = (
        state.alltrails_message_on.0.load(Ordering::Acquire).into(),
        state.alltrails_message_on.1.load(Ordering::Acquire).into(),
    );
    match claims {
        super::Claims::Authenticated { .. } => {}
        super::Claims::Unauthenticated { .. } => {
            return Err(eyre!("You are not authenticated")).with_redirect(std::borrow::Cow::Owned(
                format!(
                    "/hikea/oauth2?redirect=/hikea/upload_gpx/{}/{}",
                    channel_id.get(),
                    message_id.get()
                ),
            ));
        }
    }

    let form = UploadForm::try_from_multipart(multipart)
        .await
        .wrap_err("Failed to read multipart form")
        .with_status_code_html(StatusCode::BAD_REQUEST)?;

    let response = state
        .http
        .load()
        .get_message(channel_id, message_id)
        .await
        .wrap_err("Failed to get Discord interaction response")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?;

    let link = response
        .embeds
        .get(0)
        .ok_or_eyre("No embeds in passed Discord response")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?
        .url
        .as_ref()
        .ok_or_eyre("No URL in passed embed in Discord response")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?;

    let embed = crate::commands::suggest::embed_from_gpx(
        link,
        config.short_units,
        config.long_units,
        config.avg_speed,
        form,
    )
    .wrap_err("Failed to create Discord embed from GPX file")
    .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?;

    let react_embed = CreateEmbed::new()
        .color(Color::DARK_GREEN)
        .title("React with ⛰️ if interested");

    let http = state.http.load();
    http.get_message(channel_id, message_id)
        .await
        .wrap_err("Failed to obtain trail request interaction response from Discord")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?
        .edit(
            http.deref(),
            EditMessage::new()
                .embeds(vec![embed, react_embed])
                .components(Vec::new()),
        )
        .await
        .wrap_err("Failed to update embed for trail suggestion on Discord")
        .with_status_code_html(StatusCode::INTERNAL_SERVER_ERROR)?;

    let html = maud::html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width";
                title { "Upload GPX for AllTrails trail" }
            }
            body {
                h1 {
                    "Success"
                }
            }
        }
    };

    Ok(html)
}
