use futures::stream::StreamExt;
use xtra::{Actor, Handler, WeakAddress};

use tsclientlib::data::exts::{M2BClientEditExt, M2BClientUpdateExt};
use tsclientlib::{
    events::Event,
    sync::{SyncConnection, SyncConnectionHandle, SyncStreamItem},
    ChannelId, ClientId, ConnectOptions, DisconnectOptions, MessageTarget, OutCommandExt, Reason,
};

use slog::{debug, error, info, trace, Logger};

use crate::bot::{ChatMessage, MusicBotMessage};

mod bbcode;

pub use bbcode::*;

#[derive(Clone)]
pub struct TeamSpeakConnection {
    handle: Option<SyncConnectionHandle>,
    logger: Logger,
}

fn get_message(event: &Event) -> Option<MusicBotMessage> {
    use tsclientlib::events::{PropertyId, PropertyValue};

    match event {
        Event::Message {
            target,
            invoker: sender,
            message: msg,
        } => Some(MusicBotMessage::TextMessage(ChatMessage {
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
    pub async fn new(logger: Logger) -> Result<TeamSpeakConnection, tsclientlib::Error> {
        Ok(TeamSpeakConnection {
            handle: None,
            logger,
        })
    }

    pub fn connect_for_bot<T: Actor + Handler<MusicBotMessage>>(
        &mut self,
        options: ConnectOptions,
        bot: WeakAddress<T>,
    ) -> Result<(), tsclientlib::Error> {
        info!(self.logger, "Starting TeamSpeak connection");

        let conn = options.connect()?;
        let mut conn = SyncConnection::from(conn);
        let handle = conn.get_handle();
        self.handle = Some(handle);

        let ev_logger = self.logger.clone();
        tokio::spawn(async move {
            while let Some(item) = conn.next().await {
                use SyncStreamItem::*;

                match item {
                    Ok(ConEvents(events)) => {
                        for event in &events {
                            if let Some(msg) = get_message(event) {
                                tokio::spawn(bot.send(msg));
                            }
                        }
                    }
                    Err(e) => error!(ev_logger, "Error occured during event reading: {}", e),
                    Ok(DisconnectedTemporarily(r)) => {
                        debug!(ev_logger, "Temporary disconnect"; "reason" => ?r)
                    }
                    Ok(Audio(_)) => {
                        trace!(ev_logger, "Audio received");
                    }
                    Ok(IdentityLevelIncreasing(_)) => {
                        trace!(ev_logger, "Identity level increasing");
                    }
                    Ok(IdentityLevelIncreased) => {
                        trace!(ev_logger, "Identity level increased");
                    }
                    Ok(NetworkStatsUpdated) => {
                        trace!(ev_logger, "Network stats updated");
                    }
                }
            }
        });

        let mut handle = self.handle.clone();
        tokio::spawn(async move {
            handle
                .as_mut()
                .expect("connect_for_bot was called")
                .wait_until_connected()
                .await
                .unwrap();
            handle
                .as_mut()
                .expect("connect_for_bot was called")
                .with_connection(|mut conn| {
                    conn.get_state()
                        .expect("can get state")
                        .server
                        .set_subscribed(true)
                        .send(&mut conn)
                })
                .await
                .and_then(|v| v)
                .unwrap();
        });

        Ok(())
    }

    pub async fn send_audio_packet(&mut self, samples: &[u8]) {
        let packet =
            tsproto_packets::packets::OutAudio::new(&tsproto_packets::packets::AudioData::C2S {
                id: 0,
                codec: tsproto_packets::packets::CodecType::OpusMusic,
                data: samples,
            });

        if let Err(e) = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                conn.get_tsproto_client_mut()
                    .expect("can get tsproto client")
                    .send_packet(packet)
            })
            .await
        {
            error!(self.logger, "Failed to send voice packet: {}", e);
        }
    }

    pub async fn channel_of_user(&mut self, id: ClientId) -> Option<ChannelId> {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                conn.get_state()
                    .expect("can get state")
                    .clients
                    .get(&id)
                    .map(|c| c.channel)
            })
            .await
            .map_err(|e| error!(self.logger, "Failed to get channel of user"; "error" => %e))
            .ok()
            .and_then(|v| v)
    }

    pub async fn channel_path_of_user(&mut self, id: ClientId) -> Option<String> {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                let state = conn.get_state().expect("can get state");

                let channel_id = state.clients.get(&id)?.channel;

                let mut channel = state.channels.get(&channel_id)?;

                let mut names = vec![&channel.name[..]];

                // Channel 0 is the root channel
                while channel.parent != ChannelId(0) {
                    names.push("/");
                    channel = state.channels.get(&channel.parent)?;
                    names.push(&channel.name);
                }

                let mut path = String::new();
                while let Some(name) = names.pop() {
                    path.push_str(name);
                }

                Some(path)
            })
            .await
            .map_err(|e| error!(self.logger, "Failed to get channel path of user"; "error" => %e))
            .ok()
            .and_then(|v| v)
    }

    pub async fn current_channel(&mut self) -> Option<ChannelId> {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                let state = conn.get_state().expect("can get state");
                state
                    .clients
                    .get(&state.own_client)
                    .expect("can find myself")
                    .channel
            })
            .await
            .map_err(|e| error!(self.logger, "Failed to get channel"; "error" => %e))
            .ok()
    }

    pub async fn my_id(&mut self) -> ClientId {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| conn.get_state().expect("can get state").own_client)
            .await
            .unwrap()
    }

    pub async fn user_count(&mut self, channel: ChannelId) -> u32 {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
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
        if let Err(e) = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |mut conn| {
                conn.get_state()
                    .expect("can get state")
                    .client_update()
                    .set_name(&name)
                    .send(&mut conn)
            })
            .await
            .and_then(|v| v)
        {
            error!(self.logger, "Failed to set nickname: {}", e);
        }
    }

    pub async fn set_description(&mut self, desc: String) {
        if let Err(e) = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |mut conn| {
                let state = conn.get_state().expect("can get state");
                state
                    .clients
                    .get(&state.own_client)
                    .expect("can get myself")
                    .edit()
                    .set_description(&desc)
                    .send(&mut conn)
            })
            .await
            .and_then(|v| v)
        {
            error!(self.logger, "Failed to change description: {}", e);
        }
    }

    pub async fn send_message_to_channel(&mut self, text: String) {
        if let Err(e) = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |mut conn| {
                conn.get_state()
                    .expect("can get state")
                    .send_message(MessageTarget::Channel, &text)
                    .send(&mut conn)
            })
            .await
            .and_then(|v| v)
        {
            error!(self.logger, "Failed to send message: {}", e);
        }
    }

    pub async fn send_message_to_user(&mut self, client: ClientId, text: String) {
        if let Err(e) = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |mut conn| {
                conn.get_state()
                    .expect("can get state")
                    .send_message(MessageTarget::Client(client), &text)
                    .send(&mut conn)
            })
            .await
            .and_then(|v| v)
        {
            error!(self.logger, "Failed to send message: {}", e);
        }
    }

    pub async fn disconnect(&mut self, reason: &str) -> Result<(), tsclientlib::Error> {
        let opt = DisconnectOptions::new()
            .reason(Reason::Clientdisconnect)
            .message(reason);
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .disconnect(opt)
            .await
    }
}
