use std::process::Command;
use std::sync::{Arc, Mutex};

use futures::compat::Future01CompatExt;
use futures01::{future::Future, sink::Sink};
use futures_util::stream::StreamExt;
use tokio_process::CommandExt;

use tsclientlib::{Connection, DisconnectOptions, MessageTarget};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

use byte_slice_cast::*;

use crate::playlist::{AudioRequest, Playlist};

#[derive(Clone)]
pub struct State {
    conn: Arc<Connection>,
    pipeline: Arc<gst::Pipeline>,
    playlist: Arc<Mutex<Playlist>>,
}

impl State {
    pub fn new(conn: Connection) -> Self {
        let conn_arc = Arc::new(conn);

        gst::init().unwrap();
        let pipeline = gst::Pipeline::new(Some("Ts Audio Player"));
        let http_dl = gst::ElementFactory::make("souphttpsrc", Some("http-source")).unwrap();
        let decoder = gst::ElementFactory::make("decodebin", Some("video-decoder")).unwrap();
        pipeline.add_many(&[&http_dl, &decoder]).unwrap();

        http_dl
            .link(&decoder)
            .expect("Can link https_dl to decoder");

        let pipeline_weak = pipeline.downgrade();

        let inner_conn = conn_arc.clone();
        decoder.connect_pad_added(move |_, decoder_src| {
            let pipeline = match pipeline_weak.upgrade() {
                Some(pipeline) => pipeline,
                None => return,
            };

            let is_audio = {
                let media_type = decoder_src.get_current_caps().and_then(|caps| {
                    caps.get_structure(0).map(|s| {
                        let name = s.get_name();
                        name.starts_with("audio/")
                    })
                });

                match media_type {
                    None => {
                        eprintln!(
                            "Failed to get media type from pad {}",
                            decoder_src.get_name()
                        );
                        return;
                    }
                    Some(media_type) => media_type,
                }
            };

            if is_audio {
                let prev_volume = if let Some(volume) = pipeline.get_by_name("volume") {
                    volume
                        .get_property("volume")
                        .expect("Can get volume property")
                        .get_some::<f64>()
                        .unwrap_or(0.1)
                } else {
                    0.1
                };

                let names = [
                    "audio-converter",
                    "volume",
                    "audio-resampler",
                    "opus-encoder",
                    "app-sink",
                ];
                for element in pipeline.iterate_elements() {
                    if let Ok(element) = element {
                        if names.contains(&&*element.get_name()) {
                            element.set_state(gst::State::Null).unwrap();
                            pipeline
                                .remove_many(&[&element])
                                .expect("Can remove element");
                        }
                    }
                }

                let convert =
                    gst::ElementFactory::make("audioconvert", Some("audio-converter")).unwrap();
                let volume = gst::ElementFactory::make("volume", Some("volume")).unwrap();
                let resample =
                    gst::ElementFactory::make("audioresample", Some("audio-resampler")).unwrap();
                let opus_enc = gst::ElementFactory::make("opusenc", Some("opus-encoder")).unwrap();
                let sink = gst::ElementFactory::make("appsink", Some("app-sink")).unwrap();

                sink.set_property("async", &false)
                    .expect("Can make app-sink async");

                volume
                    .set_property("volume", &prev_volume)
                    .expect("Can change volume");

                {
                    let elements = &[&convert, &volume, &resample, &opus_enc, &sink];
                    pipeline.add_many(elements).expect("Can add audio elements");
                    gst::Element::link_many(elements).expect("Can link audio elements");

                    for e in elements {
                        e.sync_state_with_parent()
                            .expect("Can sync state with parent");
                    }
                }

                let appsink = sink
                    .dynamic_cast::<gst_app::AppSink>()
                    .expect("Sink is an Appsink");

                appsink.set_caps(Some(&gst::Caps::new_simple("audio/x-opus", &[])));

                let inner_conn = inner_conn.clone();
                appsink.set_callbacks(
                    gst_app::AppSinkCallbacks::new()
                        // Add a handler to the "new-sample" signal.
                        .new_sample(move |appsink| {
                            // Pull the sample in question out of the appsink's buffer.
                            let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            let buffer = sample
                                .get_buffer()
                                .expect("Failed to get buffer from appsink");

                            let map = buffer
                                .map_readable()
                                .expect("Failed to map buffer readable");

                            let samples = map
                                .as_slice_of::<u8>()
                                .expect("Failed to interprete buffer as S16 PCM");

                            let packet = tsproto_packets::packets::OutAudio::new(
                                &tsproto_packets::packets::AudioData::C2S {
                                    id: 0,
                                    codec: tsproto_packets::packets::CodecType::OpusMusic,
                                    data: &samples,
                                },
                            );

                            let send_packet = inner_conn
                                .get_packet_sink()
                                .send(packet)
                                .map(|_| ())
                                .map_err(|e| println!("Failed to send voice packet: {}", e));

                            tokio::run(send_packet);

                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );

                let convert_sink = convert
                    .get_static_pad("sink")
                    .expect("queue has no sinkpad");
                decoder_src
                    .link(&convert_sink)
                    .expect("Can link decoder src to convert sink");
            }
        });

        Self {
            conn: conn_arc,
            pipeline: Arc::new(pipeline),
            playlist: Arc::new(Mutex::new(Playlist::new())),
        }
    }

    pub async fn add_audio(&self, url: String) {
        if self
            .playlist
            .lock()
            .expect("Mutex was not poisoned")
            .is_full()
        {
            self.send_message(MessageTarget::Channel, "Playlist is full");
            return;
        }

        let ytdl_args = [
            "--no-playlist",
            "-f",
            "bestaudio/best",
            "-g",
            "--get-filename",
            "-o",
            "%(title)s",
            &url,
        ];

        let output = Command::new("youtube-dl")
            .args(&ytdl_args)
            .output_async()
            .compat()
            .await
            .expect("youtube-dl is runnable");

        if output.status.success() == false {
            self.set_name("PokeBot");
            self.send_message(MessageTarget::Channel, "Failed to load url");
            return;
        }

        let output_string = String::from_utf8(output.stdout).unwrap();
        let output_lines = output_string.lines().collect::<Vec<_>>();
        let address = output_lines[0];
        let title = output_lines[1];

        let req = AudioRequest {
            title: title.to_owned(),
            address: address.to_owned(),
        };

        if gst::State::Null == self.pipeline.get_state(gst::ClockTime(None)).1 {
            self.set_name("PokeBot - Playing");
            self.start_audio(req);
        } else {
            self.set_name("PokeBot - Playing");

            let title = req.title.clone();
            if self
                .playlist
                .lock()
                .expect("Mutex was not poisoned")
                .push(req)
                == false
            {
                self.send_message(MessageTarget::Channel, "Playlist is full");
            } else {
                self.send_message(
                    MessageTarget::Channel,
                    &format!("Added '{}' to the playlist", title),
                );
            }
        }
    }

    pub async fn poll(&self) {
        let bus = self
            .pipeline
            .get_bus()
            .expect("Pipeline without bus. Shouldn't happen!");

        let mut messages = gst::BusStream::new(&bus);
        while let Some(msg) = messages.next().await {
            use gst::MessageView;

            match msg.view() {
                MessageView::Eos(..) => break,
                MessageView::Error(err) => {
                    println!(
                        "Error from {:?}: {} ({:?})",
                        err.get_src().map(|s| s.get_path_string()),
                        err.get_error(),
                        err.get_debug()
                    );
                    break;
                }
                _ => (),
            };
        }

        self.next();
    }

    pub fn start_audio(&self, req: AudioRequest) {
        self.pipeline
            .set_state(gst::State::Null)
            .expect("Can set state to null");

        let http_src = self
            .pipeline
            .get_by_name("http-source")
            .expect("Http source should be registered");

        http_src
            .set_property("location", &&req.address)
            .expect("Can set location on http_dl");

        self.pipeline
            .set_state(gst::State::Playing)
            .expect("Can set state to playing");

        self.set_description(&format!("Currently playing: {}", req.title));
    }

    pub fn volume(&self, volume: f64) {
        if let Some(vol_filter) = self.pipeline.get_by_name("volume") {
            vol_filter
                .set_property("volume", &(10.0f64.powf(volume / 50.0 - 2.0)))
                .expect("can change volume");

            // TODO Reflect volume in name
        }
    }

    pub fn play(&self) {
        let http_src = self
            .pipeline
            .get_by_name("http-source")
            .expect("Http source should be registered");

        if http_src
            .get_property("location")
            .ok()
            .and_then(|v| v.get::<String>().ok().and_then(|s| s.map(|s| s.is_empty())))
            .unwrap_or(true)
        {
            if self
                .playlist
                .lock()
                .expect("Mutex was not poisoned")
                .is_empty()
            {
                self.send_message(MessageTarget::Channel, "There is nothing to play");
                return;
            }
        }

        self.pipeline
            .set_state(gst::State::Playing)
            .expect("can play");

        self.set_name("PokeBot - Playing");
    }

    pub fn next(&self) {
        if let Some(req) = self.playlist.lock().expect("Mutex was not poisoned").pop() {
            self.start_audio(req);
        } else {
            self.pipeline
                .set_state(gst::State::Null)
                .expect("Can set state to null");

            let http_src = self
                .pipeline
                .get_by_name("http-source")
                .expect("Http source should be registered");

            http_src
                .set_property("location", &"")
                .expect("Can set location on http_dl");

            self.set_name("PokeBot");
        }
    }

    pub fn clear(&self) {
        self.playlist
            .lock()
            .expect("Mutex was not poisoned")
            .clear();
    }

    pub fn pause(&self) {
        self.pipeline
            .set_state(gst::State::Paused)
            .expect("can pause");

        self.set_name("PokeBot - Paused");
    }

    pub fn stop(&self) {
        self.pipeline
            .set_state(gst::State::Ready)
            .expect("can stop");

        self.set_name("PokeBot - Stopped");
    }

    pub async fn disconnect(&self) {
        self.conn
            .disconnect(DisconnectOptions::new())
            .compat()
            .await;
    }

    pub fn send_message(&self, target: MessageTarget, message: &str) {
        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .send_message(target, message)
                .map_err(|e| println!("Failed to send message: {}", e)),
        );
    }

    pub fn set_name(&self, name: &str) {
        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .set_name(name)
                .map_err(|e| println!("Failed to change name: {}", e)),
        );
    }

    pub fn set_description(&self, desc: &str) {
        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .get_client(&self.conn.lock().own_client)
                .expect("Can get myself")
                .set_description(desc)
                .map_err(|e| println!("Failed to change description: {}", e)),
        );
    }
}
