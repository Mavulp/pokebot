use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};

use futures::future::{FutureExt, TryFutureExt};
use futures01::future::Future as Future01;
use log::info;
use rand::{rngs::SmallRng, seq::SliceRandom, SeedableRng};
use serde::{Deserialize, Serialize};
use tokio02::sync::mpsc::UnboundedSender;
use tsclientlib::{ClientId, ConnectOptions, Identity, MessageTarget};

use crate::audio_player::AudioPlayerError;
use crate::teamspeak::TeamSpeakConnection;

use crate::Args;

use crate::bot::{MusicBot, MusicBotArgs, MusicBotMessage};

pub struct MasterBot {
    config: Arc<MasterConfig>,
    music_bots: Arc<RwLock<MusicBots>>,
    teamspeak: Arc<TeamSpeakConnection>,
    sender: Arc<RwLock<UnboundedSender<MusicBotMessage>>>,
}

struct MusicBots {
    rng: SmallRng,
    available_names: Vec<usize>,
    available_ids: Vec<usize>,
    connected_bots: HashMap<String, Arc<MusicBot>>,
}

impl MasterBot {
    pub async fn new(args: MasterArgs) -> (Arc<Self>, impl Future) {
        let (tx, mut rx) = tokio02::sync::mpsc::unbounded_channel();
        let tx = Arc::new(RwLock::new(tx));
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

        let music_bots = Arc::new(RwLock::new(MusicBots {
            rng: SmallRng::from_entropy(),
            available_names: (0..name_count).collect(),
            available_ids: (0..id_count).collect(),
            connected_bots: HashMap::new(),
        }));

        let bot = Arc::new(Self {
            config,
            music_bots,
            teamspeak: connection,
            sender: tx.clone(),
        });

        bot.teamspeak
            .set_description("Poke me if you want a music bot!");

        let cbot = bot.clone();
        let msg_loop = async move {
            'outer: loop {
                while let Some(msg) = rx.recv().await {
                    if let MusicBotMessage::Quit(reason) = msg {
                        cbot.teamspeak.disconnect(&reason);
                        break 'outer;
                    }
                    cbot.on_message(msg).await.unwrap();
                }
            }
        };

        (bot, msg_loop)
    }

    fn build_bot_args_for(&self, id: ClientId) -> Option<MusicBotArgs> {
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
            return None;
        }

        let MusicBots {
            ref mut rng,
            ref mut available_names,
            ref mut available_ids,
            ref connected_bots,
        } = &mut *self.music_bots.write().expect("RwLock was not poisoned");

        for (_, bot) in connected_bots {
            if bot.my_channel() == channel {
                self.teamspeak.send_message_to_user(
                    id,
                    &format!(
                        "\"{}\" is already in this channel. \
                         Multiple bots in one channel are not allowed.",
                        bot.name()
                    ),
                );
                return None;
            }
        }

        let channel_path = self
            .teamspeak
            .channel_path_of_user(id)
            .expect("can find poke sender");

        available_names.shuffle(rng);
        let name_index = match available_names.pop() {
            Some(v) => v,
            None => {
                self.teamspeak
                    .send_message_to_user(id, "Out of names. Too many bots are already connected!");
                return None;
            }
        };
        let name = self.config.names[name_index].clone();

        available_ids.shuffle(rng);
        let id_index = match available_ids.pop() {
            Some(v) => v,
            None => {
                self.teamspeak.send_message_to_user(
                    id,
                    "Out of identities. Too many bots are already connected!",
                );
                return None;
            }
        };

        let id = self.config.ids[id_index].clone();

        let cmusic_bots = self.music_bots.clone();
        let disconnect_cb = Box::new(move |n, name_index, id_index| {
            let mut music_bots = cmusic_bots.write().expect("RwLock was not poisoned");
            music_bots.connected_bots.remove(&n);
            music_bots.available_names.push(name_index);
            music_bots.available_ids.push(id_index);
        });

        info!("Connecting to {} on {}", channel_path, self.config.address);

        Some(MusicBotArgs {
            name,
            name_index,
            id_index,
            local: self.config.local,
            address: self.config.address.clone(),
            id,
            channel: channel_path,
            verbose: self.config.verbose,
            disconnect_cb,
        })
    }

    async fn spawn_bot_for(&self, id: ClientId) {
        if let Some(bot_args) = self.build_bot_args_for(id) {
            let (bot, fut) = MusicBot::new(bot_args).await;
            tokio::spawn(fut.unit_error().boxed().compat().map(|_| ()));
            let mut music_bots = self.music_bots.write().expect("RwLock was not poisoned");
            music_bots
                .connected_bots
                .insert(bot.name().to_string(), bot);
        }
    }

    async fn on_message(&self, message: MusicBotMessage) -> Result<(), AudioPlayerError> {
        if let MusicBotMessage::TextMessage(message) = message {
            if let MessageTarget::Poke(who) = message.target {
                info!("Poked by {}, creating bot for their channel", who);
                self.spawn_bot_for(who).await;
            }
        }

        Ok(())
    }

    pub fn bot_data(&self, name: String) -> Option<crate::web_server::BotData> {
        let music_bots = self.music_bots.read().unwrap();

        let bot = music_bots.connected_bots.get(&name)?;

        Some(crate::web_server::BotData {
            name: name,
            state: bot.state(),
            volume: bot.volume(),
            currently_playing: bot.currently_playing(),
            playlist: bot.playlist_to_vec(),
        })
    }

    pub fn bot_datas(&self) -> Vec<crate::web_server::BotData> {
        let music_bots = self.music_bots.read().unwrap();

        let len = music_bots.connected_bots.len();
        let mut result = Vec::with_capacity(len);
        for (name, bot) in &music_bots.connected_bots {
            let bot_data = crate::web_server::BotData {
                name: name.clone(),
                state: bot.state(),
                volume: bot.volume(),
                currently_playing: bot.currently_playing(),
                playlist: bot.playlist_to_vec(),
            };

            result.push(bot_data);
        }

        result
    }

    pub fn quit(&self, reason: String) {
        let music_bots = self.music_bots.read().unwrap();
        for (_, bot) in &music_bots.connected_bots {
            bot.quit(reason.clone())
        }
        let sender = self.sender.read().unwrap();
        sender.send(MusicBotMessage::Quit(reason)).unwrap();
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
