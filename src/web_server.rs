use std::sync::Arc;

use actix::{Actor, Addr, Handler, Message, SyncArbiter, SyncContext};
use actix_web::{get, middleware::Logger, web, App, HttpServer, Responder};
use askama::actix_web::TemplateIntoResponse;
use askama::Template;

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

impl Handler<PlaylistRequest> for BotExecutor {
    type Result = Result<Vec<BotData>, ()>;

    fn handle(&mut self, _: PlaylistRequest, _: &mut Self::Context) -> Self::Result {
        let bot = &self.0;

        Ok(bot.bot_datas())
    }
}

struct PlaylistRequest;

impl Message for PlaylistRequest {
    type Result = Result<Vec<BotData>, ()>;
}

#[derive(Template)]
#[template(path = "index.htm")]
struct PlaylistTemplate<'a> {
    bots: &'a [BotData],
}

#[derive(Debug)]
pub struct BotData {
    pub name: String,
    pub state: crate::bot::State,
    pub volume: f64,
    pub currently_playing: Option<AudioMetadata>,
    pub playlist: Vec<AudioMetadata>,
}

#[get("/")]
async fn index(bot: web::Data<Addr<BotExecutor>>) -> impl Responder {
    let bot_datas = match bot.send(PlaylistRequest).await.unwrap() {
        Ok(data) => data,
        Err(_) => {
            //error!("Playlist error: {}", e);
            Vec::with_capacity(0)
        }
    };

    PlaylistTemplate {
        bots: &bot_datas[..],
    }
    .into_response()
}
