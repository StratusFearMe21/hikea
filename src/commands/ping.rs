use serenity::builder::{
    CreateCommand, CreateInteractionResponse, CreateInteractionResponseMessage,
};
use tracing::instrument;

pub fn create_command() -> CreateCommand {
    CreateCommand::new("ping").description("Simple ping pong")
}

#[instrument]
pub fn respond() -> CreateInteractionResponse {
    CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content("pong"))
}
