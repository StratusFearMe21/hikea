use std::{ops::Deref, sync::Arc};

use color_eyre::eyre::{self, eyre, Context, OptionExt};
use emath::{Align2, Pos2, Vec2};
use magick_rust::MagickWand;
use serenity::all::{
    CommandInteraction, CreateAttachment, CreateCommand, CreateInteractionResponseFollowup,
    EditScheduledEvent, Permissions, ResolvedTarget,
};
use tracing::instrument;

use crate::AppState;

pub fn create_command() -> CreateCommand {
    CreateCommand::new("Inject hike into recent event")
        .default_member_permissions(Permissions::MANAGE_EVENTS)
        .kind(serenity::all::CommandType::Message)
}

#[instrument(skip_all)]
pub async fn respond(
    command: &CommandInteraction,
    state: Arc<AppState>,
) -> eyre::Result<CreateInteractionResponseFollowup> {
    let guild = command
        .guild_id
        .ok_or_eyre("Command was not sent from a Guild")?;

    let scheduled_events = guild
        .scheduled_events(state.http.load().deref(), false)
        .await
        .wrap_err("Failed to grab scheduled events for guild")?;

    let target_event = scheduled_events
        .iter()
        .max_by_key(|event| event.start_time)
        .ok_or_eyre("Most recently scheduled event not found")?;

    let ResolvedTarget::Message(message) = command
        .data
        .target()
        .ok_or_eyre("Could not resolve command target")?
    else {
        return Err(eyre!("Command target was not a message"));
    };

    let target_embed = message
        .embeds
        .get(0)
        .ok_or_eyre("Target message was not an embed")?;

    let embed_image = reqwest::get(
        target_embed
            .image
            .as_ref()
            .ok_or_eyre("Target embed did not have an image")?
            .url
            .as_str(),
    )
    .await
    .wrap_err("Failed to download image linked in target embed")?
    .error_for_status()
    .wrap_err("Failed to download image linked in target embed")?
    .bytes()
    .await
    .wrap_err("Failed to get bytes from image linked in embed")?;

    let wand = MagickWand::new();
    wand.read_image_blob(embed_image)
        .wrap_err("Failed to downloaded image from target embed")?;

    let image_size = emath::Rect::from_min_size(
        Pos2::ZERO,
        Vec2::new(
            wand.get_image_width() as f32,
            wand.get_image_height() as f32,
        ),
    );

    let fit = if image_size.width() < image_size.height() {
        Vec2::new(image_size.width() / (5.0 / 2.0), image_size.width())
    } else {
        Vec2::new(image_size.height(), image_size.height() / (5.0 / 2.0))
    };

    let fit = Align2::CENTER_TOP.align_size_within_rect(fit, image_size);

    wand.crop_image(
        fit.width() as usize,
        fit.height() as usize,
        fit.min.x as isize,
        fit.min.y as isize,
    )
    .wrap_err("Failed to crop image in MagickWand")?;

    let image = wand
        .write_image_blob("jpeg")
        .wrap_err("Failed to write image from MagickWand")?;

    let mut edit_event = EditScheduledEvent::new()
        .name(
            target_embed
                .title
                .as_ref()
                .ok_or_eyre("Target embed did not have a title")?,
        )
        .image(&CreateAttachment::bytes(image, "trail.jpg"));
    let mut description = target_embed.description.clone().unwrap_or_default();

    description.push_str("\n\n");

    for field in &target_embed.fields {
        std::fmt::Write::write_fmt(
            &mut description,
            format_args!("**{}**: {}\n", field.name, field.value),
        )
        .wrap_err("Failed to write to description string for event")?;
    }
    description.pop();

    edit_event = edit_event.description(description);

    guild
        .edit_scheduled_event(state.http.load().deref(), target_event.id, edit_event)
        .await
        .wrap_err("Failed to edit scheduled event")?;

    Ok(CreateInteractionResponseFollowup::new()
        .content("Success")
        .ephemeral(true))
}
