use actix::Addr;
use actix_web::{http::header, web, Error, HttpResponse};
use askama::Template;
use askama_actix::TemplateIntoResponse;

use crate::web_server::{filters, BotData, BotDataRequest, BotExecutor, BotNameListRequest};

#[derive(Template)]
#[template(path = "tmtu/index.htm")]
struct TmtuTemplate {
    bot_names: Vec<String>,
    bot: Option<BotData>,
}

pub async fn index(bot: web::Data<Addr<BotExecutor>>) -> Result<HttpResponse, Error> {
    let bot_names = bot.send(BotNameListRequest).await.unwrap().unwrap();

    TmtuTemplate {
        bot_names,
        bot: None,
    }
    .into_response()
}

pub async fn get_bot(
    bot: web::Data<Addr<BotExecutor>>,
    name: String,
) -> Result<HttpResponse, Error> {
    let bot_names = bot.send(BotNameListRequest).await.unwrap().unwrap();

    if let Some(bot) = bot.send(BotDataRequest(name)).await.unwrap() {
        TmtuTemplate {
            bot_names,
            bot: Some(bot),
        }
        .into_response()
    } else {
        // TODO to 404 or not to 404
        Ok(HttpResponse::Found().header(header::LOCATION, "/").finish())
    }
}
