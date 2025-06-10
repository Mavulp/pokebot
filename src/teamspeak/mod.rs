use futures::stream::StreamExt;
use xtra::{Actor, Handler, WeakAddress};

use tsclientlib::data::exts::{M2BClientEditExt, M2BClientUpdateExt};
use tsclientlib::{
    events::Event,
    sync::{SyncConnection, SyncConnectionHandle, SyncStreamItem},
    ChannelId, ClientId, ConnectOptions, DisconnectOptions, MessageTarget, OutCommandExt, Reason,
};

use tracing::{debug, error, info, trace, warn, Span};

use crate::bot::{ChatMessage, MusicBotMessage};

mod bbcode;

pub use bbcode::*;

#[derive(Clone)]
pub struct TeamSpeakConnection {
    handle: Option<SyncConnectionHandle>,
    span: Span,
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
    }
}

impl TeamSpeakConnection {
    pub async fn new(span: Span) -> anyhow::Result<TeamSpeakConnection> {
        Ok(TeamSpeakConnection { handle: None, span })
    }

    pub fn connect_for_bot<T: Actor + Handler<MusicBotMessage>>(
        &mut self,
        options: ConnectOptions,
        bot: WeakAddress<T>,
    ) -> anyhow::Result<()> {
        info!(parent: &self.span, "Starting TeamSpeak connection");

        let conn = options.connect()?;
        let mut conn = SyncConnection::from(conn);
        let handle = conn.get_handle();
        self.handle = Some(handle);

        let ev_span = self.span.clone();
        tokio::spawn(async move {
            while let Some(item) = conn.next().await {
                use SyncStreamItem::*;

                match item {
                    Ok(BookEvents(events)) => {
                        for event in &events {
                            if let Some(msg) = get_message(event) {
                                // FIXME: Errors are just getting dropped
                                tokio::spawn(bot.send(msg));
                            }
                        }
                    }
                    Err(e) => error!(
                        parent:  &ev_span,
                        "Error occured during event reading: {}", e
                    ),
                    Ok(MessageEvent(_)) => {
                        trace!(parent: &ev_span, "Message event was received");
                    }
                    Ok(DisconnectedTemporarily(r)) => {
                        debug!(parent: &ev_span, reason = ?r, "Temporary disconnect")
                    }
                    Ok(Audio(_)) => {
                        trace!(parent: &ev_span, "Audio received");
                    }
                    Ok(IdentityLevelIncreasing(_)) => {
                        trace!(parent: &ev_span, "Identity level increasing");
                    }
                    Ok(IdentityLevelIncreased) => {
                        trace!(parent: &ev_span, "Identity level increased");
                    }
                    Ok(NetworkStatsUpdated) => {
                        trace!(parent: &ev_span, "Network stats updated");
                    }
                    Ok(AudioChange(_)) => {
                        trace!(parent: &ev_span, "Audio status changed");
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
                .with_connection(|conn| {
                    conn.get_state()
                        .expect("can get state")
                        .server
                        .set_subscribed(true)
                        .send(conn)
                })
                .await
                .and_then(|v| v)
                .unwrap();
        });

        Ok(())
    }

    pub async fn send_audio_packet(&mut self, samples: &[u8]) -> anyhow::Result<()> {
        let packet =
            tsproto_packets::packets::OutAudio::new(&tsproto_packets::packets::AudioData::C2S {
                id: 0,
                codec: tsproto_packets::packets::CodecType::OpusMusic,
                data: samples,
            });

        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                conn.get_tsproto_client_mut()
                    .expect("can get tsproto client")
                    .send_packet(packet)
            })
            .await??;

        Ok(())
    }

    pub async fn channel_of_user(&mut self, id: ClientId) -> anyhow::Result<Option<ChannelId>> {
        let id = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                conn.get_state()
                    .expect("can get state")
                    .clients
                    .get(&id)
                    .map(|c| c.channel)
            })
            .await?;

        Ok(id)
    }

    pub async fn channel_path_of_user(&mut self, id: ClientId) -> anyhow::Result<Option<String>> {
        let path = self
            .handle
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
            .await?;

        Ok(path)
    }

    pub async fn current_channel(&mut self) -> anyhow::Result<Option<ChannelId>> {
        let id = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                let state = conn.get_state().expect("can get state");
                state.clients.get(&state.own_client).map(|c| c.channel)
            })
            .await?;

        Ok(id)
    }

    pub async fn my_id(&mut self) -> anyhow::Result<ClientId> {
        let id = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| conn.get_state().expect("can get state").own_client)
            .await?;

        Ok(id)
    }

    pub async fn user_count(&mut self, channel: ChannelId) -> anyhow::Result<u32> {
        let count = self
            .handle
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
            .await?;

        Ok(count)
    }

    pub async fn set_nickname(&mut self, name: String) -> anyhow::Result<()> {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                conn.get_state()
                    .expect("can get state")
                    .client_update()
                    .set_name(&name)
                    .send(conn)
            })
            .await??;

        Ok(())
    }

    pub async fn set_description(&mut self, desc: String) {
        if let Err(error) = self
            .handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                let state = conn.get_state().expect("can get state");
                state
                    .clients
                    .get(&state.own_client)
                    .expect("can get myself")
                    .edit()
                    .set_description(&desc)
                    .send(conn)
            })
            .await
            .and_then(|v| v)
        {
            warn!(parent: &self.span, %error, "Failed to set description");
        }
    }

    pub async fn send_message_to_channel(&mut self, text: String) -> anyhow::Result<()> {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                conn.get_state()
                    .expect("can get state")
                    .send_message(MessageTarget::Channel, &text)
                    .send(conn)
            })
            .await??;

        Ok(())
    }

    pub async fn send_message_to_user(
        &mut self,
        client: ClientId,
        text: String,
    ) -> anyhow::Result<()> {
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .with_connection(move |conn| {
                conn.get_state()
                    .expect("can get state")
                    .send_message(MessageTarget::Client(client), &text)
                    .send(conn)
            })
            .await??;

        Ok(())
    }

    pub async fn disconnect(&mut self, reason: &str) -> anyhow::Result<()> {
        let opt = DisconnectOptions::new()
            .reason(Reason::Clientdisconnect)
            .message(reason);
        self.handle
            .as_mut()
            .expect("connect_for_bot was called")
            .disconnect(opt)
            .await?;

        Ok(())
    }
}
