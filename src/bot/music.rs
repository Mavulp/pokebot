use std::future::Future;
use std::io::BufRead;
use std::sync::{Arc, Mutex};
use std::thread;

use humantime;
use log::{debug, info};
use structopt::StructOpt;
use tokio02::sync::mpsc::UnboundedSender;
use tsclientlib::{data, ChannelId, ClientId, ConnectOptions, Identity, Invoker, MessageTarget};

use crate::audio_player::{AudioPlayer, AudioPlayerError, PollResult, Seek};
use crate::command::Command;
use crate::playlist::Playlist;
use crate::teamspeak::TeamSpeakConnection;
use crate::youtube_dl::AudioMetadata;

#[derive(Debug)]
pub struct Message {
    pub target: MessageTarget,
    pub invoker: Invoker,
    pub text: String,
}

fn parse_seek(mut amount: &str) -> Result<Seek, ()> {
    let sign = match amount.chars().next() {
        Some('+') => 1,
        Some('-') => -1,
        _ => 0,
    };
    let is_relative = sign != 0;

    if is_relative {
        amount = &amount[1..];
    }

    let duration = humantime::parse_duration(amount).map_err(|_| ())?;

    match sign {
        1 => Ok(Seek::Positive(duration)),
        -1 => Ok(Seek::Negative(duration)),
        _ => Ok(Seek::Absolute(duration)),
    }
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
    ClientChannel {
        client: ClientId,
        old_channel: ChannelId,
    },
    ClientDisconnected {
        id: ClientId,
        client: data::Client,
    },
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
                .name(format!("🎵 {}", args.name))
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
            name: args.name.clone(),
            player,
            teamspeak: connection,
            playlist,
            state: Arc::new(Mutex::new(State::Stopped)),
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
                        cbot.with_teamspeak(|ts| ts.disconnect(&reason));
                        disconnect_cb(name, name_index, id_index);
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

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn my_channel(&self) -> ChannelId {
        self.teamspeak
            .as_ref()
            .map(|ts| ts.my_channel())
            .expect("my_channel needs ts")
    }

    fn user_count(&self, channel: ChannelId) -> u32 {
        self.teamspeak
            .as_ref()
            .map(|ts| ts.user_count(channel))
            .expect("user_count needs ts")
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
                        self.player.stop_current()?;
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
            Command::Seek { amount } => {
                if let Ok(seek) = parse_seek(&amount) {
                    if let Ok(time) = self.player.seek(seek) {
                        self.send_message(&format!("New position: {}", time));
                    } else {
                        self.send_message("Failed to seek");
                    }
                } else {
                    info!("Failed to parse seeking command");
                }
            }
            Command::Next => {
                let playlist = self.playlist.lock().expect("Mutex was not poisoned");
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
                    self.set_nickname(&format!("🎵 {} - Playing", self.name));
                }
                State::Paused => {
                    self.set_nickname(&format!("🎵 {} - Paused", self.name));
                }
                State::Stopped => {
                    self.set_nickname(&format!("🎵 {}", self.name));
                    self.set_description("");
                }
                State::EndOfStream => {
                    let next_track = self.playlist.lock().expect("Mutex was not poisoned").pop();
                    if let Some(request) = next_track {
                        info!("Advancing playlist");

                        self.start_playing_audio(request);
                    } else {
                        self.set_nickname(&format!("🎵 {}", self.name));
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
            MusicBotMessage::ClientChannel {
                client: _,
                old_channel,
            } => {
                self.on_client_left_channel(old_channel);
            }
            MusicBotMessage::ClientDisconnected { id: _, client } => {
                let old_channel = client.channel;
                self.on_client_left_channel(old_channel);
            }
            MusicBotMessage::StateChange(state) => {
                self.on_state(state)?;
            }
            MusicBotMessage::Quit(_) => (),
        }

        Ok(())
    }

    fn on_client_left_channel(&self, old_channel: ChannelId) {
        let my_channel = self.my_channel();
        if old_channel == my_channel && self.user_count(my_channel) <= 1 {
            self.quit(String::from("Channel is empty"));
        }
    }

    pub fn quit(&self, reason: String) {
        self.player.quit(reason);
    }
}

fn spawn_stdin_reader(tx: Arc<Mutex<UnboundedSender<MusicBotMessage>>>) {
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

                let tx = tx.lock().unwrap();
                tx.send(message).unwrap();
            }
        })
        .expect("Failed to spawn stdin reader thread");
}

fn spawn_gstreamer_thread(
    player: Arc<AudioPlayer>,
    tx: Arc<Mutex<UnboundedSender<MusicBotMessage>>>,
) {
    thread::Builder::new()
        .name(String::from("gstreamer polling"))
        .spawn(move || loop {
            if player.poll() == PollResult::Quit {
                break;
            }

            tx.lock()
                .unwrap()
                .send(MusicBotMessage::StateChange(State::EndOfStream))
                .unwrap();
        })
        .expect("Failed to spawn gstreamer thread");
}
