use axum::{
    extract::FromRequestParts,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};

use serde::Deserialize;

#[derive(PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontEnd {
    Default,
    Tmtu,
}

impl FrontEnd {
    const COOKIE_NAME: &'static str = "front-end";

    fn cookie(&self) -> String {
        let name = match self {
            FrontEnd::Default => "default",
            FrontEnd::Tmtu => "tmtu",
        };

        format!("{}={}", Self::COOKIE_NAME, name)
    }
}

impl<S> FromRequestParts<S> for FrontEnd
where
    S: Send + Sync,
{
    type Rejection = String;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Ok(headers) = HeaderMap::from_request_parts(parts, state).await;
        for header in headers.get_all(header::COOKIE) {
            if let Ok(value) = header.to_str() {
                for c in value.split(';').map(|s| s.trim()) {
                    let mut split = c.split('=');
                    if Some(Self::COOKIE_NAME) == split.next() {
                        match split.next() {
                            Some("default") => return Ok(Self::Default),
                            Some("tmtu") => return Ok(Self::Tmtu),
                            _ => (),
                        }
                    }
                }
            }
        }
        Ok(Self::Default)
    }
}

pub fn set_front_end(front: FrontEnd) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::LOCATION, HeaderValue::from_static("/"));
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&front.cookie()).unwrap(),
    );
    (headers, StatusCode::FOUND)
}
