use std::sync::Arc;

use actix::{Actor, Addr, Handler, Message, SyncArbiter, SyncContext};
use actix_web::{
    get, middleware::Logger, web, App, HttpResponse, HttpServer, Responder, ResponseError,
};
use askama::actix_web::TemplateIntoResponse;
use askama::Template;
use derive_more::Display;
use serde::Serialize;

use crate::bot::MasterBot;
use crate::youtube_dl::AudioMetadata;

pub struct WebServerArgs {
    pub domain: String,
    pub bind_address: String,
    pub bot: Arc<MasterBot>,
}

#[actix_rt::main]
pub async fn start(args: WebServerArgs) -> std::io::Result<()> {
    let cbot = args.bot.clone();
    let bot_addr: Addr<BotExecutor> = SyncArbiter::start(4, move || BotExecutor(cbot.clone()));

    HttpServer::new(move || {
        App::new()
            .data(bot_addr.clone())
            .wrap(Logger::default())
            .service(index)
            .service(web::scope("/api").service(get_bot_list).service(get_bot))
            .service(actix_files::Files::new("/static", "static/"))
    })
    .bind(args.bind_address)?
    .run()
    .await?;

    args.bot.quit(String::from("Stopping"));

    Ok(())
}

pub struct BotExecutor(pub Arc<MasterBot>);

impl Actor for BotExecutor {
    type Context = SyncContext<Self>;
}

struct BotDataListRequest;

impl Message for BotDataListRequest {
    // A plain Vec does not work for some reason
    type Result = Result<Vec<BotData>, ()>;
}

impl Handler<BotDataListRequest> for BotExecutor {
    type Result = Result<Vec<BotData>, ()>;

    fn handle(&mut self, _: BotDataListRequest, _: &mut Self::Context) -> Self::Result {
        let bot = &self.0;

        Ok(bot.bot_datas())
    }
}

struct BotDataRequest(String);

impl Message for BotDataRequest {
    type Result = Option<BotData>;
}

impl Handler<BotDataRequest> for BotExecutor {
    type Result = Option<BotData>;

    fn handle(&mut self, r: BotDataRequest, _: &mut Self::Context) -> Self::Result {
        let name = r.0;
        let bot = &self.0;

        bot.bot_data(name)
    }
}

#[derive(Template)]
#[template(path = "index.htm")]
struct OverviewTemplate<'a> {
    bots: &'a [BotData],
}

#[derive(Debug, Serialize)]
pub struct BotData {
    pub name: String,
    pub state: crate::bot::State,
    pub volume: f64,
    pub currently_playing: Option<AudioMetadata>,
    pub playlist: Vec<AudioMetadata>,
}

#[get("/")]
async fn index(bot: web::Data<Addr<BotExecutor>>) -> impl Responder {
    let bot_datas = match bot.send(BotDataListRequest).await.unwrap() {
        Ok(data) => data,
        Err(_) => Vec::with_capacity(0),
    };

    OverviewTemplate {
        bots: &bot_datas[..],
    }
    .into_response()
}

#[get("/bots")]
async fn get_bot_list(bot: web::Data<Addr<BotExecutor>>) -> impl Responder {
    let bot_datas = match bot.send(BotDataListRequest).await.unwrap() {
        Ok(data) => data,
        Err(_) => Vec::with_capacity(0),
    };

    web::Json(bot_datas)
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

#[get("/bots/{name}")]
async fn get_bot(bot: web::Data<Addr<BotExecutor>>, name: web::Path<String>) -> impl Responder {
    if let Some(bot_data) = bot.send(BotDataRequest(name.into_inner())).await.unwrap() {
        Ok(web::Json(bot_data))
    } else {
        Err(ApiErrorKind::NotFound)
    }
}
