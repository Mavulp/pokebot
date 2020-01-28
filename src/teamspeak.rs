use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::compat::Future01CompatExt;
use futures01::{future::Future, sink::Sink};
use tokio02::sync::mpsc::UnboundedSender;

use tsclientlib::Event::ConEvents;
use tsclientlib::{
    events::Event, ChannelId, ClientId, ConnectOptions, Connection, DisconnectOptions,
    MessageTarget, Reason,
};

use log::error;

use crate::bot::{Message, MusicBotMessage};

pub struct TeamSpeakConnection {
    conn: Connection,
}

fn get_message<'a>(event: &Event) -> Option<MusicBotMessage> {
    use tsclientlib::events::{PropertyId, PropertyValue};

    match event {
        Event::Message {
            from: target,
            invoker: sender,
            message: msg,
        } => Some(MusicBotMessage::TextMessage(Message {
            target: *target,
            invoker: sender.clone(),
            text: msg.clone(),
        })),
        Event::PropertyChanged {
            id: property,
            old: from,
            invoker: _,
        } => match property {
            PropertyId::ClientChannel(client) => {
                if let PropertyValue::ChannelId(from) = from {
                    Some(MusicBotMessage::ClientChannel {
                        client: *client,
                        old_channel: *from,
                    })
                } else {
                    None
                }
            }
            _ => None,
        },
        Event::PropertyRemoved {
            id: property,
            old: client,
            invoker: _,
        } => match property {
            PropertyId::Client(id) => {
                if let PropertyValue::Client(client) = client {
                    Some(MusicBotMessage::ClientDisconnected {
                        id: *id,
                        client: client.clone(),
                    })
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    }
}

impl TeamSpeakConnection {
    pub async fn new(
        tx: Arc<Mutex<UnboundedSender<MusicBotMessage>>>,
        options: ConnectOptions,
    ) -> Result<TeamSpeakConnection, tsclientlib::Error> {
        let conn = Connection::new(options).compat().await?;
        let packet = conn.lock().server.set_subscribed(true);
        conn.send_packet(packet).compat().await.unwrap();

        conn.add_event_listener(
            String::from("listener"),
            Box::new(move |e| {
                if let ConEvents(_conn, events) = e {
                    for event in *events {
                        if let Some(msg) = get_message(event) {
                            let tx = tx.lock().expect("Mutex was not poisoned");
                            // Ignore the result because the receiver might get dropped first.
                            let _ = tx.send(msg);
                        }
                    }
                }
            }),
        );

        Ok(TeamSpeakConnection { conn })
    }

    pub fn send_audio_packet(&self, samples: &[u8]) {
        let packet =
            tsproto_packets::packets::OutAudio::new(&tsproto_packets::packets::AudioData::C2S {
                id: 0,
                codec: tsproto_packets::packets::CodecType::OpusMusic,
                data: samples,
            });

        let send_packet = self
            .conn
            .get_packet_sink()
            .send(packet)
            .map(|_| ())
            .map_err(|_| error!("Failed to send voice packet"));

        tokio::run(send_packet);
    }

    pub fn channel_of_user(&self, id: ClientId) -> Option<ChannelId> {
        Some(self.conn.lock().clients.get(&id)?.channel)
    }

    pub fn channel_path_of_user(&self, id: ClientId) -> Option<String> {
        let conn = self.conn.lock();

        let channel_id = conn.clients.get(&id)?.channel;

        let mut channel = conn
            .channels
            .get(&channel_id)
            .expect("can find user channel");

        let mut names = vec![&channel.name[..]];

        // Channel 0 is the root channel
        while channel.parent != ChannelId(0) {
            names.push("/");
            channel = conn
                .channels
                .get(&channel.parent)
                .expect("can find user channel");
            names.push(&channel.name);
        }

        let mut path = String::new();
        while let Some(name) = names.pop() {
            path.push_str(name);
        }

        Some(path)
    }

    pub fn my_channel(&self) -> ChannelId {
        let conn = self.conn.lock();
        conn.clients
            .get(&conn.own_client)
            .expect("can find myself")
            .channel
    }

    pub fn user_count(&self, channel: ChannelId) -> u32 {
        let conn = self.conn.lock();
        let mut count = 0;
        for (_, client) in &conn.clients {
            if client.channel == channel {
                count += 1;
            }
        }

        count
    }

    pub fn set_nickname(&self, name: &str) {
        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .set_name(name)
                .map_err(|e| error!("Failed to set nickname: {}", e)),
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
                .map_err(|e| error!("Failed to change description: {}", e)),
        );
    }

    pub fn send_message_to_channel(&self, text: &str) {
        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .send_message(MessageTarget::Channel, text)
                .map_err(|e| error!("Failed to send message: {}", e)),
        );
    }

    pub fn send_message_to_user(&self, id: ClientId, text: &str) {
        tokio::spawn(
            self.conn
                .lock()
                .to_mut()
                .send_message(MessageTarget::Client(id), text)
                .map_err(|e| error!("Failed to send message: {}", e)),
        );
    }

    pub fn disconnect(&self, reason: &str) {
        let opt = DisconnectOptions::new()
            .reason(Reason::Clientdisconnect)
            .message(reason);
        tokio::spawn(
            self.conn
                .disconnect(opt)
                .map_err(|e| error!("Failed to send message: {}", e)),
        );
        // Might or might not be required to keep tokio running while the bot disconnects
        tokio::spawn(
            tokio::timer::Delay::new(Instant::now() + Duration::from_secs(1)).map_err(|_| ()),
        );
    }
}
