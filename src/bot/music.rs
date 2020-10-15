use async_trait::async_trait;

use serde::Serialize;
use slog::{debug, info, Logger};
use structopt::StructOpt;
use tsclientlib::{data, ChannelId, ClientId, Connection, Identity, Invoker, MessageTarget};
use xtra::{spawn::Tokio, Actor, Address, Context, Handler, Message, WeakAddress};

use crate::audio_player::{AudioPlayer, AudioPlayerError};
use crate::bot::{BotDisonnected, Connect, MasterBot, Quit};
use crate::command::Command;
use crate::command::VolumeChange;
use crate::playlist::Playlist;
use crate::teamspeak as ts;
use crate::youtube_dl::{self, AudioMetadata};
use ts::TeamSpeakConnection;

#[derive(Debug)]
pub struct ChatMessage {
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

impl Message for State {
    type Result = ();
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
    TextMessage(ChatMessage),
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
}

impl Message for MusicBotMessage {
    type Result = Result<(), AudioPlayerError>;
}

pub struct MusicBot {
    name: String,
    identity: Identity,
    player: AudioPlayer,
    teamspeak: Option<TeamSpeakConnection>,
    master: Option<WeakAddress<MasterBot>>,
    playlist: Playlist,
    state: State,
    logger: Logger,
}

pub struct MusicBotArgs {
    pub name: String,
    pub master: Option<WeakAddress<MasterBot>>,
    pub local: bool,
    pub address: String,
    pub identity: Identity,
    pub channel: String,
    pub verbose: u8,
    pub logger: Logger,
}

impl MusicBot {
    pub async fn spawn(args: MusicBotArgs) -> Address<Self> {
        let mut player = AudioPlayer::new(args.logger.clone()).unwrap();
        player.change_volume(VolumeChange::Absolute(0.5)).unwrap();

        let playlist = Playlist::new(args.logger.clone());

        let teamspeak = if args.local {
            info!(args.logger, "Starting in CLI mode");
            player.setup_with_audio_callback(None).unwrap();

            None
        } else {
            Some(TeamSpeakConnection::new(args.logger.clone()).await.unwrap())
        };
        let bot = Self {
            name: args.name.clone(),
            master: args.master,
            identity: args.identity.clone(),
            player,
            teamspeak,
            playlist,
            state: State::EndOfStream,
            logger: args.logger.clone(),
        };

        let bot_addr = bot.create(None).spawn(&mut Tokio::Global);

        info!(
            args.logger,
            "Connecting";
            "name" => &args.name,
            "channel" => &args.channel,
            "address" => &args.address,
        );

        let opt = Connection::build(args.address)
            .logger(args.logger.clone())
            .version(tsclientlib::Version::Linux_3_3_2)
            .name(format!("🎵 {}", args.name))
            .identity(args.identity)
            .log_commands(args.verbose >= 1)
            .log_packets(args.verbose >= 2)
            .log_udp_packets(args.verbose >= 3)
            .channel(args.channel);
        bot_addr.send(Connect(opt)).await.unwrap().unwrap();
        bot_addr
            .send(MusicBotMessage::StateChange(State::EndOfStream))
            .await
            .unwrap()
            .unwrap();

        if args.local {
            debug!(args.logger, "Spawning stdin reader thread");
            spawn_stdin_reader(bot_addr.downgrade());
        }

        bot_addr
    }

    async fn start_playing_audio(&mut self, metadata: AudioMetadata) {
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

    pub async fn add_audio(&mut self, url: String, user: String) {
        match youtube_dl::get_audio_download_from_url(url, &self.logger).await {
            Ok(mut metadata) => {
                metadata.added_by = user;
                info!(self.logger, "Found source"; "url" => &metadata.url);

                self.playlist.push(metadata.clone());

                if !self.player.is_started() {
                    let entry = self.playlist.pop();
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
                info!(self.logger, "Failed to find audio url"; "error" => &e);

                self.send_message(format!("Failed to find url: {}", e))
                    .await;
            }
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub async fn volume(&self) -> f64 {
        self.player.volume()
    }

    pub async fn current_channel(&mut self) -> Option<ChannelId> {
        let ts = self.teamspeak.as_mut().expect("current_channel needs ts");

        ts.current_channel().await
    }

    async fn user_count(&mut self, channel: ChannelId) -> u32 {
        let ts = self.teamspeak.as_mut().expect("user_count needs ts");

        ts.user_count(channel).await
    }

    async fn send_message(&mut self, text: String) {
        debug!(self.logger, "Sending message to TeamSpeak"; "message" => &text);

        if let Some(ts) = &mut self.teamspeak {
            ts.send_message_to_channel(text).await;
        }
    }

    async fn set_nickname(&mut self, name: String) {
        info!(self.logger, "Setting TeamSpeak nickname"; "name" => &name);

        if let Some(ts) = &mut self.teamspeak {
            ts.set_nickname(name).await;
        }
    }

    async fn set_description(&mut self, desc: String) {
        info!(self.logger, "Setting TeamSpeak description"; "description" => &desc);

        if let Some(ts) = &mut self.teamspeak {
            ts.set_description(desc).await;
        }
    }

    async fn on_text(&mut self, message: ChatMessage) -> Result<(), AudioPlayerError> {
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

    async fn on_command(
        &mut self,
        command: Command,
        invoker: Invoker,
    ) -> Result<(), AudioPlayerError> {
        match command {
            Command::Play => {
                if !self.player.is_started() {
                    if !self.playlist.is_empty() {
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
                if !self.playlist.is_empty() {
                    info!(self.logger, "Skipping to next track");
                    self.player.stop_current()?;
                } else {
                    info!(self.logger, "Playlist empty, cannot skip");
                    self.player.reset()?;
                }
            }
            Command::Clear => {
                self.send_message(String::from("Cleared playlist")).await;
                self.playlist.clear();
            }
            Command::Volume { volume } => {
                self.player.change_volume(volume)?;
                self.update_name(self.state()).await;
            }
            Command::Leave => {
                self.quit(String::from("Leaving"), true).await.unwrap();
            }
        }

        Ok(())
    }

    async fn update_name(&mut self, state: State) {
        let volume = (self.volume().await * 100.0).round();
        let name = match state {
            State::EndOfStream => format!("🎵 {} ({}%)", self.name, volume),
            _ => format!("🎵 {} - {} ({}%)", self.name, state, volume),
        };
        self.set_nickname(name).await;
    }

    async fn on_state(&mut self, new_state: State) -> Result<(), AudioPlayerError> {
        if self.state != new_state {
            match new_state {
                State::EndOfStream => {
                    self.player.reset()?;
                    let next_track = self.playlist.pop();
                    if let Some(request) = next_track {
                        info!(self.logger, "Advancing playlist");

                        self.start_playing_audio(request).await;
                    } else {
                        self.update_name(new_state).await;
                        self.set_description(String::new()).await;
                    }
                }
                State::Stopped => {
                    if self.state != State::EndOfStream {
                        self.update_name(new_state).await;
                        self.set_description(String::new()).await;
                    }
                }
                _ => self.update_name(new_state).await,
            }
        }

        if !(self.state == State::EndOfStream && new_state == State::Stopped) {
            self.state = new_state;
        }

        Ok(())
    }

    async fn on_message(&mut self, message: MusicBotMessage) -> Result<(), AudioPlayerError> {
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
            MusicBotMessage::StateChange(state) => {
                self.on_state(state).await?;
            }
            _ => (),
        }

        Ok(())
    }

    // FIXME logs an error if this music bot is the one leaving
    async fn on_client_left_channel(&mut self, old_channel: ChannelId) {
        let current_channel = match self.current_channel().await {
            Some(c) => c,
            None => {
                return;
            }
        };
        if old_channel == current_channel && self.user_count(current_channel).await <= 1 {
            self.quit(String::from("Channel is empty"), true)
                .await
                .unwrap();
        }
    }

    pub async fn quit(
        &mut self,
        reason: String,
        inform_master: bool,
    ) -> Result<(), tsclientlib::Error> {
        // FIXME logs errors if the bot is playing something because it tries to
        // change its name and description
        self.player.reset().unwrap();

        let ts = self.teamspeak.as_mut().unwrap();
        ts.disconnect(&reason).await?;

        if inform_master {
            if let Some(master) = &self.master {
                master
                    .send(BotDisonnected {
                        name: self.name.clone(),
                        identity: self.identity.clone(),
                    })
                    .await
                    .unwrap();
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Actor for MusicBot {
    async fn started(&mut self, ctx: &mut Context<Self>) {
        let addr = ctx.address().unwrap().downgrade();
        self.player.register_bot(addr);
    }
}

#[async_trait]
impl Handler<Connect> for MusicBot {
    async fn handle(
        &mut self,
        opt: Connect,
        ctx: &mut Context<Self>,
    ) -> Result<(), tsclientlib::Error> {
        let addr = ctx.address().unwrap().downgrade();
        self.teamspeak
            .as_mut()
            .unwrap()
            .connect_for_bot(opt.0, addr)?;

        let mut connection = self.teamspeak.as_ref().unwrap().clone();
        let handle = tokio::runtime::Handle::current();
        self.player
            .setup_with_audio_callback(Some(Box::new(move |samples| {
                handle.block_on(connection.send_audio_packet(samples));
            })))
            .unwrap();

        Ok(())
    }
}

pub struct GetName;
impl Message for GetName {
    type Result = String;
}

#[async_trait]
impl Handler<GetName> for MusicBot {
    async fn handle(&mut self, _: GetName, _: &mut Context<Self>) -> String {
        self.name().to_owned()
    }
}

pub struct GetBotData;
impl Message for GetBotData {
    type Result = crate::web_server::BotData;
}

#[async_trait]
impl Handler<GetBotData> for MusicBot {
    async fn handle(&mut self, _: GetBotData, _: &mut Context<Self>) -> crate::web_server::BotData {
        crate::web_server::BotData {
            name: self.name.clone(),
            playlist: self.playlist.to_vec(),
            currently_playing: self.player.currently_playing(),
            position: self.player.position(),
            state: self.state(),
            volume: self.volume().await,
        }
    }
}

pub struct GetChannel;
impl Message for GetChannel {
    type Result = Option<ChannelId>;
}

#[async_trait]
impl Handler<GetChannel> for MusicBot {
    async fn handle(&mut self, _: GetChannel, _: &mut Context<Self>) -> Option<ChannelId> {
        self.current_channel().await
    }
}

#[async_trait]
impl Handler<Quit> for MusicBot {
    async fn handle(&mut self, q: Quit, _: &mut Context<Self>) -> Result<(), tsclientlib::Error> {
        self.quit(q.0, false).await
    }
}

#[async_trait]
impl Handler<MusicBotMessage> for MusicBot {
    async fn handle(
        &mut self,
        msg: MusicBotMessage,
        _: &mut Context<Self>,
    ) -> Result<(), AudioPlayerError> {
        self.on_message(msg).await
    }
}

fn spawn_stdin_reader(addr: WeakAddress<MusicBot>) {
    use tokio::io::AsyncBufReadExt;

    tokio::task::spawn(async move {
        let stdin = tokio::io::stdin();
        let reader = tokio::io::BufReader::new(stdin);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await.unwrap() {
            let message = MusicBotMessage::TextMessage(ChatMessage {
                target: MessageTarget::Channel,
                invoker: Invoker {
                    name: String::from("stdin"),
                    id: ClientId(0),
                    uid: None,
                },
                text: line,
            });

            addr.send(message).await.unwrap().unwrap();
        }
    });
}
