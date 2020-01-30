use std::sync::Arc;

use actix::{Actor, Addr, Handler, Message, SyncArbiter, SyncContext};
use actix_web::{get, middleware::Logger, web, App, HttpResponse, HttpServer, Responder};

use crate::bot::MasterBot;

struct GetNames;

impl Message for GetNames {
    type Result = Result<Vec<String>, ()>;
}

#[get("/")]
async fn index(bot: web::Data<Addr<BotExecutor>>) -> impl Responder {
    let names = bot.send(GetNames).await.unwrap().unwrap();
    HttpResponse::Ok().body(&format!("Music bots connected: {}", names.join(", ")))
}

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

impl Handler<GetNames> for BotExecutor {
    type Result = Result<Vec<String>, ()>;

    fn handle(&mut self, _: GetNames, _: &mut Self::Context) -> Self::Result {
        let bot = &self.0;

        Ok(bot.names())
    }
}
