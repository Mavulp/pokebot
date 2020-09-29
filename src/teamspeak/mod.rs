use std::sync::{Arc, RwLock};

use futures::stream::StreamExt;
use tokio::sync::mpsc::UnboundedSender;

use tsclientlib::data::exts::{M2BClientEditExt, M2BClientUpdateExt};
use tsclientlib::{
    events::Event,
    sync::{SyncConnection, SyncConnectionHandle, SyncStreamItem},
    ChannelId, ClientId, ConnectOptions, Connection, DisconnectOptions, MessageTarget,
    OutCommandExt, Reason,
};

use log::{debug, error};

use crate::bot::{Message, MusicBotMessage};

mod bbcode;

pub use bbcode::*;

#[derive(Clone)]
pub struct TeamSpeakConnection {
    handle: SyncConnectionHandle,
}

fn get_message(event: &Event) -> Option<MusicBotMessage> {
    use tsclientlib::events::{PropertyId, PropertyValue};

    match event {
        Event::Message {
            target,
            invoker: sender,
            message: msg,
        } => Some(MusicBotMessage::TextMessage(Message {
            target: *target,
            invoker: sender.clone(),
            text: msg.clone(),
        })),
        Event::PropertyAdded {
            id: property,
            invoker: _,
            extra: _,
        } => match property {
            PropertyId::Channel(id) => Some(MusicBotMessage::ChannelAdded(*id)),
            PropertyId::Client(id) => Some(MusicBotMessage::ClientAdded(*id)),
            _ => None,
        },
        Event::PropertyChanged {
            id: property,
            old: from,
            invoker: _,
            extra: _,
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
            extra: _,
        } => match property {
            PropertyId::Client(id) => {
                if let PropertyValue::Client(client) = client {
                    Some(MusicBotMessage::ClientDisconnected {
                        id: *id,
                        client: Box::new(client.clone()),
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
        tx: Arc<RwLock<UnboundedSender<MusicBotMessage>>>,
        options: ConnectOptions,
    ) -> Result<TeamSpeakConnection, tsclientlib::Error> {
        let conn = Connection::new(options)?;
        let conn = SyncConnection::from(conn);
        let mut handle = conn.get_handle();

        tokio::spawn(conn.for_each(move |i| {
            let tx = tx.clone();
            async move {
                match i {
                    Ok(SyncStreamItem::ConEvents(events)) => {
                        for event in &events {
                            if let Some(msg) = get_message(event) {
                                let tx = tx.read().expect("RwLock was not poisoned");
                                // Ignore the result because the receiver might get dropped first.
                                let _ = tx.send(msg);
                            }
                        }
                    }
                    Err(e) => error!("Error occured during event reading: {}", e),
                    Ok(SyncStreamItem::DisconnectedTemporarily) => debug!("Temporary disconnect!"),
                    _ => (),
                }
            }
        }));

        handle.wait_until_connected().await?;

        let mut chandle = handle.clone();
        chandle
            .with_connection(|mut conn| {
                conn.get_state()
                    .expect("is connected")
                    .server
                    .set_subscribed(true)
                    .send(&mut conn)
                    .unwrap()
            })
            .await
            .unwrap();

        Ok(TeamSpeakConnection { handle })
    }

    pub async fn send_audio_packet(&mut self, samples: &[u8]) {
        let packet =
            tsproto_packets::packets::OutAudio::new(&tsproto_packets::packets::AudioData::C2S {
                id: 0,
                codec: tsproto_packets::packets::CodecType::OpusMusic,
                data: samples,
            });

        self.handle
            .with_connection(|conn| {
                if let Err(e) = conn
                    .get_tsproto_client_mut()
                    .expect("can get tsproto client")
                    .send_packet(packet)
                {
                    error!("Failed to send voice packet: {}", e);
                }
            })
            .await
            .unwrap();
    }

    pub async fn channel_of_user(&mut self, id: ClientId) -> Option<ChannelId> {
        self.handle
            .with_connection(move |conn| {
                conn.get_state()
                    .expect("can get state")
                    .clients
                    .get(&id)
                    .map(|c| c.channel)
            })
            .await
            .unwrap()
    }

    pub async fn channel_path_of_user(&mut self, id: ClientId) -> Option<String> {
        self.handle
            .with_connection(move |conn| {
                let state = conn.get_state().expect("can get state");

                let channel_id = state.clients.get(&id)?.channel;

                let mut channel = state
                    .channels
                    .get(&channel_id)
                    .expect("can find user channel");

                let mut names = vec![&channel.name[..]];

                // Channel 0 is the root channel
                while channel.parent != ChannelId(0) {
                    names.push("/");
                    channel = state
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
            })
            .await
            .unwrap()
    }

    pub async fn my_channel(&mut self) -> ChannelId {
        self.handle
            .with_connection(move |conn| {
                let state = conn.get_state().expect("can get state");
                state
                    .clients
                    .get(&state.own_client)
                    .expect("can find myself")
                    .channel
            })
            .await
            .unwrap()
    }

    pub async fn my_id(&mut self) -> ClientId {
        self.handle
            .with_connection(move |conn| conn.get_state().expect("can get state").own_client)
            .await
            .unwrap()
    }

    pub async fn user_count(&mut self, channel: ChannelId) -> u32 {
        self.handle
            .with_connection(move |conn| {
                let state = conn.get_state().expect("can get state");
                let mut count = 0;
                for client in state.clients.values() {
                    if client.channel == channel {
                        count += 1;
                    }
                }

                count
            })
            .await
            .unwrap()
    }

    pub async fn set_nickname(&mut self, name: String) {
        self.handle
            .with_connection(move |mut conn| {
                conn.get_state()
                    .expect("can get state")
                    .client_update()
                    .set_name(&name)
                    .send(&mut conn)
                    .map_err(|e| error!("Failed to set nickname: {}", e))
            })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn set_description(&mut self, desc: String) {
        self.handle
            .with_connection(move |mut conn| {
                let state = conn.get_state().expect("can get state");
                let _ = state
                    .clients
                    .get(&state.own_client)
                    .expect("can get myself")
                    .edit()
                    .set_description(&desc)
                    .send(&mut conn)
                    .map_err(|e| error!("Failed to change description: {}", e));
            })
            .await
            .unwrap()
    }

    pub async fn send_message_to_channel(&mut self, text: String) {
        self.handle
            .with_connection(move |mut conn| {
                let _ = conn
                    .get_state()
                    .expect("can get state")
                    .send_message(MessageTarget::Channel, &text)
                    .send(&mut conn)
                    .map_err(|e| error!("Failed to send message: {}", e));
            })
            .await
            .unwrap()
    }

    pub async fn send_message_to_user(&mut self, client: ClientId, text: String) {
        self.handle
            .with_connection(move |mut conn| {
                let _ = conn
                    .get_state()
                    .expect("can get state")
                    .send_message(MessageTarget::Client(client), &text)
                    .send(&mut conn)
                    .map_err(|e| error!("Failed to send message: {}", e));
            })
            .await
            .unwrap()
    }

    pub async fn subscribe_all(&mut self) {
        self.handle
            .with_connection(move |mut conn| {
                if let Err(e) = conn
                    .get_state()
                    .expect("can get state")
                    .server
                    .set_subscribed(true)
                    .send(&mut conn)
                {
                    error!("Failed to send subscribe packet: {}", e);
                }
            })
            .await
            .unwrap()
    }

    pub async fn disconnect(&mut self, reason: &str) {
        let opt = DisconnectOptions::new()
            .reason(Reason::Clientdisconnect)
            .message(reason);
        self.handle.disconnect(opt).await.unwrap();
    }
}
