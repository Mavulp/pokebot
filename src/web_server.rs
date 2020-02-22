use std::sync::Arc;
use std::time::Duration;

use actix::{Addr, SyncArbiter};
use actix_web::{
    get, http::header, middleware::Logger, post, web, App, HttpResponse, HttpServer, Responder,
};
use askama::actix_web::TemplateIntoResponse;
use askama::Template;
use serde::{Deserialize, Serialize};

use crate::bot::MasterBot;
use crate::youtube_dl::AudioMetadata;

mod api;
mod bot_executor;
mod default;
mod front_end_cookie;
mod tmtu;
pub use bot_executor::*;
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
            .service(get_bot)
            .service(post_front_end)
            .service(
                web::scope("/api")
                    .service(api::get_bot_list)
                    .service(api::get_bot),
            )
            .service(web::scope("/docs").service(get_api_docs))
            .service(actix_files::Files::new("/static", "web_server/static/"))
    })
    .bind(args.bind_address)?
    .run()
    .await?;

    args.bot.quit(String::from("Stopping"));

    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FrontEndForm {
    front_end: FrontEnd,
}

#[post("/front-end")]
async fn post_front_end(form: web::Form<FrontEndForm>) -> impl Responder {
    front_end_cookie::set_front_end(form.into_inner().front_end).await
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
async fn index(bot: web::Data<Addr<BotExecutor>>, front: FrontEnd) -> impl Responder {
    match front {
        FrontEnd::Default => default::index(bot).await,
        FrontEnd::Tmtu => tmtu::index(bot).await,
    }
}

#[get("/bot/{name}")]
async fn get_bot(
    bot: web::Data<Addr<BotExecutor>>,
    name: web::Path<String>,
    front: FrontEnd,
) -> impl Responder {
    match front {
        FrontEnd::Tmtu => tmtu::get_bot(bot, name.into_inner()).await,
        FrontEnd::Default => Ok(HttpResponse::Found().header(header::LOCATION, "/").finish()),
    }
}

#[derive(Template)]
#[template(path = "docs/api.htm")]
struct ApiDocsTemplate;

#[get("/api")]
async fn get_api_docs() -> impl Responder {
    ApiDocsTemplate.into_response()
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
