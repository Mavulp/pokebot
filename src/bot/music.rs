use std::future::Future;
use std::io::BufRead;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use log::{debug, info};
use serde::Serialize;
use structopt::StructOpt;
use tokio::sync::mpsc::UnboundedSender;
use tsclientlib::{data, ChannelId, ClientId, ConnectOptions, Identity, Invoker, MessageTarget};

use crate::audio_player::{AudioPlayer, AudioPlayerError, PollResult};
use crate::command::Command;
use crate::command::VolumeChange;
use crate::playlist::Playlist;
use crate::teamspeak as ts;
use crate::youtube_dl::AudioMetadata;
use ts::TeamSpeakConnection;

#[derive(Debug)]
pub struct Message {
    pub target: MessageTarget,
    pub invoker: Invoker,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize)]
pub enum State {
    Playing,
    Paused,
    Stopped,
    EndOfStream,
}

impl std::fmt::Display for State {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        match self {
            State::Playing => write!(fmt, "Playing"),
            State::Paused => write!(fmt, "Paused"),
            State::Stopped | State::EndOfStream => write!(fmt, "Stopped"),
        }?;

        Ok(())
    }
}

#[derive(Debug)]
pub enum MusicBotMessage {
    TextMessage(Message),
    ClientChannel {
        client: ClientId,
        old_channel: ChannelId,
    },
    ChannelAdded(ChannelId),
    ClientAdded(ClientId),
    ClientDisconnected {
        id: ClientId,
        client: Box<data::Client>,
    },
    StateChange(State),
    Quit(String),
}

pub struct MusicBot {
    name: String,
    player: Arc<AudioPlayer>,
    teamspeak: Option<TeamSpeakConnection>,
    playlist: Arc<RwLock<Playlist>>,
    state: Arc<RwLock<State>>,
}

pub struct MusicBotArgs {
    pub name: String,
    pub name_index: usize,
    pub id_index: usize,
    pub local: bool,
    pub address: String,
    pub id: Identity,
    pub channel: String,
    pub verbose: u8,
    pub disconnect_cb: Box<dyn FnMut(String, usize, usize) + Send + Sync>,
}

impl MusicBot {
    pub async fn new(args: MusicBotArgs) -> (Arc<Self>, impl Future<Output = ()>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = Arc::new(RwLock::new(tx));
        let (player, connection) = if args.local {
            info!("Starting in CLI mode");
            let audio_player = AudioPlayer::new(tx.clone(), None).unwrap();

            (audio_player, None)
        } else {
            info!("Starting in TeamSpeak mode");

            let con_config = ConnectOptions::new(args.address)
                .version(tsclientlib::Version::Linux_3_3_2)
                .name(format!("🎵 {}", args.name))
                .identity(args.id)
                .log_commands(args.verbose >= 1)
                .log_packets(args.verbose >= 2)
                .log_udp_packets(args.verbose >= 3)
                .channel(args.channel);

            let connection = TeamSpeakConnection::new(tx.clone(), con_config)
                .await
                .unwrap();
            let mut cconnection = connection.clone();
            let audio_player = AudioPlayer::new(
                tx.clone(),
                Some(Box::new(move |samples| {
                    let mut rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(cconnection.send_audio_packet(samples));
                })),
            )
            .unwrap();

            (audio_player, Some(connection))
        };

        player.change_volume(VolumeChange::Absolute(0.5)).unwrap();
        let player = Arc::new(player);
        let playlist = Arc::new(RwLock::new(Playlist::new()));

        spawn_gstreamer_thread(player.clone(), tx.clone());

        if args.local {
            spawn_stdin_reader(tx);
        }

        let bot = Arc::new(Self {
            name: args.name.clone(),
            player,
            teamspeak: connection,
            playlist,
            state: Arc::new(RwLock::new(State::EndOfStream)),
        });

        let cbot = bot.clone();
        let mut disconnect_cb = args.disconnect_cb;
        let name = args.name;
        let name_index = args.name_index;
        let id_index = args.id_index;
        let msg_loop = async move {
            'outer: loop {
                while let Some(msg) = rx.recv().await {
                    if let MusicBotMessage::Quit(reason) = msg {
                        if let Some(ts) = &cbot.teamspeak {
                            let mut ts = ts.clone();
                            ts.disconnect(&reason).await;
                        }
                        disconnect_cb(name, name_index, id_index);
                        break 'outer;
                    }
                    cbot.on_message(msg).await.unwrap();
                }
            }
            debug!("Left message loop");
        };

        bot.update_name(State::EndOfStream).await;

        (bot, msg_loop)
    }

    async fn start_playing_audio(&self, metadata: AudioMetadata) {
        let duration = if let Some(duration) = metadata.duration {
            format!("({})", ts::bold(&humantime::format_duration(duration)))
        } else {
            format!("")
        };

        self.send_message(format!(
            "Playing {} {}",
            ts::underline(&metadata.title),
            duration
        ))
        .await;
        self.set_description(format!("Currently playing '{}'", metadata.title))
            .await;
        self.player.reset().unwrap();
        self.player.set_metadata(metadata).unwrap();
        self.player.play().unwrap();
    }

    pub async fn add_audio(&self, url: String, user: String) {
        match crate::youtube_dl::get_audio_download_url(url).await {
            Ok(mut metadata) => {
                metadata.added_by = user;
                info!("Found audio url: {}", metadata.url);

                // RWLockGuard can not be kept around or the compiler complains that
                // it might cross the await boundary
                self.playlist
                    .write()
                    .expect("RwLock was not poisoned")
                    .push(metadata.clone());

                if !self.player.is_started() {
                    let entry = self
                        .playlist
                        .write()
                        .expect("RwLock was not poisoned")
                        .pop();
                    if let Some(request) = entry {
                        self.start_playing_audio(request).await;
                    }
                } else {
                    let duration = if let Some(duration) = metadata.duration {
                        format!(" ({})", ts::bold(&humantime::format_duration(duration)))
                    } else {
                        format!("")
                    };

                    self.send_message(format!(
                        "Added {}{} to playlist",
                        ts::underline(&metadata.title),
                        duration
                    ))
                    .await;
                }
            }
            Err(e) => {
                info!("Failed to find audio url: {}", e);

                self.send_message(format!("Failed to find url: {}", e))
                    .await;
            }
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> State {
        *self.state.read().expect("RwLock was not poisoned")
    }

    pub fn volume(&self) -> f64 {
        self.player.volume()
    }

    pub fn position(&self) -> Option<Duration> {
        self.player.position()
    }

    pub fn currently_playing(&self) -> Option<AudioMetadata> {
        self.player.currently_playing()
    }

    pub fn playlist_to_vec(&self) -> Vec<AudioMetadata> {
        self.playlist.read().unwrap().to_vec()
    }

    pub async fn my_channel(&self) -> ChannelId {
        let ts = self.teamspeak.as_ref().expect("my_channel needs ts");

        let mut ts = ts.clone();
        ts.my_channel().await
    }

    async fn user_count(&self, channel: ChannelId) -> u32 {
        let ts = self.teamspeak.as_ref().expect("user_count needs ts");

        let mut ts = ts.clone();
        ts.user_count(channel).await
    }

    async fn send_message(&self, text: String) {
        debug!("Sending message to TeamSpeak: {}", text);

        if let Some(ts) = &self.teamspeak {
            let mut ts = ts.clone();
            ts.send_message_to_channel(text).await;
        }
    }

    async fn set_nickname(&self, name: String) {
        info!("Setting TeamSpeak nickname: {}", name);

        if let Some(ts) = &self.teamspeak {
            let mut ts = ts.clone();
            ts.set_nickname(name).await;
        }
    }

    async fn set_description(&self, desc: String) {
        info!("Setting TeamSpeak description: {}", desc);

        if let Some(ts) = &self.teamspeak {
            let mut ts = ts.clone();
            ts.set_description(desc).await;
        }
    }

    async fn subscribe_all(&self) {
        if let Some(ts) = &self.teamspeak {
            let mut ts = ts.clone();
            ts.subscribe_all().await;
        }
    }

    async fn on_text(&self, message: Message) -> Result<(), AudioPlayerError> {
        let msg = message.text;
        if msg.starts_with('!') {
            let tokens = msg[1..].split_whitespace().collect::<Vec<_>>();

            match Command::from_iter_safe(&tokens) {
                Ok(args) => self.on_command(args, message.invoker).await?,
                Err(e) if e.kind == structopt::clap::ErrorKind::HelpDisplayed => {
                    self.send_message(format!("\n{}", e.message)).await;
                }
                _ => (),
            }
        }

        Ok(())
    }

    async fn on_command(&self, command: Command, invoker: Invoker) -> Result<(), AudioPlayerError> {
        match command {
            Command::Play => {
                let playlist = self.playlist.read().expect("RwLock was not poisoned");

                if !self.player.is_started() {
                    if !playlist.is_empty() {
                        self.player.stop_current()?;
                    }
                } else {
                    self.player.play()?;
                }
            }
            Command::Add { url } => {
                // strip bbcode tags from url
                let url = url.replace("[URL]", "").replace("[/URL]", "");

                self.add_audio(url.to_string(), invoker.name).await;
            }
            Command::Search { query } => {
                self.add_audio(format!("ytsearch:{}", query.join(" ")), invoker.name)
                    .await;
            }
            Command::Pause => {
                self.player.pause()?;
            }
            Command::Stop => {
                self.player.reset()?;
            }
            Command::Seek { amount } => {
                if let Ok(time) = self.player.seek(amount) {
                    self.send_message(format!("New position: {}", ts::bold(&time)))
                        .await;
                } else {
                    self.send_message(String::from("Failed to seek")).await;
                }
            }
            Command::Next => {
                let playlist = self.playlist.read().expect("RwLock was not poisoned");
                if !playlist.is_empty() {
                    info!("Skipping to next track");
                    self.player.stop_current()?;
                } else {
                    info!("Playlist empty, cannot skip");
                    self.player.reset()?;
                }
            }
            Command::Clear => {
                self.playlist
                    .write()
                    .expect("RwLock was not poisoned")
                    .clear();
            }
            Command::Volume { volume } => {
                self.player.change_volume(volume)?;
                self.update_name(self.state()).await;
            }
            Command::Leave => {
                self.quit(String::from("Leaving"));
            }
        }

        Ok(())
    }

    async fn update_name(&self, state: State) {
        let volume = (self.volume() * 100.0).round();
        let name = match state {
            State::EndOfStream => format!("🎵 {} ({}%)", self.name, volume),
            _ => format!("🎵 {} - {} ({}%)", self.name, state, volume),
        };
        self.set_nickname(name).await;
    }

    async fn on_state(&self, state: State) -> Result<(), AudioPlayerError> {
        let current_state = *self.state.read().unwrap();
        if current_state != state {
            match state {
                State::EndOfStream => {
                    let next_track = self
                        .playlist
                        .write()
                        .expect("RwLock was not poisoned")
                        .pop();
                    if let Some(request) = next_track {
                        info!("Advancing playlist");

                        self.start_playing_audio(request).await;
                    } else {
                        self.update_name(state).await;
                        self.set_description(String::new()).await;
                    }
                }
                State::Stopped => {
                    if current_state != State::EndOfStream {
                        self.update_name(state).await;
                        self.set_description(String::new()).await;
                    }
                }
                _ => self.update_name(state).await,
            }
        }

        if !(current_state == State::EndOfStream && state == State::Stopped) {
            *self.state.write().unwrap() = state;
        }

        Ok(())
    }

    async fn on_message(&self, message: MusicBotMessage) -> Result<(), AudioPlayerError> {
        match message {
            MusicBotMessage::TextMessage(message) => {
                if MessageTarget::Channel == message.target {
                    self.on_text(message).await?;
                }
            }
            MusicBotMessage::ClientChannel {
                client: _,
                old_channel,
            } => {
                self.on_client_left_channel(old_channel).await;
            }
            MusicBotMessage::ClientDisconnected { id: _, client } => {
                let old_channel = client.channel;
                self.on_client_left_channel(old_channel).await;
            }
            MusicBotMessage::ChannelAdded(_) => {
                // TODO Only subscribe to one channel
                self.subscribe_all().await;
            }
            MusicBotMessage::StateChange(state) => {
                self.on_state(state).await?;
            }
            _ => (),
        }

        Ok(())
    }

    async fn on_client_left_channel(&self, old_channel: ChannelId) {
        let my_channel = self.my_channel().await;
        if old_channel == my_channel && self.user_count(my_channel).await <= 1 {
            self.quit(String::from("Channel is empty"));
        }
    }

    pub fn quit(&self, reason: String) {
        self.player.quit(reason);
    }
}

fn spawn_stdin_reader(tx: Arc<RwLock<UnboundedSender<MusicBotMessage>>>) {
    debug!("Spawning stdin reader thread");
    thread::Builder::new()
        .name(String::from("stdin reader"))
        .spawn(move || {
            let stdin = ::std::io::stdin();
            let lock = stdin.lock();
            for line in lock.lines() {
                let line = line.unwrap();

                let message = MusicBotMessage::TextMessage(Message {
                    target: MessageTarget::Channel,
                    invoker: Invoker {
                        name: String::from("stdin"),
                        id: ClientId(0),
                        uid: None,
                    },
                    text: line,
                });

                let tx = tx.read().unwrap();
                tx.send(message).unwrap();
            }
        })
        .expect("Failed to spawn stdin reader thread");
}

fn spawn_gstreamer_thread(
    player: Arc<AudioPlayer>,
    tx: Arc<RwLock<UnboundedSender<MusicBotMessage>>>,
) {
    thread::Builder::new()
        .name(String::from("gstreamer polling"))
        .spawn(move || loop {
            if player.poll() == PollResult::Quit {
                break;
            }

            tx.read()
                .unwrap()
                .send(MusicBotMessage::StateChange(State::EndOfStream))
                .unwrap();
        })
        .expect("Failed to spawn gstreamer thread");
}
