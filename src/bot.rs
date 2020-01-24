use std::future::Future;
use std::io::BufRead;
use std::sync::{Arc, Mutex};
use std::thread;

use futures::future::{FutureExt, TryFutureExt};
use futures01::future::Future as Future01;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use structopt::StructOpt;
use tokio02::sync::mpsc::UnboundedSender;
use tsclientlib::{ClientId, ConnectOptions, Identity, Invoker, MessageTarget};

use crate::audio_player::*;
use crate::command::Command;
use crate::playlist::*;
use crate::teamspeak::*;
use crate::youtube_dl::AudioMetadata;

use crate::Args;

#[derive(Debug)]
pub struct Message {
    pub target: MessageTarget,
    pub invoker: Invoker,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Playing,
    Paused,
    Stopped,
    EndOfStream,
}

#[derive(Debug)]
pub enum MusicBotMessage {
    TextMessage(Message),
    StateChange(State),
    Quit(String),
}

pub struct MusicBot {
    name: String,
    player: Arc<AudioPlayer>,
    teamspeak: Option<Arc<TeamSpeakConnection>>,
    playlist: Arc<Mutex<Playlist>>,
    state: Arc<Mutex<State>>,
}

impl MusicBot {
    pub async fn new(args: MusicBotArgs) -> (Arc<Self>, impl Future) {
        let (tx, mut rx) = tokio02::sync::mpsc::unbounded_channel();
        let tx = Arc::new(Mutex::new(tx));
        let (player, connection) = if args.local {
            info!("Starting in CLI mode");
            let audio_player = AudioPlayer::new(tx.clone(), None).unwrap();

            (audio_player, None)
        } else {
            info!("Starting in TeamSpeak mode");

            let con_config = ConnectOptions::new(args.address)
                .version(tsclientlib::Version::Linux_3_3_2)
                .name(args.name.clone())
                .identity(args.id)
                .log_commands(args.verbose >= 1)
                .log_packets(args.verbose >= 2)
                .log_udp_packets(args.verbose >= 3)
                .channel(args.channel);

            let connection = Arc::new(
                TeamSpeakConnection::new(tx.clone(), con_config)
                    .await
                    .unwrap(),
            );
            let cconnection = connection.clone();
            let audio_player = AudioPlayer::new(
                tx.clone(),
                Some(Box::new(move |samples| {
                    cconnection.send_audio_packet(samples);
                })),
            )
            .unwrap();

            (audio_player, Some(connection))
        };

        player.set_volume(0.5).unwrap();
        let player = Arc::new(player);
        let playlist = Arc::new(Mutex::new(Playlist::new()));

        spawn_gstreamer_thread(player.clone(), tx.clone());

        if args.local {
            spawn_stdin_reader(tx);
        }

        let bot = Arc::new(Self {
            name: args.name,
            player,
            teamspeak: connection,
            playlist,
            state: Arc::new(Mutex::new(State::Stopped)),
        });

        let cbot = bot.clone();
        let msg_loop = async move {
            'outer: loop {
                while let Some(msg) = rx.recv().await {
                    if let MusicBotMessage::Quit(reason) = msg {
                        cbot.with_teamspeak(|ts| ts.disconnect(&reason));
                        break 'outer;
                    }
                    cbot.on_message(msg).await.unwrap();
                }
            }
            debug!("Left message loop");
        };

        (bot, msg_loop)
    }

    #[inline(always)]
    fn with_teamspeak<F: Fn(&TeamSpeakConnection)>(&self, func: F) {
        if let Some(ts) = &self.teamspeak {
            func(&ts);
        }
    }

    fn start_playing_audio(&self, metadata: AudioMetadata) {
        if let Some(title) = metadata.title {
            self.send_message(&format!("Playing '{}'", title));
            self.set_description(&format!("Currently playing '{}'", title));
        } else {
            self.send_message("Playing unknown title");
            self.set_description("Currently playing");
        }
        self.player.reset().unwrap();
        self.player.set_source_url(metadata.url).unwrap();
        self.player.play().unwrap();
    }

    pub async fn add_audio(&self, url: String) {
        match crate::youtube_dl::get_audio_download_url(url).await {
            Ok(metadata) => {
                info!("Found audio url: {}", metadata.url);

                let mut playlist = self.playlist.lock().expect("Mutex was not poisoned");
                playlist.push(metadata.clone());

                if !self.player.is_started() {
                    if let Some(request) = playlist.pop() {
                        self.start_playing_audio(request);
                    }
                } else {
                    if let Some(title) = metadata.title {
                        self.send_message(&format!("Added '{}' to playlist", title));
                    } else {
                        self.send_message("Added to playlist");
                    }
                }
            }
            Err(e) => {
                info!("Failed to find audio url: {}", e);

                self.send_message(&format!("Failed to find url: {}", e));
            }
        }
    }

    fn send_message(&self, text: &str) {
        debug!("Sending message to TeamSpeak: {}", text);

        self.with_teamspeak(|ts| ts.send_message_to_channel(text));
    }

    fn set_nickname(&self, name: &str) {
        info!("Setting TeamsSpeak nickname to {}", name);

        self.with_teamspeak(|ts| ts.set_nickname(name));
    }

    fn set_description(&self, desc: &str) {
        info!("Setting TeamsSpeak description to {}", desc);

        self.with_teamspeak(|ts| ts.set_description(desc));
    }

    async fn on_text(&self, message: Message) -> Result<(), AudioPlayerError> {
        let msg = message.text;
        if msg.starts_with("!") {
            let tokens = msg[1..].split_whitespace().collect::<Vec<_>>();

            match Command::from_iter_safe(&tokens) {
                Ok(args) => self.on_command(args).await?,
                Err(e) if e.kind == structopt::clap::ErrorKind::HelpDisplayed => {
                    self.send_message(&format!("\n{}", e.message));
                }
                _ => (),
            }
        }

        Ok(())
    }

    async fn on_command(&self, command: Command) -> Result<(), AudioPlayerError> {
        match command {
            Command::Play => {
                let playlist = self.playlist.lock().expect("Mutex was not poisoned");

                if !self.player.is_started() {
                    if !playlist.is_empty() {
                        self.player.stop_current();
                    }
                } else {
                    self.player.play()?;
                }
            }
            Command::Add { url } => {
                // strip bbcode tags from url
                let url = url.replace("[URL]", "").replace("[/URL]", "");

                self.add_audio(url.to_string()).await;
            }
            Command::Pause => {
                self.player.pause()?;
            }
            Command::Stop => {
                self.player.reset()?;
            }
            Command::Next => {
                let playlist = self.playlist.lock().expect("Mutex was not poisoned");
                if !playlist.is_empty() {
                    info!("Skipping to next track");
                    self.player.stop_current();
                } else {
                    info!("Playlist empty, cannot skip");
                    self.player.reset()?;
                }
            }
            Command::Clear => {
                self.playlist
                    .lock()
                    .expect("Mutex was not poisoned")
                    .clear();
            }
            Command::Volume { percent: volume } => {
                let volume = volume.max(0.0).min(100.0) * 0.01;
                self.player.set_volume(volume)?;
            }
            Command::Leave => {
                self.quit(String::from("Leaving"));
            }
        }

        Ok(())
    }

    fn on_state(&self, state: State) -> Result<(), AudioPlayerError> {
        let mut current_state = self.state.lock().unwrap();
        if *current_state != state {
            match state {
                State::Playing => {
                    self.set_nickname(&format!("{} - Playing", self.name));
                }
                State::Paused => {
                    self.set_nickname(&format!("{} - Paused", self.name));
                }
                State::Stopped => {
                    self.set_nickname(&self.name);
                    self.set_description("");
                }
                State::EndOfStream => {
                    let next_track = self.playlist.lock().expect("Mutex was not poisoned").pop();
                    if let Some(request) = next_track {
                        info!("Advancing playlist");

                        self.start_playing_audio(request);
                    } else {
                        self.set_nickname(&self.name);
                        self.set_description("");
                    }
                }
            }
        }

        *current_state = state;

        Ok(())
    }

    async fn on_message(&self, message: MusicBotMessage) -> Result<(), AudioPlayerError> {
        match message {
            MusicBotMessage::TextMessage(message) => {
                if MessageTarget::Channel == message.target {
                    self.on_text(message).await?;
                }
            }
            MusicBotMessage::StateChange(state) => {
                self.on_state(state)?;
            }
            MusicBotMessage::Quit(_) => (),
        }

        Ok(())
    }

    pub fn quit(&self, reason: String) {
        self.player.quit(reason);
    }
}

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
        let channel_name = if let Some(ts) = &self.teamspeak {
            ts.channel_path_of_user(id)
        } else {
            String::from("local")
        };

        info!("Connecting to {} on {}", channel_name, self.config.address);
        let preset = self.config.bots[0].clone();
        let bot_args = MusicBotArgs {
            name: preset.name,
            owner: preset.owner,
            local: self.config.local,
            address: self.config.address.clone(),
            id: preset.id,
            channel: channel_name,
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

fn spawn_stdin_reader(tx: Arc<Mutex<UnboundedSender<MusicBotMessage>>>) {
    thread::spawn(move || {
        let stdin = ::std::io::stdin();
        let lock = stdin.lock();
        for line in lock.lines() {
            let line = line.unwrap();

            let message = MusicBotMessage::TextMessage(Message {
                target: MessageTarget::Server,
                invoker: Invoker {
                    name: String::from("stdin"),
                    id: ClientId(0),
                    uid: None,
                },
                text: line,
            });

            let tx = tx.lock().unwrap();
            tx.send(message).unwrap();
        }
    });
}

fn spawn_gstreamer_thread(
    player: Arc<AudioPlayer>,
    tx: Arc<Mutex<UnboundedSender<MusicBotMessage>>>,
) {
    thread::spawn(move || loop {
        if player.poll() == PollResult::Quit {
            break;
        }

        tx.lock()
            .unwrap()
            .send(MusicBotMessage::StateChange(State::EndOfStream))
            .unwrap();
    });
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

#[derive(Debug)]
pub struct MusicBotArgs {
    name: String,
    owner: Option<ClientId>,
    local: bool,
    address: String,
    id: Identity,
    channel: String,
    verbose: u8,
}

pub struct MasterConfig {
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
