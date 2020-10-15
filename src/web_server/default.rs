use actix_web::{http::header, web, Error, HttpResponse};
use askama_actix::{Template, TemplateIntoResponse};
use xtra::WeakAddress;

use crate::web_server::{filters, BotData, BotDataRequest, BotNameListRequest};
use crate::MasterBot;

#[derive(Template)]
#[template(path = "index.htm")]
struct OverviewTemplate<'a> {
    bot_names: &'a [String],
    bot: Option<&'a BotData>,
}

pub async fn index(bot: web::Data<WeakAddress<MasterBot>>) -> Result<HttpResponse, Error> {
    let bot_names = bot.send(BotNameListRequest).await.unwrap();

    OverviewTemplate {
        bot_names: &bot_names,
        bot: None,
    }
    .into_response()
}

pub async fn get_bot(
    bot: web::Data<WeakAddress<MasterBot>>,
    name: String,
) -> Result<HttpResponse, Error> {
    let bot_names = bot.send(BotNameListRequest).await.unwrap();

    if let Some(bot) = bot.send(BotDataRequest(name)).await.unwrap() {
        OverviewTemplate {
            bot_names: &bot_names,
            bot: Some(&bot),
        }
        .into_response()
    } else {
        Ok(HttpResponse::Found().header(header::LOCATION, "/").finish())
    }
}
