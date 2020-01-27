use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use futures::future::{FutureExt, TryFutureExt};
use log::{debug, info};
use structopt::clap::AppSettings;
use structopt::StructOpt;
use tokio02::sync::mpsc::UnboundedSender;
use tsclientlib::{ClientId, ConnectOptions, Identity, Invoker, MessageTarget};

mod audio_player;
mod command;
mod playlist;
mod teamspeak;
mod youtube_dl;

use audio_player::*;
use playlist::*;
use teamspeak::*;
use youtube_dl::AudioMetadata;
use command::Command;

#[derive(StructOpt, Debug)]
#[structopt(raw(global_settings = "&[AppSettings::ColoredHelp]"))]
struct Args {
    #[structopt(short = "l", long = "local", help = "Run locally in text mode")]
    local: bool,
    #[structopt(
        short = "a",
        long = "address",
        default_value = "localhost",
        help = "The address of the server to connect to"
    )]
    address: String,
    #[structopt(
        short = "i",
        long = "id",
        help = "Identity file - good luck creating one",
        parse(from_os_str)
    )]
    id_path: Option<PathBuf>,
    #[structopt(
        short = "c",
        long = "channel",
        help = "The channel the bot should connect to"
    )]
    default_channel: Option<String>,
    #[structopt(
        short = "v",
        long = "verbose",
        help = "Print the content of all packets",
        parse(from_occurrences)
    )]
    verbose: u8,
    // 0. Print nothing
    // 1. Print command string
    // 2. Print packets
    // 3. Print udp packets
}

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
pub enum ApplicationMessage {
    TextMessage(Message),
    StateChange(State),
}

struct Application {
    player: Arc<AudioPlayer>,
    teamspeak: Option<Arc<TeamSpeakConnection>>,
    playlist: Arc<Mutex<Playlist>>,
    state: Arc<Mutex<State>>,
}

impl Application {
    pub fn new(
        player: Arc<AudioPlayer>,
        playlist: Arc<Mutex<Playlist>>,
        teamspeak: Option<Arc<TeamSpeakConnection>>,
    ) -> Self {
        Self {
            player,
            teamspeak,
            playlist,
            state: Arc::new(Mutex::new(State::Stopped)),
        }
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
        match youtube_dl::get_audio_download_url(url).await {
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
        }

        Ok(())
    }

    fn on_state(&self, state: State) -> Result<(), AudioPlayerError> {
        let mut current_state = self.state.lock().unwrap();
        if *current_state != state {
            match state {
                State::Playing => {
                    self.set_nickname("PokeBot - Playing");
                }
                State::Paused => {
                    self.set_nickname("PokeBot - Paused");
                }
                State::Stopped => {
                    self.set_nickname("PokeBot");
                    self.set_description("");
                }
                State::EndOfStream => {
                    let next_track = self.playlist.lock().expect("Mutex was not poisoned").pop();
                    if let Some(request) = next_track {
                        info!("Advancing playlist");

                        self.start_playing_audio(request);
                    } else {
                        self.set_nickname("PokeBot");
                        self.set_description("");
                    }
                }
            }
        }

        *current_state = state;

        Ok(())
    }

    pub async fn on_message(&self, message: ApplicationMessage) -> Result<(), AudioPlayerError> {
        match message {
            ApplicationMessage::TextMessage(message) => {
                if let MessageTarget::Poke(who) = message.target {
                    info!("Poked by {}, joining their channel", who);
                    self.with_teamspeak(|ts| ts.join_channel_of_user(who));
                } else {
                    self.on_text(message).await?;
                }
            }
            ApplicationMessage::StateChange(state) => {
                self.on_state(state)?;
            }
        }

        Ok(())
    }
}

fn main() {
    log4rs::init_file("log4rs.yml", Default::default()).unwrap();

    tokio::run(async_main().unit_error().boxed().compat());
}

async fn async_main() {
    info!("Starting PokeBot!");

    // Parse command line options
    let args = Args::from_args();

    debug!("Received CLI arguments: {:?}", std::env::args());

    let (tx, mut rx) = tokio02::sync::mpsc::unbounded_channel();
    let tx = Arc::new(Mutex::new(tx));
    let (player, connection) = if args.local {
        info!("Starting in CLI mode");
        let audio_player = AudioPlayer::new(tx.clone(), None).unwrap();

        (audio_player, None)
    } else {
        info!("Starting in TeamSpeak mode");

        let id = if let Some(path) = args.id_path {
            let mut file = std::fs::File::open(path).expect("Failed to open id file");
            let mut content = String::new();
            file.read_to_string(&mut content)
                .expect("Failed to read id file");

            toml::from_str(&content).expect("Failed to parse id file")
        } else {
            Identity::create().expect("Failed to create id")
        };

        let mut con_config = ConnectOptions::new(args.address)
            .version(tsclientlib::Version::Linux_3_3_2)
            .name(String::from("PokeBot"))
            .identity(id)
            .log_commands(args.verbose >= 1)
            .log_packets(args.verbose >= 2)
            .log_udp_packets(args.verbose >= 3);

        if let Some(channel) = args.default_channel {
            con_config = con_config.channel(channel);
        }

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
    let application = Arc::new(Application::new(
        player.clone(),
        playlist.clone(),
        connection,
    ));

    spawn_gstreamer_thread(player, tx.clone());

    if args.local {
        spawn_stdin_reader(tx);
    }

    loop {
        while let Some(msg) = rx.recv().await {
            application.on_message(msg).await.unwrap();
        }
    }
}

fn spawn_stdin_reader(tx: Arc<Mutex<UnboundedSender<ApplicationMessage>>>) {
    thread::spawn(move || {
        let stdin = ::std::io::stdin();
        let lock = stdin.lock();
        for line in lock.lines() {
            let line = line.unwrap();

            let message = ApplicationMessage::TextMessage(Message {
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
    tx: Arc<Mutex<UnboundedSender<ApplicationMessage>>>,
) {
    thread::spawn(move || loop {
        player.poll();

        tx.lock()
            .unwrap()
            .send(ApplicationMessage::StateChange(State::EndOfStream))
            .unwrap();
    });
}
