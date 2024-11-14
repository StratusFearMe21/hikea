use color_eyre::eyre::eyre;
use maud::DOCTYPE;
use serenity::all::PartialMember;
use tracing::instrument;

use crate::error::WithStatusCode;

#[instrument(skip_all)]
pub async fn page(claims: super::Claims) -> Result<maud::Markup, crate::error::HtmlError> {
    let member: PartialMember = match claims {
        super::Claims::Authenticated { member, .. } => member,
        super::Claims::Unauthenticated { .. } => {
            return Err(eyre!("You are not authenticated"))
                .with_redirect(std::borrow::Cow::Borrowed("/hikea/oauth2?redirect=/hikea"));
        }
    };

    let user = member
        .nick
        .or_else(|| member.user.map(|u| u.name))
        .unwrap_or_default();

    let html = maud::html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width";
                title { "Test page" }
            }
            body {
                p { (format_args!("Hi, {}!", user)) }
            }
        }
    };

    Ok(html)
}
