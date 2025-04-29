use std::sync::Once;
use std::time::Duration;

use gst::prelude::*;
use gst::GhostPad;
use gstreamer as gst;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_audio::{StreamVolume, StreamVolumeFormat};

use glib::BoolError;
use slog::{debug, error, info, warn, Logger};
use xtra::WeakAddress;

use crate::bot::{MusicBot, MusicBotMessage, State};
use crate::command::{Seek, VolumeChange};
use crate::youtube_dl::AudioMetadata;

static GST_INIT: Once = Once::new();

pub struct AudioPlayer {
    pipeline: gst::Pipeline,
    bus: gst::Bus,
    uri_src: gst::Element,

    volume_f64: f64,
    volume: gst::Element,
    currently_playing: Option<AudioMetadata>,

    logger: Logger,
}

fn make_element(factoryname: &str, display_name: &str) -> Result<gst::Element, AudioPlayerError> {
    gst::ElementFactory::make(factoryname, Some(display_name))
        .map_err(|_| AudioPlayerError::MissingPlugin(factoryname.to_string()))
}

fn add_uri_src_new_pad_callback(
    uri_src: &gst::Element,
    audio_bin: gst::Bin,
    ghost_pad: gst::GhostPad,
    logger: Logger,
) {
    uri_src.connect_pad_added(move |_, new_pad| {
        debug!(logger, "New pad received on decode bin");
        let name = if let Some(caps) = new_pad.current_caps() {
            debug!(logger, "Found caps"; "caps" => caps.to_string());
            caps.structure(0)
                .map(|structure| structure.name().to_string())
        } else {
            None
        };

        if let Some("audio/x-raw") = name.as_deref() {
            if let Some(peer) = ghost_pad.peer() {
                peer.unlink(&ghost_pad).unwrap();
            }

            info!(logger, "Found raw audio, linking audio bin");
            new_pad.link(&ghost_pad).unwrap();

            audio_bin.sync_state_with_parent().unwrap();
        }
    });
}

type AudioCallback = dyn FnMut(&[u8]) + Send;
impl AudioPlayer {
    pub fn new(logger: Logger) -> Result<Self, AudioPlayerError> {
        GST_INIT.call_once(|| gst::init().unwrap());

        info!(logger, "Creating audio player");

        let pipeline = gst::Pipeline::new(Some("TeamSpeak Audio Player"));
        let bus = pipeline.bus().unwrap();
        let uri_src = make_element("uridecodebin", "uri source")?;
        let volume = make_element("volume", "volume")?;

        // The documentation says that we have to make sure to handle
        // all messages if auto flushing is deactivated.
        // I hope our way of reading messages is good enough.
        //
        // https://gstreamer.freedesktop.org/documentation/gstreamer/gstpipeline.html#gst_pipeline_set_auto_flush_bus
        pipeline.set_auto_flush_bus(false);
        pipeline.set_state(gst::State::Ready)?;

        Ok(AudioPlayer {
            pipeline,
            bus,
            uri_src,
            logger,
            volume_f64: 0.0,
            volume,
            currently_playing: None,
        })
    }

    pub fn setup_with_audio_callback(
        &self,
        callback: Option<Box<AudioCallback>>,
    ) -> Result<(), AudioPlayerError> {
        self.pipeline.add(&self.uri_src)?;

        let audio_bin = gst::Bin::new(Some("audio bin"));
        let queue = make_element("queue", "audio queue")?;
        let convert = make_element("audioconvert", "audio converter")?;
        let resample = make_element("audioresample", "audio resampler")?;
        let pads = queue.sink_pads();
        let queue_sink_pad = pads.first().unwrap();

        audio_bin.add_many(&[&queue, &convert, &self.volume, &resample])?;

        if let Some(mut callback) = callback {
            let opus_enc = make_element("opusenc", "opus encoder")?;
            let sink = make_element("appsink", "app sink")?;

            let appsink = sink
                .clone()
                .dynamic_cast::<AppSink>()
                .expect("Sink element is expected to be an appsink!");
            appsink.set_caps(Some(&gst::Caps::new_simple(
                "audio/x-opus",
                &[("channels", &(2i32)), ("rate", &(48_000i32))],
            )));
            let callbacks = AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let samples = map.as_slice();

                    callback(samples);

                    Ok(gst::FlowSuccess::Ok)
                })
                .build();
            appsink.set_callbacks(callbacks);

            audio_bin.add_many(&[&opus_enc, &sink])?;

            gst::Element::link_many(&[
                &queue,
                &convert,
                &self.volume,
                &resample,
                &opus_enc,
                &sink,
            ])?;
        } else {
            let sink = make_element("autoaudiosink", "auto audio sink")?;

            audio_bin.add(&sink)?;

            gst::Element::link_many(&[&queue, &convert, &self.volume, &resample, &sink])?;
        };

        let ghost_pad = GhostPad::with_target(Some("audio bin sink"), queue_sink_pad).unwrap();
        ghost_pad.set_active(true)?;
        audio_bin.add_pad(&ghost_pad)?;

        add_uri_src_new_pad_callback(
            &self.uri_src,
            audio_bin.clone(),
            ghost_pad,
            self.logger.clone(),
        );

        self.pipeline.add(&audio_bin)?;

        Ok(())
    }

    pub fn set_metadata(&mut self, data: AudioMetadata) -> Result<(), AudioPlayerError> {
        self.set_source_uri(data.uri.clone())?;
        self.currently_playing = Some(data);

        Ok(())
    }

    fn set_source_uri(&self, location: String) -> Result<(), AudioPlayerError> {
        info!(self.logger, "Setting source"; "uri" => &location);
        self.uri_src.set_property("uri", &location)?;

        Ok(())
    }

    pub fn change_volume(&mut self, volume: VolumeChange) -> Result<(), AudioPlayerError> {
        let new_volume = match volume {
            VolumeChange::Positive(vol) => self.volume_f64 + vol,
            VolumeChange::Negative(vol) => self.volume_f64 - vol,
            VolumeChange::Absolute(vol) => vol,
        };
        let new_volume = new_volume.clamp(0.0, 1.0);

        self.volume_f64 = new_volume;
        let db = 50.0 * new_volume.log10();
        info!(self.logger, "Setting volume"; "volume" => new_volume, "db" => db);

        let linear =
            StreamVolume::convert_volume(StreamVolumeFormat::Db, StreamVolumeFormat::Linear, db);

        self.volume.set_property("volume", linear)?;

        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), AudioPlayerError> {
        info!(self.logger, "Setting pipeline state"; "to" => "null");

        self.currently_playing = None;

        self.pipeline.set_state(gst::State::Null)?;

        Ok(())
    }

    pub fn play(&self) -> Result<(), AudioPlayerError> {
        info!(self.logger, "Setting pipeline state"; "to" => "playing");

        self.pipeline.set_state(gst::State::Playing)?;

        Ok(())
    }

    pub fn pause(&self) -> Result<(), AudioPlayerError> {
        info!(self.logger, "Setting pipeline state"; "to" => "paused");

        self.pipeline.set_state(gst::State::Paused)?;

        Ok(())
    }

    pub fn seek(&self, seek: Seek) -> Result<humantime::FormattedDuration, AudioPlayerError> {
        let base = match seek {
            Seek::Positive(_) | Seek::Negative(_) => {
                let pos = self
                    .pipeline
                    .query_position::<gst::ClockTime>()
                    .ok_or(AudioPlayerError::SeekError)?;

                Duration::from_nanos(pos.nseconds())
            }
            _ => Duration::new(0, 0),
        };

        let absolute = match seek {
            Seek::Positive(duration) => base + duration,
            Seek::Negative(duration) => {
                if duration > base {
                    Duration::new(0, 0)
                } else {
                    base - duration
                }
            }
            Seek::Absolute(duration) => duration,
        };

        let time = humantime::format_duration(absolute);
        info!(self.logger, "Seeking"; "time" => %time);

        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH,
            gst::ClockTime::from_nseconds(absolute.as_nanos() as _),
        )?;

        Ok(time)
    }

    pub fn stop_current(&self) -> Result<(), AudioPlayerError> {
        info!(self.logger, "Stopping pipeline, sending EOS");

        self.bus.post(&gst::message::Eos::new())?;

        Ok(())
    }

    pub fn is_started(&self) -> bool {
        let (_, current, pending) = self.pipeline.state(gst::ClockTime::NONE);

        !matches!(
            (current, pending),
            (gst::State::Null, gst::State::VoidPending)
                | (_, gst::State::Null)
                | (gst::State::Ready, gst::State::VoidPending)
        )
    }

    pub fn volume(&self) -> f64 {
        self.volume_f64
    }

    pub fn position(&self) -> Option<Duration> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    pub fn currently_playing(&self) -> Option<AudioMetadata> {
        self.currently_playing.clone()
    }

    pub fn register_bot(&self, bot: WeakAddress<MusicBot>) {
        let pipeline_name = self.pipeline.name();
        debug!(self.logger, "Setting sync handler on gstreamer bus");

        let logger = self.logger.clone();
        let handle = tokio::runtime::Handle::current();
        self.bus.set_sync_handler(move |_, msg| {
            use gst::MessageView;

            match msg.view() {
                MessageView::StateChanged(state) => {
                    if let Some(src) = state.src() {
                        if src.name() != pipeline_name {
                            return gst::BusSyncReply::Drop;
                        }
                    }

                    let old = state.old();
                    let current = state.current();
                    let pending = state.pending();

                    match (old, current, pending) {
                        (gst::State::Paused, gst::State::Playing, gst::State::VoidPending) => {
                            send_state(&handle, &bot, State::Playing);
                        }
                        (gst::State::Playing, gst::State::Paused, gst::State::VoidPending) => {
                            send_state(&handle, &bot, State::Paused);
                        }
                        (_, gst::State::Ready, gst::State::Null) => {
                            send_state(&handle, &bot, State::Stopped);
                        }
                        (_, gst::State::Null, gst::State::VoidPending) => {
                            send_state(&handle, &bot, State::Stopped);
                        }
                        _ => {
                            debug!(
                                logger,
                                "Pipeline transitioned";
                                "from" => ?old,
                                "to" => ?current,
                                "pending" => ?pending
                            );
                        }
                    }
                }
                MessageView::Eos(..) => {
                    info!(logger, "End of stream reached");

                    send_state(&handle, &bot, State::EndOfStream);
                }
                MessageView::Warning(warn) => {
                    warn!(
                        logger,
                        "Received warning from bus";
                        "source" => warn.src().map(|s| s.path_string().as_str().to_owned()),
                        "error" => %warn.error(),
                        "debug" => warn.debug()
                    );
                }
                MessageView::Error(err) => {
                    error!(
                        logger,
                        "Received error from bus";
                        "source" => err.src().map(|s| s.path_string().as_str().to_owned()),
                        "error" => %err.error(),
                        "debug" => err.debug()
                    );

                    send_state(&handle, &bot, State::EndOfStream);
                }
                _ => {
                    //debug!("Unhandled message on bus: {:?}", msg)
                }
            }

            gst::BusSyncReply::Drop
        });
    }
}

fn send_state(handle: &tokio::runtime::Handle, addr: &WeakAddress<MusicBot>, state: State) {
    handle.spawn(addr.send(MusicBotMessage::StateChange(state)));
}

#[derive(Debug)]
pub enum AudioPlayerError {
    MissingPlugin(String),
    GStreamerError(glib::error::BoolError),
    StateChangeFailed,
    SeekError,
}

impl std::fmt::Display for AudioPlayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use AudioPlayerError::*;
        match self {
            MissingPlugin(name) => write!(f, "The '{}' GStreamer plugin was not found", name),
            GStreamerError(e) => write!(f, "{}", e),
            StateChangeFailed => write!(f, "AudioPlayer failed to change state"),
            SeekError => write!(f, "AudioPlayer failed to seek"),
        }
    }
}

impl std::error::Error for AudioPlayerError {}

impl From<glib::error::BoolError> for AudioPlayerError {
    fn from(err: BoolError) -> Self {
        AudioPlayerError::GStreamerError(err)
    }
}

impl From<gst::StateChangeError> for AudioPlayerError {
    fn from(_err: gst::StateChangeError) -> Self {
        AudioPlayerError::StateChangeFailed
    }
}
