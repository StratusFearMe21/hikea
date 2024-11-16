use std::{borrow::Cow, ops::Deref, sync::Arc};

use color_eyre::eyre::{self, eyre, Context, OptionExt};
use serenity::all::{
    CommandInteraction, CreateCommand, CreateInteractionResponseMessage, ResolvedTarget,
};
use tracing::instrument;

use crate::AppState;

use super::suggest::SuggestionCommand;

pub fn create_command() -> CreateCommand {
    CreateCommand::new("Convert to hiking suggestion").kind(serenity::all::CommandType::Message)
}

#[instrument(skip_all)]
pub async fn respond(
    command: &CommandInteraction,
    state: Arc<AppState>,
) -> eyre::Result<CreateInteractionResponseMessage> {
    let ResolvedTarget::Message(message) = command
        .data
        .target()
        .ok_or_eyre("Could not resolve command target")?
    else {
        return Err(eyre!("Command target was not a message"));
    };

    let response = CreateInteractionResponseMessage::new().embed(
        SuggestionCommand {
            suggestion_link: Cow::Borrowed(&message.content),
        }
        .respond(
            command,
            Arc::clone(&state),
            message.author.display_name().to_owned(),
        )
        .await
        .wrap_err("Failed to create embed to update link message")?,
    );

    message
        .delete(state.http.load().deref())
        .await
        .wrap_err("Failed to delete message to convert")?;

    Ok(response)
}
