use std::error::Error;
use std::process::{Command, Stdio};
use std::sync::Arc;

use futures::{
    compat::Future01CompatExt,
    future::{FutureExt, TryFutureExt},
};
use futures01::{future::Future, sink::Sink};

use futures_util::stream::StreamExt;
use tsclientlib::{Connection, MessageTarget, DisconnectOptions};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_audio as gst_audio;

use byte_slice_cast::*;

#[derive(Clone)]
pub struct State {
    conn: Arc<Connection>,
    pipeline: Arc<gst::Pipeline>,
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
                            element.set_state(gst::State::Null);
                            pipeline.remove_many(&[&element]);
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

                sink.set_property("async", &false);

                volume.set_property("volume", &0.2);

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
                    .expect("Sink element is expected to be an appsink!");

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
                                .map_err(|e| println!("Failed to send voice packet"));

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
        }
    }

    pub fn add_audio<'a>(&self, uri: String) {
        let ytdl_args = [
            "--no-playlist",
            "-f",
            "bestaudio/best",
            "-g",
            &uri,
            "-o",
            "-",
        ];

        let ytdl_output = Command::new("youtube-dl")
            .args(&ytdl_args)
            .stdin(Stdio::null())
            .output()
            .unwrap();

        if ytdl_output.status.success() == false {
            tokio::spawn(
                self.conn
                .lock()
                .to_mut()
                .set_name("PokeBot")
                .map_err(|_| println!("Failed to change name")),
            );
            tokio::spawn(
                self.conn
                .lock()
                .to_mut()
                .send_message(MessageTarget::Channel, "Failed to load url")
                .map_err(|_| println!("Failed to change name")),
            );
            return;
        }
        let dl_url: &str = &String::from_utf8(ytdl_output.stderr).unwrap();

        self.pipeline
            .set_state(gst::State::Ready)
            .expect("Can set state to ready");

        let http_src = self
            .pipeline
            .get_by_name("http-source")
            .expect("Http source should be registered");
        http_src
            .set_property("location", &dl_url)
            .expect("Can set location on http_dl");

        self.pipeline
            .set_state(gst::State::Playing)
            .expect("Can set state to playing");

        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .set_name("PokeBot - Playing")
                .map_err(|_| println!("Failed to change name")),
        );
    }

    pub async fn poll(&self) {
        let bus = self
            .pipeline
            .get_bus()
            .expect("Pipeline without bus. Shouldn't happen!");

        let mut messages = gst::BusStream::new(&bus);
        while let Some(msg) = messages.next().await {
            use gst::MessageView;

            // Determine whether we want to quit: on EOS or error message
            // we quit, otherwise simply continue.
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

        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .set_name("PokeBot")
                .map_err(|_| println!("Failed to change name")),
        );

        self.pipeline
            .set_state(gst::State::Null)
            .expect("Can set state to null");
    }

    pub fn volume(&self, volume: f64) {
        self.pipeline
            .get_by_name("volume")
            .expect("Volume filter should be registered")
            .set_property("volume", &volume)
            .expect("can change volume");
    }

    pub fn play(&self) {
        self.pipeline
            .set_state(gst::State::Playing)
            .expect("can play");

        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .set_name("PokeBot - Playing")
                .map_err(|_| println!("Failed to change name")),
        );
    }

    pub fn pause(&self) {
        self.pipeline
            .set_state(gst::State::Paused)
            .expect("can pause");

        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .set_name("PokeBot - Paused")
                .map_err(|_| println!("Failed to change name")),
        );
    }

    pub fn stop(&self) {
        self.pipeline
            .set_state(gst::State::Ready)
            .expect("can stop");

        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .set_name("PokeBot - Stopped")
                .map_err(|_| println!("Failed to change name")),
        );
    }

    pub async fn disconnect(&self) {
        self.conn
            .disconnect(DisconnectOptions::new())
            .compat()
            .await;
    }
}
