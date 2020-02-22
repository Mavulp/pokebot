use std::sync::Arc;
use std::time::Duration;

use actix::{Actor, Addr, Handler, Message, SyncArbiter, SyncContext};
use actix_web::{
    get, http::header, middleware::Logger, post, web, App, Error, HttpResponse, HttpServer,
    Responder, ResponseError,
};
use askama::actix_web::TemplateIntoResponse;
use askama::Template;
use derive_more::Display;
use serde::{Deserialize, Serialize};

use crate::bot::MasterBot;
use crate::youtube_dl::AudioMetadata;

mod front_end_cookie;
use front_end_cookie::FrontEnd;

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
            .service(tmtu_bot)
            .service(post_front_end)
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

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FrontEndForm {
    front_end: FrontEnd,
}

#[post("/front-end")]
async fn post_front_end(form: web::Form<FrontEndForm>) -> Result<HttpResponse, Error> {
    front_end_cookie::set_front_end(form.into_inner().front_end).await
}

struct BotNameListRequest;

impl Message for BotNameListRequest {
    // A plain Vec does not work for some reason
    type Result = Result<Vec<String>, ()>;
}

impl Handler<BotNameListRequest> for BotExecutor {
    type Result = Result<Vec<String>, ()>;

    fn handle(&mut self, _: BotNameListRequest, _: &mut Self::Context) -> Self::Result {
        let bot = &self.0;

        Ok(bot.bot_names())
    }
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

#[derive(Template)]
#[template(path = "tmtu/index.htm")]
struct TmtuTemplate {
    bot_names: Vec<String>,
    bot: Option<BotData>,
}

#[derive(Debug, Serialize)]
pub struct BotData {
    pub name: String,
    pub state: crate::bot::State,
    pub volume: f64,
    pub position: Option<Duration>,
    pub currently_playing: Option<AudioMetadata>,
    pub playlist: Vec<AudioMetadata>,
}

#[get("/")]
async fn index(bot: web::Data<Addr<BotExecutor>>, front: FrontEnd) -> Result<HttpResponse, Error> {
    match front {
        FrontEnd::Lazy => lazy_index(bot).await,
        FrontEnd::Tmtu => tmtu_index(bot).await,
    }
}

async fn lazy_index(bot: web::Data<Addr<BotExecutor>>) -> Result<HttpResponse, Error> {
    let bot_datas = match bot.send(BotDataListRequest).await.unwrap() {
        Ok(data) => data,
        Err(_) => Vec::with_capacity(0),
    };

    OverviewTemplate {
        bots: &bot_datas[..],
    }
    .into_response()
}

async fn tmtu_index(bot: web::Data<Addr<BotExecutor>>) -> Result<HttpResponse, Error> {
    let bot_names = bot.send(BotNameListRequest).await.unwrap().unwrap();

    TmtuTemplate {
        bot_names,
        bot: None,
    }
    .into_response()
}

#[get("/tmtu/{name}")]
async fn tmtu_bot(
    bot: web::Data<Addr<BotExecutor>>,
    name: web::Path<String>,
    front: FrontEnd,
) -> Result<HttpResponse, Error> {
    if front != FrontEnd::Tmtu {
        return Ok(HttpResponse::Found().header(header::LOCATION, "/").finish());
    }

    let bot_names = bot.send(BotNameListRequest).await.unwrap().unwrap();
    if let Some(bot) = bot.send(BotDataRequest(name.into_inner())).await.unwrap() {
        TmtuTemplate {
            bot_names,
            bot: Some(bot),
        }
        .into_response()
    } else {
        Ok(HttpResponse::Found().header(header::LOCATION, "/").finish())
    }
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

mod filters {
    use std::time::Duration;

    pub fn fmt_duration(duration: &Option<Duration>) -> Result<String, askama::Error> {
        if let Some(duration) = duration {
            let secs = duration.as_secs();
            let mins = secs / 60;
            let submin_secs = secs % 60;

            Ok(format!("{:02}:{:02}", mins, submin_secs))
        } else {
            Ok(String::from("--:--"))
        }
    }
}
