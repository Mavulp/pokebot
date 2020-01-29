use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use futures::future::{FutureExt, TryFutureExt};
use futures01::future::Future as Future01;
use log::info;
use rand::{rngs::SmallRng, seq::SliceRandom, SeedableRng};
use serde::{Deserialize, Serialize};
use tsclientlib::{ClientId, ConnectOptions, Identity, MessageTarget};

use crate::audio_player::AudioPlayerError;
use crate::teamspeak::TeamSpeakConnection;

use crate::Args;

use crate::bot::{MusicBot, MusicBotArgs, MusicBotMessage};

pub struct MasterBot {
    config: Arc<MasterConfig>,
    rng: Arc<Mutex<SmallRng>>,
    available_names: Arc<Mutex<Vec<usize>>>,
    available_ids: Arc<Mutex<Vec<usize>>>,
    teamspeak: Arc<TeamSpeakConnection>,
    connected_bots: Arc<Mutex<HashMap<String, Arc<MusicBot>>>>,
}

impl MasterBot {
    pub async fn new(args: MasterArgs) -> (Arc<Self>, impl Future) {
        let (tx, mut rx) = tokio02::sync::mpsc::unbounded_channel();
        let tx = Arc::new(Mutex::new(tx));
        info!("Starting in TeamSpeak mode");

        let mut con_config = ConnectOptions::new(args.address.clone())
            .version(tsclientlib::Version::Linux_3_3_2)
            .name(args.master_name.clone())
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

        let config = Arc::new(MasterConfig {
            master_name: args.master_name,
            address: args.address,
            names: args.names,
            ids: args.ids,
            local: args.local,
            verbose: args.verbose,
        });

        let name_count = config.names.len();
        let id_count = config.ids.len();
        let bot = Arc::new(Self {
            config,
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            available_names: Arc::new(Mutex::new((0..name_count).collect())),
            available_ids: Arc::new(Mutex::new((0..id_count).collect())),
            teamspeak: connection,
            connected_bots: Arc::new(Mutex::new(HashMap::new())),
        });

        bot.teamspeak.set_description("Poke me if you want a music bot!");

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
        let channel = self
            .teamspeak
            .channel_of_user(id)
            .expect("Can find poke sender");

        if channel == self.teamspeak.my_channel() {
            self.teamspeak.send_message_to_user(
                id,
                &format!(
                    "Joining the channel of \"{}\" is not allowed",
                    self.config.master_name
                ),
            );
            return;
        }

        for (_, bot) in &*self.connected_bots.lock().expect("Mutex was not poisoned") {
            if bot.my_channel() == channel {
                self.teamspeak.send_message_to_user(
                    id,
                    &format!(
                        "\"{}\" is already in this channel. \
                         Multiple bots in one channel are not allowed.",
                        bot.name()
                    ),
                );
                return;
            }
        }

        let channel_path = self
            .teamspeak
            .channel_path_of_user(id)
            .expect("can find poke sender");

        let (name, name_index) = {
            let mut available_names = self.available_names.lock().expect("Mutex was not poisoned");
            let mut rng = self.rng.lock().expect("Mutex was not poisoned");
            available_names.shuffle(&mut *rng);
            let name_index = match available_names.pop() {
                Some(v) => v,
                None => {
                    self.teamspeak.send_message_to_user(
                        id,
                        "Out of names. Too many bots are already connected!",
                    );
                    return;
                }
            };

            (self.config.names[name_index].clone(), name_index)
        };

        let (id, id_index) = {
            let mut available_ids = self.available_ids.lock().expect("Mutex was not poisoned");
            let mut rng = self.rng.lock().expect("Mutex was not poisoned");
            available_ids.shuffle(&mut *rng);
            let id_index = match available_ids.pop() {
                Some(v) => v,
                None => {
                    self.teamspeak.send_message_to_user(
                        id,
                        "Out of identities. Too many bots are already connected!",
                    );
                    return;
                }
            };

            (self.config.ids[id_index].clone(), id_index)
        };

        let cconnected_bots = self.connected_bots.clone();
        let cavailable_names = self.available_names.clone();
        let cavailable_ids = self.available_ids.clone();
        let disconnect_cb = Box::new(move |n, name_index, id_index| {
            let mut bots = cconnected_bots.lock().expect("Mutex was not poisoned");
            bots.remove(&n);
            cavailable_names
                .lock()
                .expect("Mutex was not poisoned")
                .push(name_index);
            cavailable_ids
                .lock()
                .expect("Mutex was not poisoned")
                .push(id_index);
        });

        info!("Connecting to {} on {}", channel_path, self.config.address);
        let bot_args = MusicBotArgs {
            name: name.clone(),
            name_index,
            id_index,
            local: self.config.local,
            address: self.config.address.clone(),
            id,
            channel: channel_path,
            verbose: self.config.verbose,
            disconnect_cb,
        };

        let (app, fut) = MusicBot::new(bot_args).await;
        tokio::spawn(fut.unit_error().boxed().compat().map(|_| ()));
        let mut bots = self.connected_bots.lock().expect("Mutex was not poisoned");
        bots.insert(name, app);
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
    pub master_name: String,
    #[serde(default = "default_local")]
    pub local: bool,
    pub address: String,
    pub channel: Option<String>,
    #[serde(default = "default_verbose")]
    pub verbose: u8,
    pub names: Vec<String>,
    pub id: Identity,
    pub ids: Vec<Identity>,
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
            master_name: self.master_name,
            names: self.names,
            ids: self.ids,
            local,
            address,
            id: self.id,
            channel,
            verbose,
        }
    }
}

pub struct MasterConfig {
    pub master_name: String,
    pub address: String,
    pub names: Vec<String>,
    pub ids: Vec<Identity>,
    pub local: bool,
    pub verbose: u8,
}
