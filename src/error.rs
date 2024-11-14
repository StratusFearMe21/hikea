//! # yaaxum-error
//! Yet Another Axum Error Handler
//!
//! This crate uses `eyre` to capture the error,
//! the error is then returned to the browser or
//! whatever it is, it's then nicely formatted to
//! a webpage using `ansi_to_html`

use std::{
    borrow::Cow,
    fmt::{Debug, Display},
};

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    Json,
};
use color_eyre::eyre::eyre;
use reqwest::Response;
use serenity::all::{
    Color, CreateEmbed, CreateInteractionResponse, CreateInteractionResponseMessage,
};

pub struct HtmlError(
    pub StatusCode,
    pub color_eyre::eyre::Report,
    pub Option<Cow<'static, str>>,
);

impl Display for HtmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.handler().display(self.1.as_ref(), f)
    }
}

impl Debug for HtmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.handler().debug(self.1.as_ref(), f)
    }
}

impl IntoResponse for HtmlError {
    fn into_response(self) -> axum::response::Response {
        if let Some(redirect) = self.2 {
            (self.0, Redirect::to(&redirect)).into_response()
        } else {
            let ansi_string = format!("{:?}", self);
            let error = ansi_to_html::convert(&ansi_string).unwrap();
            (
            self.0,
            Html(format!(
                "<!DOCTYPE html><html><head><meta charset=\"utf8\"></head><body><pre><code>{}</code></pre></body></html>",
                error
            )),
        )
            .into_response()
        }
    }
}

pub struct DiscordError(pub StatusCode, pub color_eyre::eyre::Report);

impl Display for DiscordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.handler().display(self.1.as_ref(), f)
    }
}

impl Debug for DiscordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.handler().debug(self.1.as_ref(), f)
    }
}

impl DiscordError {
    pub fn create_embed(self) -> CreateEmbed {
        let handler: &color_eyre::Handler = self.1.handler().downcast_ref().unwrap();

        let mut span_trace = Vec::new();
        let mut span_at = 0;

        handler.span_trace().unwrap().with_spans(|span, fields| {
            span_trace.push((
                span_at.to_string(),
                format!(
                    "`{}::{}`",
                    span.module_path().unwrap_or_default(),
                    span.name()
                ),
                false,
            ));
            if !fields.is_empty() {
                span_trace.push((String::from("with"), format!("`{}`", fields), true));
            }
            if let Some((file, line)) = span.file().and_then(|f| Some((f, span.line()?))) {
                span_trace.push((String::from("at"), format!("{}:{}", file, line), true));
            }
            span_at += 1;
            true
        });

        CreateEmbed::new().title("Error").color(Color::RED).fields(
            self.1
                .chain()
                .enumerate()
                .map(|(i, e)| (i.to_string(), format!("{}", e), false))
                .chain([
                    (
                        String::from("Location"),
                        format!("{}", handler.last_location().unwrap()),
                        false,
                    ),
                    (String::from("Spantrace"), String::new(), false),
                ])
                .chain(span_trace),
        )
    }

    pub fn create_interaction_response(self) -> CreateInteractionResponse {
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .ephemeral(true)
                .embed(self.create_embed()),
        )
    }
}

impl IntoResponse for DiscordError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(self.create_interaction_response())).into_response()
    }
}

pub trait WithStatusCode<T> {
    fn with_status_code_html(self, code: StatusCode) -> Result<T, HtmlError>;
    fn with_redirect(self, redirect: Cow<'static, str>) -> Result<T, HtmlError>;
    fn with_status_code(self, code: StatusCode) -> Result<T, DiscordError>;
    fn interaction_response(self) -> Result<T, DiscordError>;
}

impl<T> WithStatusCode<T> for std::result::Result<T, color_eyre::eyre::Report> {
    fn with_status_code_html(self, code: StatusCode) -> Result<T, HtmlError> {
        self.map_err(|e| HtmlError(code, e, None))
    }

    fn with_redirect(self, redirect: Cow<'static, str>) -> Result<T, HtmlError> {
        self.map_err(|e| HtmlError(StatusCode::SEE_OTHER, e, Some(redirect)))
    }

    fn with_status_code(self, code: StatusCode) -> Result<T, DiscordError> {
        self.map_err(|e| DiscordError(code, e))
    }

    fn interaction_response(self) -> Result<T, DiscordError> {
        self.map_err(|e| DiscordError(StatusCode::OK, e))
    }
}

pub trait PropogateRequest {
    fn propogate_request_if_err(self) -> Result<Response, HtmlError>;
}

impl PropogateRequest for Response {
    fn propogate_request_if_err(self) -> Result<Response, HtmlError> {
        let status = self.status();
        if status.is_server_error() || status.is_client_error() {
            return Err(eyre!("Reqwest request encountered an issue: {:?}", status))
                .with_status_code_html(status);
        }
        Ok(self)
    }
}
