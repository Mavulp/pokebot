use futures::{
    compat::Future01CompatExt,
};
use futures01::{future::Future, sink::Sink};

use tsclientlib::{Connection, ConnectOptions, events::Event, ClientId, MessageTarget};
use tsclientlib::Event::ConEvents;
use crate::{ApplicationMessage, Message};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, Arc};

use log::{error};

pub struct TeamSpeakConnection {
    conn: Connection,
}

fn get_message<'a>(event: &Event) -> Option<Message> {
    match event {
        Event::Message {
            from: target,
            invoker: sender,
            message: msg,
        } => {
            Some(Message {
                target: target.clone(),
                invoker: sender.clone(),
                text: msg.clone(),
            })
        }
        _ => None,
    }
}

impl TeamSpeakConnection {
    pub async fn new(tx: Arc<Mutex<Sender<ApplicationMessage>>>, options: ConnectOptions) -> Result<TeamSpeakConnection, tsclientlib::Error> {
        let conn = Connection::new(options).compat().await?;
        let packet = conn.lock().server.set_subscribed(true);
        conn.send_packet(packet).compat().await.unwrap();

        conn.add_event_listener(
            String::from("listener"),
            Box::new(move |e| {
                if let ConEvents(_conn, events) = e {
                    for event in *events {
                        if let Some(msg) = get_message(event) {
                            let tx = tx.lock().unwrap();
                            tx.send(ApplicationMessage::TextMessage(msg)).unwrap();
                        }
                    }
                }
            }),
        );

        Ok(TeamSpeakConnection {
            conn,
        })
    }

    pub fn send_audio_packet(&self, samples: &[u8]) {
        let packet = tsproto_packets::packets::OutAudio::new(
            &tsproto_packets::packets::AudioData::C2S {
                id: 0,
                codec: tsproto_packets::packets::CodecType::OpusMusic,
                data: samples,
            },
        );

        let send_packet = self.conn
            .get_packet_sink()
            .send(packet)
            .map(|_| ())
            .map_err(|_| error!("Failed to send voice packet"));

        tokio::run(send_packet);
    }

    pub fn join_channel_of_user(&self, id: ClientId) {
        let channel = self.conn.lock()
            .clients
            .get(&id)
            .expect("can find poke sender")
            .channel;
        tokio::spawn(self.conn.lock().to_mut()
                .get_client(&self.conn.lock().own_client)
                .expect("can get myself")
                .set_channel(channel)
                .map_err(|e| error!("Failed to switch channel: {}", e)));
    }

    pub fn set_nickname(&self, name: &str) {
        tokio::spawn(self.conn
                .lock()
                .to_mut()
                .set_name(name)
                .map_err(|e| error!("Failed to set nickname: {}", e)));
    }

    pub fn set_description(&self, desc: &str) {
        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .get_client(&self.conn.lock().own_client)
                .expect("Can get myself")
                .set_description(desc)
                .map_err(|e| error!("Failed to change description: {}", e)));
    }

    pub fn send_message_to_channel(&self, text: &str) {
        tokio::spawn(self.conn.lock().to_mut()
                .send_message(MessageTarget::Channel, text)
                .map_err(|e| error!("Failed to send message: {}", e)));

    }
}
