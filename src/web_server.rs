use std::time::Duration;

use askama::Template;
use axum::extract::Path;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, get_service, post};
use axum::{Extension, Form, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use xtra::WeakAddress;

use crate::bot::MasterBot;
use crate::youtube_dl::AudioMetadata;

mod api;
mod bot_data;
mod default;
mod front_end_cookie;
mod tmtu;
pub use bot_data::*;
use front_end_cookie::FrontEnd;

pub struct WebServerArgs {
    pub bind_address: String,
    pub bot: WeakAddress<MasterBot>,
}

pub async fn start(args: WebServerArgs, shutdown_rx: oneshot::Receiver<()>) -> std::io::Result<()> {
    let bot = args.bot;
    let bind_address = args.bind_address;

    // FIXME: Add logging
    let listener = tokio::net::TcpListener::bind(&bind_address).await?;
    axum::serve(
        listener,
        Router::new()
            .route("/", get(index))
            .route("/bot/{name}", get(get_bot))
            .route("/api/bots/", get(api::get_bot_list))
            .route("/api/bots/{name}", get(api::get_bot))
            .route("/docs/api", get(get_api_docs))
            .route("/front-end", post(post_front_end))
            .nest_service("/static", get_service(ServeDir::new("web_server/static")))
            .layer(CorsLayer::permissive())
            .layer(TraceLayer::new_for_http())
            .layer(Extension(bot.clone())),
    )
    .with_graceful_shutdown(async {
        shutdown_rx.await.unwrap();
    })
    .await?;

    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FrontEndForm {
    front_end: FrontEnd,
}

async fn post_front_end(Form(form): Form<FrontEndForm>) -> impl IntoResponse {
    front_end_cookie::set_front_end(form.front_end)
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

async fn index(Extension(bot): Extension<WeakAddress<MasterBot>>, front: FrontEnd) -> Html<String> {
    match front {
        FrontEnd::Default => default::index(bot).await,
        FrontEnd::Tmtu => tmtu::index(bot).await,
    }
}

async fn get_bot(
    Extension(bot): Extension<WeakAddress<MasterBot>>,
    Path(name): Path<String>,
    front: FrontEnd,
) -> impl IntoResponse {
    match front {
        FrontEnd::Default => default::get_bot(bot, name).await,
        FrontEnd::Tmtu => tmtu::get_bot(bot, name).await,
    }
}

#[derive(Template)]
#[template(path = "docs/api.htm")]
struct ApiDocsTemplate;

async fn get_api_docs() -> Html<String> {
    Html(ApiDocsTemplate.render().unwrap())
}

mod filters {
    use std::time::Duration;

    pub fn fmt_duration(
        duration: &Option<Duration>,
        _: &dyn askama::Values,
    ) -> Result<String, askama::Error> {
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
