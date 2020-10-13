use async_trait::async_trait;

use xtra::{Context, Handler, Message};

use crate::bot::MasterBot;
use crate::web_server::BotData;

pub struct BotNameListRequest;

impl Message for BotNameListRequest {
    type Result = Vec<String>;
}

#[async_trait]
impl Handler<BotNameListRequest> for MasterBot {
    async fn handle(&mut self, _: BotNameListRequest, _: &mut Context<Self>) -> Vec<String> {
        self.bot_names()
    }
}

pub struct BotDataListRequest;

impl Message for BotDataListRequest {
    type Result = Vec<BotData>;
}

#[async_trait]
impl Handler<BotDataListRequest> for MasterBot {
    async fn handle(&mut self, _: BotDataListRequest, _: &mut Context<Self>) -> Vec<BotData> {
        self.bot_datas().await
    }
}

pub struct BotDataRequest(pub String);

impl Message for BotDataRequest {
    type Result = Option<BotData>;
}

#[async_trait]
impl Handler<BotDataRequest> for MasterBot {
    async fn handle(&mut self, r: BotDataRequest, _: &mut Context<Self>) -> Option<BotData> {
        let name = r.0;

        self.bot_data(name).await
    }
}
