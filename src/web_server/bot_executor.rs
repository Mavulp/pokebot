use std::sync::Arc;

use actix::{Actor, Context, Handler, Message};

use crate::bot::MasterBot;
use crate::web_server::BotData;

pub struct BotExecutor(pub Arc<MasterBot>);

impl Actor for BotExecutor {
    type Context = Context<Self>;
}

pub struct BotNameListRequest;

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

pub struct BotDataListRequest;

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

pub struct BotDataRequest(pub String);

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
