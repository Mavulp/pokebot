use futures::future::{ok, Ready};

use actix_web::{
    dev::Payload,
    http::header::{COOKIE, LOCATION, SET_COOKIE},
    FromRequest, HttpRequest, HttpResponse,
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

impl FromRequest for FrontEnd {
    type Error = ();
    type Future = Ready<Result<Self, ()>>;
    type Config = ();

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        for header in req.headers().get_all(COOKIE) {
            if let Ok(value) = header.to_str() {
                for c in value.split(';').map(|s| s.trim()) {
                    let mut split = c.split('=');
                    if Some(Self::COOKIE_NAME) == split.next() {
                        match split.next() {
                            Some("default") => return ok(FrontEnd::Default),
                            Some("tmtu") => return ok(FrontEnd::Tmtu),
                            _ => (),
                        }
                    }
                }
            }
        }

        ok(FrontEnd::Default)
    }
}

pub fn set_front_end(front: FrontEnd) -> HttpResponse {
    HttpResponse::Found()
        .header(SET_COOKIE, front.cookie())
        .header(LOCATION, "/")
        .finish()
}
