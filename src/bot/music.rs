use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::anyhow;
use askama::filters::urlencode;
use async_trait::async_trait;
use serde::Serialize;
use slog::{debug, error, info, trace, warn, Logger};
use structopt::StructOpt;
use tsclientlib::{data, ChannelId, ClientId, Connection, Identity, Invoker, MessageTarget};
use walkdir::WalkDir;
use xtra::{spawn::Tokio, Actor, Address, Context, Handler, Message, WeakAddress};

use crate::audio_player::AudioPlayer;
use crate::bot::{BotDisonnected, Connect, MasterBot, Quit};
use crate::command::Command;
use crate::command::VolumeChange;
use crate::playlist::Playlist;
use crate::teamspeak as ts;
use crate::youtube_dl::AudioMetadata;
use ts::TeamSpeakConnection;

static FILE_PREFIX: &str = "file://";

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
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> anyhow::Result<(), std::fmt::Error> {
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
    type Result = anyhow::Result<()>;
}

#[derive(Debug)]
pub enum AudioLocation {
    Url(String),
    YoutubeSearch(String),
    Path(PathBuf),
}

pub struct MusicBot {
    name: String,
    music_root: Option<PathBuf>,
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
    pub music_root: Option<PathBuf>,
    pub master: Option<WeakAddress<MasterBot>>,
    pub local: bool,
    pub address: String,
    pub identity: Identity,
    pub channel: String,
    pub verbose: u8,
    pub logger: Logger,
    pub volume: f64,
}

impl MusicBot {
    pub async fn spawn(args: MusicBotArgs) -> Address<Self> {
        let mut player = AudioPlayer::new(args.logger.clone()).unwrap();
        player
            .change_volume(VolumeChange::Absolute(args.volume))
            .unwrap();

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
            music_root: args.music_root,
            master: args.master,
            identity: args.identity.clone(),
            player,
            teamspeak,
            playlist,
            state: State::EndOfStream,
            logger: args.logger.clone(),
        };

        let bot_addr = bot.create(None).spawn(&mut Tokio::Global);

        if args.local {
            info!(args.logger, "Starting in local mode");
        } else {
            info!(
                args.logger,
                "Connecting";
                "name" => &args.name,
                "channel" => &args.channel,
                "address" => &args.address,
            );
        }

        let opt = Connection::build(args.address)
            .version(tsclientlib::Version::Linux_3_3_2)
            .name(format!("🎵 {}", args.name))
            .identity(args.identity)
            .log_commands(args.verbose >= 1)
            .log_packets(args.verbose >= 2)
            .log_udp_packets(args.verbose >= 3)
            .channel(args.channel);
        bot_addr.send(Connect(opt)).await.unwrap().unwrap();

        if args.local {
            debug!(args.logger, "Spawning stdin reader thread");
            spawn_stdin_reader(bot_addr.clone());
        }

        bot_addr
            .send(MusicBotMessage::StateChange(State::EndOfStream))
            .await
            .unwrap()
            .unwrap();

        bot_addr
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

    pub async fn current_channel(&mut self) -> anyhow::Result<Option<ChannelId>> {
        let ts = self.teamspeak.as_mut().expect("current_channel needs ts");

        ts.current_channel().await
    }

    async fn user_count(&mut self, channel: ChannelId) -> anyhow::Result<u32> {
        let ts = self.teamspeak.as_mut().expect("user_count needs ts");

        ts.user_count(channel).await
    }

    async fn send_message(&mut self, text: String) -> anyhow::Result<()> {
        debug!(self.logger, "Sending message to TeamSpeak"; "message" => &text);

        if let Some(ts) = &mut self.teamspeak {
            ts.send_message_to_channel(text).await?;
        }

        Ok(())
    }

    async fn set_nickname(&mut self, name: String) -> anyhow::Result<()> {
        info!(self.logger, "Setting TeamSpeak nickname"; "name" => &name);

        if let Some(ts) = &mut self.teamspeak {
            ts.set_nickname(name).await?;
        }

        Ok(())
    }

    async fn set_description(&mut self, desc: String) {
        info!(self.logger, "Setting TeamSpeak description"; "description" => &desc);

        if let Some(ts) = &mut self.teamspeak {
            ts.set_description(desc).await;
        }
    }

    async fn on_text(&mut self, message: ChatMessage) -> anyhow::Result<()> {
        let msg = message.text;
        if msg.starts_with('!') {
            let tokens = msg[1..].split_whitespace().collect::<Vec<_>>();

            match Command::from_iter_safe(&tokens) {
                Ok(args) => self.on_command(args, message.invoker).await?,
                Err(e) if e.kind == structopt::clap::ErrorKind::HelpDisplayed => {
                    self.send_message(format!("\n{}", e.message)).await?;
                }
                _ => (),
            }
        }

        Ok(())
    }

    async fn on_command(&mut self, command: Command, invoker: Invoker) -> anyhow::Result<()> {
        debug!(self.logger, "User command: {:?}", command);
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
                let url = url.join(" ").replace("[URL]", "").replace("[/URL]", "");

                let location = if url.starts_with(FILE_PREFIX) {
                    AudioLocation::Path(PathBuf::from(&url[FILE_PREFIX.len()..]))
                } else {
                    AudioLocation::Url(url)
                };

                if let Err(e) = self.add_audio(location, invoker.name).await {
                    self.send_message(format!("Failed to add: {}", e)).await?;
                }
            }
            Command::Search { query } => {
                let location = if let Some(path) = self.find_local_file(&query).await {
                    AudioLocation::Path(path)
                } else {
                    AudioLocation::YoutubeSearch(query.join(" "))
                };

                if let Err(e) = self.add_audio(location, invoker.name).await {
                    self.send_message(format!("Failed to add: {}", e)).await?;
                }
            }
            Command::Pause => {
                self.player.pause()?;
            }
            Command::Stop => {
                self.player.reset()?;
            }
            Command::Seek { amount } => match self.player.seek(amount) {
                Ok(time) => {
                    self.send_message(format!("New position: {}", ts::bold(&time)))
                        .await?;
                }
                Err(e) => {
                    warn!(self.logger, "Failed to seek"; "error" => %e);
                    self.send_message(String::from("Failed to seek")).await?;
                }
            },
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
                self.send_message(String::from("Cleared playlist")).await?;
                self.playlist.clear();
            }
            Command::Volume { volume } => {
                self.player.change_volume(volume)?;
                self.update_name(self.state()).await?;
            }
            Command::Leave => {
                self.quit(String::from("Leaving"), true).await?;
            }
        }

        Ok(())
    }

    pub async fn add_audio(&mut self, location: AudioLocation, user: String) -> anyhow::Result<()> {
        let metadata = match location {
            AudioLocation::Path(rel_path) => {
                if self.music_root.is_none() {
                    anyhow::bail!("music_root was not configured");
                }

                let path = self.music_root.as_ref().unwrap().join(rel_path);
                let path = match path.canonicalize() {
                    Ok(p) => p,
                    Err(e) => {
                        return Err(anyhow!("Invalid path: {}", e)).into();
                    }
                };

                // Make sure files outside of the root path can't be accessed
                if !path.starts_with(self.music_root.as_ref().unwrap()) || !path.is_file() {
                    return Err(anyhow!("Invalid path"));
                }

                match metadata_from_file(&path, &user) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(
                            self.logger,
                            "No metadata was found for {}: {}",
                            path.to_string_lossy(),
                            e
                        );

                        AudioMetadata {
                            // FIXME: Since we use a rust string non-utf8 file names
                            // will not work as expected
                            uri: format!("{}{}", FILE_PREFIX, path.to_string_lossy()),
                            webpage_url: None,
                            title: path.file_name().unwrap().to_string_lossy().to_string(),
                            thumbnail: None,
                            duration: None,
                            added_by: user,
                        }
                    }
                }
            }
            AudioLocation::YoutubeSearch(query) => {
                self.get_url_from_ytdl(format!("ytsearch:{}", query), user)
                    .await?
            }
            AudioLocation::Url(query) => self.get_url_from_ytdl(query, user).await?,
        };

        self.playlist.push(metadata.clone());

        if !self.player.is_started() {
            if let Some(request) = self.playlist.pop() {
                self.start_playing_audio(request).await?;
            }
        } else {
            let duration = if let Some(duration) = metadata.duration {
                format!(" ({})", ts::bold(&humantime::format_duration(duration)))
            } else {
                format!("")
            };

            let msg = if metadata.uri.starts_with(FILE_PREFIX) {
                format!(
                    "Added local file {}{} to playlist",
                    ts::underline(&metadata.title),
                    duration
                )
            } else {
                format!(
                    "Added {}{} to playlist",
                    ts::underline(&metadata.title),
                    duration
                )
            };

            if let Err(e) = self.send_message(msg).await {
                error!(self.logger, "Failed to send message: {}", e);
            }
        }

        Ok(())
    }

    async fn get_url_from_ytdl(
        &self,
        query: String,
        user: String,
    ) -> anyhow::Result<AudioMetadata> {
        match crate::youtube_dl::get_audio_download_from_url(query, &self.logger).await {
            Ok(mut metadata) => {
                metadata.added_by = user;
                info!(self.logger, "Found source"; "uri" => &metadata.uri);

                Ok(metadata)
            }
            Err(e) => {
                info!(self.logger, "Failed to find audio url"; "error" => &e);

                Err(anyhow!("Failed to find url: {}", e)).into()
            }
        }
    }

    async fn start_playing_audio(&mut self, metadata: AudioMetadata) -> anyhow::Result<()> {
        let duration = if let Some(duration) = metadata.duration {
            format!("({})", ts::bold(&humantime::format_duration(duration)))
        } else {
            format!("")
        };

        let msg = if metadata.uri.starts_with(FILE_PREFIX) {
            format!(
                "Playing local file {} {}",
                ts::underline(&metadata.title),
                duration
            )
        } else {
            format!("Playing {} {}", ts::underline(&metadata.title), duration)
        };

        self.send_message(msg).await?;
        self.set_description(format!("Currently playing '{}'", metadata.title))
            .await;
        self.player.reset().unwrap();
        self.player.set_metadata(metadata).unwrap();
        self.player.play().unwrap();

        Ok(())
    }

    async fn find_local_file(&self, query: &Vec<String>) -> Option<PathBuf> {
        let known_exts = [OsStr::new("mp3"), OsStr::new("flac")];

        if let Some(music_root) = &self.music_root {
            let mut largest = (None, 0);

            'outer: for entry in WalkDir::new(music_root) {
                if let Err(e) = entry {
                    warn!(self.logger, "Failed to access file system entry: {}", e);
                    continue;
                }
                let entry = entry.unwrap();

                if !entry.file_type().is_file()
                    || entry
                        .path()
                        .extension()
                        .map(|e| !known_exts.contains(&e))
                        .unwrap_or(true)
                {
                    continue;
                }

                let rel_path = entry
                    .path()
                    .strip_prefix(music_root)
                    .expect("WalkDir only walks music_dir");

                let path_str = match rel_path.to_str() {
                    Some(path) => path,
                    None => continue,
                };

                let mut score = 0;
                let lowered_path = path_str.to_lowercase();
                for word in query {
                    let found = lowered_path.match_indices(&word.to_lowercase()).count();
                    if found == 0 {
                        continue 'outer;
                    }

                    score += found;
                }

                if score > largest.1 {
                    trace!(self.logger, "Found better score {} for {}", score, path_str);
                    largest = (Some(rel_path.to_path_buf()), score);
                }
            }
            if let Some(path) = largest.0 {
                return Some(path);
            }
        }

        None
    }

    async fn update_name(&mut self, state: State) -> anyhow::Result<()> {
        let volume = (self.volume().await * 100.0).round();
        let name = match state {
            State::EndOfStream => format!("🎵 {} ({}%)", self.name, volume),
            _ => format!("🎵 {} - {} ({}%)", self.name, state, volume),
        };

        self.set_nickname(name).await
    }

    async fn on_message(&mut self, message: MusicBotMessage) -> anyhow::Result<()> {
        match message {
            MusicBotMessage::TextMessage(message) => {
                if MessageTarget::Channel == message.target {
                    self.on_text(message).await?;
                }
            }
            MusicBotMessage::ClientChannel {
                client,
                old_channel,
            } => {
                self.on_client_left_channel(client, old_channel).await?;
            }
            MusicBotMessage::ClientDisconnected { id, client } => {
                let old_channel = client.channel;
                self.on_client_left_channel(id, old_channel).await?;
            }
            MusicBotMessage::StateChange(state) => {
                self.on_state(state).await?;
            }
            _ => (),
        }

        Ok(())
    }

    async fn on_state(&mut self, new_state: State) -> anyhow::Result<()> {
        if self.state != new_state {
            match new_state {
                State::EndOfStream => {
                    self.player.reset()?;
                    let next_track = self.playlist.pop();
                    if let Some(request) = next_track {
                        info!(self.logger, "Advancing playlist");

                        self.start_playing_audio(request).await?;
                    } else {
                        self.update_name(new_state).await?;
                        self.set_description(String::new()).await;
                    }
                }
                State::Stopped => {
                    if self.state != State::EndOfStream {
                        self.update_name(new_state).await?;
                        self.set_description(String::new()).await;
                    }
                }
                _ => self.update_name(new_state).await?,
            }
        }

        if !(self.state == State::EndOfStream && new_state == State::Stopped) {
            self.state = new_state;
        }

        Ok(())
    }

    async fn on_client_left_channel(
        &mut self,
        id: ClientId,
        old_channel: ChannelId,
    ) -> anyhow::Result<()> {
        match self.teamspeak.as_mut().unwrap().my_id().await {
            Ok(my_id) if my_id != id => (),
            _ => return Ok(()),
        };

        let current_channel = self
            .current_channel()
            .await?
            .expect("Current channel is known");
        if old_channel == current_channel {
            let quit = match self.user_count(current_channel).await {
                Ok(count) if count <= 1 => Some(String::from("Channel is empty")),
                Err(e) => Some(format!("Error: {}", e)),
                Ok(_) => None,
            };

            if let Some(reason) = quit {
                self.quit(reason, true).await?;
            }
        }

        Ok(())
    }

    pub async fn quit(&mut self, reason: String, inform_master: bool) -> anyhow::Result<()> {
        // FIXME logs errors if the bot is playing something because it tries to
        // change its name and description
        self.player.reset().unwrap();

        if let Some(ts) = self.teamspeak.as_mut() {
            ts.disconnect(&reason).await?;
        }

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
    async fn handle(&mut self, opt: Connect, ctx: &mut Context<Self>) -> anyhow::Result<()> {
        let addr = ctx.address().unwrap().downgrade();
        if let Some(ts) = self.teamspeak.as_mut() {
            ts.connect_for_bot(opt.0, addr)?;
            let mut connection = ts.clone();
            let handle = tokio::runtime::Handle::current();
            self.player
                .setup_with_audio_callback(Some(Box::new(move |samples| {
                    handle
                        .block_on(connection.send_audio_packet(samples))
                        .unwrap();
                })))
                .unwrap();
        }

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
    type Result = anyhow::Result<Option<ChannelId>>;
}

#[async_trait]
impl Handler<GetChannel> for MusicBot {
    async fn handle(
        &mut self,
        _: GetChannel,
        _: &mut Context<Self>,
    ) -> anyhow::Result<Option<ChannelId>> {
        self.current_channel().await
    }
}

#[async_trait]
impl Handler<Quit> for MusicBot {
    async fn handle(&mut self, q: Quit, _: &mut Context<Self>) -> anyhow::Result<()> {
        self.quit(q.0, false).await
    }
}

#[async_trait]
impl Handler<MusicBotMessage> for MusicBot {
    async fn handle(&mut self, msg: MusicBotMessage, _: &mut Context<Self>) -> anyhow::Result<()> {
        self.on_message(msg).await?;

        Ok(())
    }
}

fn metadata_from_file(path: &Path, user: &str) -> Result<AudioMetadata, anyhow::Error> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("mp3") => {
            let tag = id3::Tag::read_from_path(path)?;
            let title = match (tag.title(), tag.artist()) {
                (Some(title), Some(artist)) => format!("{} - {}", title, artist),
                (Some(title), _) => title.to_owned(),
                (_, _) => path.file_name().unwrap().to_string_lossy().to_string(),
            };

            let mut cover = None;
            for picture in tag.pictures() {
                if picture.picture_type == id3::frame::PictureType::CoverFront {
                    // The image type might be wrong but it does not seem like the big browsers
                    // care so finding the correct type does not seem like it is worth the effort.
                    cover = Some(format!(
                        "data:image/jpg;base64,{}",
                        base64::encode(&picture.data)
                    ));
                }
            }

            return Ok(AudioMetadata {
                uri: format!(
                    "{}{}",
                    FILE_PREFIX,
                    urlencode(&path.to_string_lossy()).expect("it cant fail")
                ),
                webpage_url: None,
                title,
                thumbnail: cover,
                duration: tag.duration().map(|s| Duration::from_millis(s as u64)),
                added_by: user.to_owned(),
            });
        }
        Some("flac") => {
            let tag = metaflac::Tag::read_from_path(path)?;
            let comments = &tag
                .vorbis_comments()
                .ok_or_else(|| anyhow!("no vorbis comments found"))?;
            let title = match (comments.title(), comments.artist()) {
                (Some(title), Some(artist)) => {
                    format!("{} - {}", title.join(";"), artist.join(";"))
                }
                (Some(title), _) => title.join(";"),
                (_, _) => path.file_name().unwrap().to_string_lossy().to_string(),
            };

            let mut cover = None;
            for picture in tag.pictures() {
                if picture.picture_type == metaflac::block::PictureType::CoverFront {
                    cover = Some(format!(
                        "data:image/jpg;base64,{}",
                        base64::encode(&picture.data)
                    ));
                }
            }

            return Ok(AudioMetadata {
                uri: format!(
                    "{}{}",
                    FILE_PREFIX,
                    urlencode(&path.to_string_lossy()).expect("it cant fail")
                ),
                webpage_url: None,
                title,
                thumbnail: cover,
                duration: None,
                added_by: user.to_owned(),
            });
        }
        _ => (),
    }

    Err(anyhow!(
        "file does not contain metadata or filetype is unknown"
    ))
}

fn spawn_stdin_reader(addr: Address<MusicBot>) {
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
