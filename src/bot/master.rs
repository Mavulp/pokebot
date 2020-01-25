use std::future::Future;
use std::sync::{Arc, Mutex};

use futures::future::{FutureExt, TryFutureExt};
use futures01::future::Future as Future01;
use log::{info};
use serde::{Deserialize, Serialize};
use tsclientlib::{ClientId, ConnectOptions, Identity, MessageTarget};

use crate::audio_player::AudioPlayerError;
use crate::teamspeak::TeamSpeakConnection;

use crate::Args;

use crate::bot::{MusicBot, MusicBotMessage, MusicBotArgs};

pub struct MasterBot {
    config: MasterConfig,
    teamspeak: Option<Arc<TeamSpeakConnection>>,
    connected_bots: Arc<Mutex<Vec<Arc<MusicBot>>>>,
}

impl MasterBot {
    pub async fn new(args: MasterArgs) -> (Arc<Self>, impl Future) {
        let (tx, mut rx) = tokio02::sync::mpsc::unbounded_channel();
        let tx = Arc::new(Mutex::new(tx));
        let connection = if args.local {
            info!("Starting in CLI mode");

            None
        } else {
            info!("Starting in TeamSpeak mode");

            let mut con_config = ConnectOptions::new(args.address.clone())
                .version(tsclientlib::Version::Linux_3_3_2)
                .name(args.name.clone())
                .identity(args.id)
                .log_commands(args.verbose >= 1)
                .log_packets(args.verbose >= 2)
                .log_udp_packets(args.verbose >= 3);

            if let Some(channel) = args.channel {
                con_config = con_config.channel(channel);
            }

            let connection = Arc::new(
                TeamSpeakConnection::new(tx.clone(), con_config)
                    .await
                    .unwrap(),
            );

            Some(connection)
        };

        let config = MasterConfig {
            name: args.name,
            address: args.address,
            bots: args.bots,
            local: args.local,
            verbose: args.verbose,
        };

        let bot = Arc::new(Self {
            config,
            teamspeak: connection,
            connected_bots: Arc::new(Mutex::new(Vec::new())),
        });

        let cbot = bot.clone();
        let msg_loop = async move {
            loop {
                while let Some(msg) = rx.recv().await {
                    cbot.on_message(msg).await.unwrap();
                }
            }
        };

        (bot, msg_loop)
    }

    async fn spawn_bot(&self, id: ClientId) {
        let channel = if let Some(ts) = &self.teamspeak {
            ts.channel_path_of_user(id)
        } else {
            String::from("local")
        };

        info!("Connecting to {} on {}", channel, self.config.address);
        let preset = self.config.bots[0].clone();
        let bot_args = MusicBotArgs {
            name: format!("{}({})", preset.name, self.config.name),
            owner: preset.owner,
            local: self.config.local,
            address: self.config.address.clone(),
            id: preset.id,
            channel,
            verbose: self.config.verbose,
        };

        let (app, fut) = MusicBot::new(bot_args).await;
        tokio::spawn(fut.unit_error().boxed().compat().map(|_| ()));
        let mut bots = self.connected_bots.lock().expect("Mutex was not poisoned");
        bots.push(app);
    }

    async fn on_message(&self, message: MusicBotMessage) -> Result<(), AudioPlayerError> {
        if let MusicBotMessage::TextMessage(message) = message {
            if let MessageTarget::Poke(who) = message.target {
                info!("Poked by {}, creating bot for their channel", who);
                self.spawn_bot(who).await;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MasterArgs {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_local")]
    pub local: bool,
    pub address: String,
    pub channel: Option<String>,
    #[serde(default = "default_verbose")]
    pub verbose: u8,
    pub id: Identity,
    pub bots: Vec<BotConfig>,
}

fn default_name() -> String {
    String::from("PokeBot")
}

fn default_local() -> bool {
    false
}

fn default_verbose() -> u8 {
    0
}

impl MasterArgs {
    pub fn merge(self, args: Args) -> Self {
        let address = args.address.unwrap_or(self.address);
        let local = args.local || self.local;
        let channel = args.master_channel.or(self.channel);
        let verbose = if args.verbose > 0 {
            args.verbose
        } else {
            self.verbose
        };

        Self {
            name: self.name,
            bots: self.bots,
            local,
            address,
            id: self.id,
            channel,
            verbose,
        }
    }
}

pub struct MasterConfig {
    pub name: String,
    pub address: String,
    pub bots: Vec<BotConfig>,
    pub local: bool,
    pub verbose: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BotConfig {
    pub name: String,
    #[serde(
        deserialize_with = "client_id_deserialize",
        serialize_with = "client_id_serialize"
    )]
    pub owner: Option<ClientId>,
    pub id: Identity,
}

fn client_id_serialize<S>(c: &Option<ClientId>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match c {
        Some(c) => s.serialize_some(&c.0),
        None => s.serialize_none(),
    }
}

fn client_id_deserialize<'de, D>(deserializer: D) -> Result<Option<ClientId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let id: Option<u16> = Deserialize::deserialize(deserializer)?;

    Ok(id.map(|id| ClientId(id)))
}
