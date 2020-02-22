use actix::Addr;
use actix_web::{web, Error, HttpResponse};
use askama::actix_web::TemplateIntoResponse;
use askama::Template;

use crate::web_server::{filters, BotData, BotDataListRequest, BotExecutor};

#[derive(Template)]
#[template(path = "index.htm")]
struct OverviewTemplate<'a> {
    bots: &'a [BotData],
}

pub async fn index(bot: web::Data<Addr<BotExecutor>>) -> Result<HttpResponse, Error> {
    let bot_datas = match bot.send(BotDataListRequest).await.unwrap() {
        Ok(data) => data,
        Err(_) => Vec::with_capacity(0),
    };

    OverviewTemplate {
        bots: &bot_datas[..],
    }
    .into_response()
}
