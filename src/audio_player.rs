use std::sync::Once;
use std::time::Duration;

use gst::prelude::*;
use gst::GhostPad;
use gstreamer as gst;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_audio::{StreamVolume, StreamVolumeFormat};

use crate::bot::{MusicBotMessage, State};
use glib::BoolError;
use log::{debug, error, info, warn};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc::UnboundedSender;

use crate::command::{Seek, VolumeChange};
use crate::youtube_dl::AudioMetadata;

static GST_INIT: Once = Once::new();

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum PollResult {
    Continue,
    Quit,
}

pub struct AudioPlayer {
    pipeline: gst::Pipeline,
    bus: gst::Bus,
    http_src: gst::Element,

    volume_f64: RwLock<f64>,
    volume: gst::Element,
    sender: Arc<RwLock<UnboundedSender<MusicBotMessage>>>,
    currently_playing: RwLock<Option<AudioMetadata>>,
}

fn make_element(factoryname: &str, display_name: &str) -> Result<gst::Element, AudioPlayerError> {
    Ok(gst::ElementFactory::make(factoryname, Some(display_name))?)
}

fn link_elements(a: &gst::Element, b: &gst::Element) -> Result<(), AudioPlayerError> {
    a.link(b)?;

    Ok(())
}

fn add_decode_bin_new_pad_callback(
    decode_bin: &gst::Element,
    audio_bin: gst::Bin,
    ghost_pad: gst::GhostPad,
) {
    decode_bin.connect_pad_added(move |_, new_pad| {
        debug!("New pad received on decode bin");
        let name = if let Some(caps) = new_pad.get_current_caps() {
            debug!("Pad caps: {}", caps.to_string());
            if let Some(structure) = caps.get_structure(0) {
                Some(structure.get_name().to_string())
            } else {
                None
            }
        } else {
            None
        };

        if let Some("audio/x-raw") = name.as_deref() {
            if let Some(peer) = ghost_pad.get_peer() {
                peer.unlink(&ghost_pad).unwrap();
            }

            info!("Found raw audio, linking audio bin");
            new_pad.link(&ghost_pad).unwrap();

            audio_bin.sync_state_with_parent().unwrap();
        }
    });
}

impl AudioPlayer {
    pub fn new(
        sender: Arc<RwLock<UnboundedSender<MusicBotMessage>>>,
        callback: Option<Box<dyn FnMut(&[u8]) + Send>>,
    ) -> Result<Self, AudioPlayerError> {
        GST_INIT.call_once(|| gst::init().unwrap());

        info!("Creating audio player");

        let pipeline = gst::Pipeline::new(Some("TeamSpeak Audio Player"));
        let bus = pipeline.get_bus().unwrap();
        let http_src = make_element("souphttpsrc", "http source")?;
        let decode_bin = make_element("decodebin", "decode bin")?;
        pipeline.add_many(&[&http_src, &decode_bin])?;

        link_elements(&http_src, &decode_bin)?;

        let (audio_bin, volume, ghost_pad) = Self::create_audio_bin(callback)?;

        add_decode_bin_new_pad_callback(&decode_bin, audio_bin.clone(), ghost_pad);

        pipeline.add(&audio_bin)?;

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
            http_src,

            volume_f64: RwLock::new(0.0),
            volume,
            sender,
            currently_playing: RwLock::new(None),
        })
    }

    fn create_audio_bin(
        callback: Option<Box<dyn FnMut(&[u8]) + Send>>,
    ) -> Result<(gst::Bin, gst::Element, gst::GhostPad), AudioPlayerError> {
        let audio_bin = gst::Bin::new(Some("audio bin"));
        let queue = make_element("queue", "audio queue")?;
        let convert = make_element("audioconvert", "audio converter")?;
        let volume = make_element("volume", "volume")?;
        let resample = make_element("audioresample", "audio resampler")?;
        let pads = queue.get_sink_pads();
        let queue_sink_pad = pads.first().unwrap();

        audio_bin.add_many(&[&queue, &convert, &volume, &resample])?;

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
                    let buffer = sample.get_buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let samples = map.as_slice();

                    callback(samples);

                    Ok(gst::FlowSuccess::Ok)
                })
                .build();
            appsink.set_callbacks(callbacks);

            audio_bin.add_many(&[&opus_enc, &sink])?;

            gst::Element::link_many(&[&queue, &convert, &volume, &resample, &opus_enc, &sink])?;
        } else {
            let sink = make_element("autoaudiosink", "auto audio sink")?;

            audio_bin.add_many(&[&sink])?;

            gst::Element::link_many(&[&queue, &convert, &volume, &resample, &sink])?;
        };

        let ghost_pad = GhostPad::with_target(Some("audio bin sink"), queue_sink_pad).unwrap();
        ghost_pad.set_active(true)?;
        audio_bin.add_pad(&ghost_pad)?;

        Ok((audio_bin, volume, ghost_pad))
    }

    pub fn set_metadata(&self, data: AudioMetadata) -> Result<(), AudioPlayerError> {
        self.set_source_url(data.url.clone())?;

        let mut currently_playing = self.currently_playing.write().unwrap();
        *currently_playing = Some(data);

        Ok(())
    }

    fn set_source_url(&self, location: String) -> Result<(), AudioPlayerError> {
        info!("Setting location URI: {}", location);
        self.http_src.set_property("location", &location)?;

        Ok(())
    }

    pub fn change_volume(&self, volume: VolumeChange) -> Result<(), AudioPlayerError> {
        let new_volume = match volume {
            VolumeChange::Positive(vol) => self.volume() + vol,
            VolumeChange::Negative(vol) => self.volume() - vol,
            VolumeChange::Absolute(vol) => vol,
        };
        let new_volume = new_volume.max(0.0).min(1.0);

        *self.volume_f64.write().unwrap() = new_volume;
        let db = 50.0 * new_volume.log10();
        info!("Setting volume: {} -> {} dB", new_volume, db);

        let linear =
            StreamVolume::convert_volume(StreamVolumeFormat::Db, StreamVolumeFormat::Linear, db);

        self.volume.set_property("volume", &linear)?;

        Ok(())
    }

    pub fn is_started(&self) -> bool {
        let (_, current, pending) = self.pipeline.get_state(gst::ClockTime(None));

        match (current, pending) {
            (gst::State::Null, gst::State::VoidPending) => false,
            (_, gst::State::Null) => false,
            (gst::State::Ready, gst::State::VoidPending) => false,
            _ => true,
        }
    }

    pub fn volume(&self) -> f64 {
        *self.volume_f64.read().unwrap()
    }

    pub fn position(&self) -> Option<Duration> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .and_then(|t| t.0.map(Duration::from_nanos))
    }

    pub fn currently_playing(&self) -> Option<AudioMetadata> {
        self.currently_playing.read().unwrap().clone()
    }

    pub fn reset(&self) -> Result<(), AudioPlayerError> {
        info!("Setting pipeline state to null");

        let mut currently_playing = self.currently_playing.write().unwrap();
        *currently_playing = None;

        self.pipeline.set_state(gst::State::Null)?;

        Ok(())
    }

    pub fn play(&self) -> Result<(), AudioPlayerError> {
        info!("Setting pipeline state to playing");

        self.pipeline.set_state(gst::State::Playing)?;

        Ok(())
    }

    pub fn pause(&self) -> Result<(), AudioPlayerError> {
        info!("Setting pipeline state to paused");

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
                Duration::from_nanos(pos.nanoseconds().ok_or(AudioPlayerError::SeekError)?)
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
        info!("Seeking to {}", time);

        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH,
            gst::ClockTime::from_nseconds(absolute.as_nanos() as _),
        )?;

        Ok(time)
    }

    pub fn stop_current(&self) -> Result<(), AudioPlayerError> {
        info!("Stopping pipeline, sending EOS");

        self.bus.post(&gst::message::Eos::new())?;

        Ok(())
    }

    pub fn quit(&self, reason: String) {
        info!("Quitting audio player");

        if self
            .bus
            .post(&gst::message::Application::new(gst::Structure::new_empty(
                "quit",
            )))
            .is_err()
        {
            warn!("Tried to send \"quit\" app event on flushing bus.");
        }

        let sender = self.sender.read().unwrap();
        sender.send(MusicBotMessage::Quit(reason)).unwrap();
    }

    fn send_state(&self, state: State) {
        info!("Sending state {:?} to application", state);
        let sender = self.sender.read().unwrap();
        sender.send(MusicBotMessage::StateChange(state)).unwrap();
    }

    pub fn poll(&self) -> PollResult {
        debug!("Polling GStreamer");
        'outer: loop {
            while let Some(msg) = self.bus.timed_pop(gst::ClockTime(None)) {
                use gst::MessageView;

                match msg.view() {
                    MessageView::StateChanged(state) => {
                        if let Some(src) = state.get_src() {
                            if src.get_name() != self.pipeline.get_name() {
                                continue;
                            }
                        }

                        let old = state.get_old();
                        let current = state.get_current();
                        let pending = state.get_pending();

                        match (old, current, pending) {
                            (gst::State::Paused, gst::State::Playing, gst::State::VoidPending) => {
                                self.send_state(State::Playing)
                            }
                            (gst::State::Playing, gst::State::Paused, gst::State::VoidPending) => {
                                self.send_state(State::Paused)
                            }
                            (_, gst::State::Ready, gst::State::Null) => {
                                self.send_state(State::Stopped)
                            }
                            (_, gst::State::Null, gst::State::VoidPending) => {
                                self.send_state(State::Stopped)
                            }
                            _ => {
                                debug!(
                                    "Pipeline transitioned from {:?} to {:?}, with {:?} pending",
                                    old, current, pending
                                );
                            }
                        }
                    }
                    MessageView::Eos(..) => {
                        info!("End of stream reached");
                        self.reset().unwrap();

                        break 'outer;
                    }
                    MessageView::Warning(warn) => {
                        warn!(
                            "Warning from {:?}: {} ({:?})",
                            warn.get_src().map(|s| s.get_path_string()),
                            warn.get_error(),
                            warn.get_debug()
                        );
                        break 'outer;
                    }
                    MessageView::Error(err) => {
                        error!(
                            "Error from {:?}: {} ({:?})",
                            err.get_src().map(|s| s.get_path_string()),
                            err.get_error(),
                            err.get_debug()
                        );
                        break 'outer;
                    }
                    MessageView::Application(content) => {
                        if let Some(s) = content.get_structure() {
                            if s.get_name() == "quit" {
                                self.reset().unwrap();
                                return PollResult::Quit;
                            }
                        }
                    }
                    _ => {
                        //debug!("Unhandled message on bus: {:?}", msg)
                    }
                };
            }
        }
        debug!("Left GStreamer message loop");

        PollResult::Continue
    }
}

#[derive(Debug)]
pub enum AudioPlayerError {
    GStreamerError(glib::error::BoolError),
    StateChangeFailed,
    SeekError,
}

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
