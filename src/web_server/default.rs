use askama::Template;
use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use axum::response::{Html, IntoResponse};
use xtra::WeakAddress;

use crate::web_server::{filters, BotData, BotDataRequest, BotNameListRequest};
use crate::MasterBot;

#[derive(Template)]
#[template(path = "index.htm")]
struct OverviewTemplate<'a> {
    bot_names: &'a [String],
    bot: Option<&'a BotData>,
}

pub async fn index(bot: WeakAddress<MasterBot>) -> Html<String> {
    let bot_names = bot.send(BotNameListRequest).await.unwrap();

    Html(
        OverviewTemplate {
            bot_names: &bot_names,
            bot: None,
        }
        .render()
        .unwrap(),
    )
}

pub async fn get_bot(bot: WeakAddress<MasterBot>, name: String) -> Response<Body> {
    let bot_names = bot.send(BotNameListRequest).await.unwrap();

    if let Some(bot) = bot.send(BotDataRequest(name)).await.unwrap() {
        Html(
            OverviewTemplate {
                bot_names: &bot_names,
                bot: Some(&bot),
            }
            .render()
            .unwrap(),
        )
        .into_response()
    } else {
        let mut headers = HeaderMap::new();
        headers.insert(header::LOCATION, HeaderValue::from_static("/"));
        (headers, StatusCode::FOUND).into_response()
    }
}
