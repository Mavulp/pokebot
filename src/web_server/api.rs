use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use derive_more::Display;
use serde::Serialize;
use xtra::WeakAddress;

use crate::web_server::{BotDataListRequest, BotDataRequest};
use crate::MasterBot;

use super::BotData;

pub async fn get_bot_list(Extension(bot): Extension<WeakAddress<MasterBot>>) -> Json<Vec<BotData>> {
    let bot_datas = bot.send(BotDataListRequest).await.unwrap();

    Json(bot_datas)
}

pub async fn get_bot(
    Extension(bot): Extension<WeakAddress<MasterBot>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Some(bot_data) = bot.send(BotDataRequest(name)).await.unwrap() {
        Ok(Json(bot_data))
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

impl IntoResponse for ApiErrorKind {
    fn into_response(self) -> Response {
        match self {
            ApiErrorKind::NotFound => (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: self.to_string(),
                    description: String::from("The requested resource was not found"),
                }),
            )
                .into_response(),
        }
    }
}
