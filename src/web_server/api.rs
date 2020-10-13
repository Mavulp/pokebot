use actix_web::{get, web, HttpResponse, Responder, ResponseError};
use derive_more::Display;
use serde::Serialize;
use xtra::WeakAddress;

use crate::web_server::{BotDataListRequest, BotDataRequest};
use crate::MasterBot;

#[get("/bots")]
pub async fn get_bot_list(bot: web::Data<WeakAddress<MasterBot>>) -> impl Responder {
    let bot_datas = bot.send(BotDataListRequest).await.unwrap();

    web::Json(bot_datas)
}

#[get("/bots/{name}")]
pub async fn get_bot(
    bot: web::Data<WeakAddress<MasterBot>>,
    name: web::Path<String>,
) -> impl Responder {
    if let Some(bot_data) = bot.send(BotDataRequest(name.into_inner())).await.unwrap() {
        Ok(web::Json(bot_data))
    } else {
        Err(ApiErrorKind::NotFound)
    }
}

#[derive(Serialize)]
struct ApiError {
    error: String,
    description: String,
}

#[derive(Debug, Display)]
enum ApiErrorKind {
    #[display(fmt = "Not Found")]
    NotFound,
}

impl ResponseError for ApiErrorKind {
    fn error_response(&self) -> HttpResponse {
        match *self {
            ApiErrorKind::NotFound => HttpResponse::NotFound().json(ApiError {
                error: self.to_string(),
                description: String::from("The requested resource was not found"),
            }),
        }
    }
}
