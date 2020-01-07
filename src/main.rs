use std::io::Read;
use std::path::PathBuf;
use std::str::FromStr;

use futures::{
    compat::Future01CompatExt,
    future::{FutureExt, TryFutureExt},
};

use futures01::future::Future;
use structopt::clap::AppSettings;
use structopt::StructOpt;

use tsclientlib::{
    events::Event, ConnectOptions, Connection, ConnectionLock, Event::ConEvents,
    Identity, MessageTarget,
};

use log::error;

mod playlist;
mod state;
use state::State;

#[derive(StructOpt, Debug)]
#[structopt(raw(global_settings = "&[AppSettings::ColoredHelp]"))]
struct Args {
    #[structopt(
        short = "a",
        long = "address",
        default_value = "localhost",
        help = "The address of the server to connect to"
    )]
    address: String,
    #[structopt(
        short = "i",
        long = "id",
        help = "Identity file - good luck creating one",
        parse(from_os_str)
    )]
    id_path: Option<PathBuf>,
    #[structopt(
        short = "c",
        long = "channel",
        help = "The channel the bot should connect to"
    )]
    default_channel: Option<String>,
    #[structopt(
        short = "v",
        long = "verbose",
        help = "Print the content of all packets",
        parse(from_occurrences)
    )]
    verbose: u8,
    // 0. Print nothing
    // 1. Print command string
    // 2. Print packets
    // 3. Print udp packets
}

fn main() {
    tokio::run(async_main().unit_error().boxed().compat());
}

async fn async_main() {
    // Parse command line options
    let args = Args::from_args();

    let id = if let Some(path) = args.id_path {
        let mut file = std::fs::File::open(path).expect("Failed to open id file");
        let mut content = String::new();
        file.read_to_string(&mut content)
            .expect("Failed to read id file");

        toml::from_str(&content).expect("Failed to parse id file")
    } else {
        Identity::create().expect("Failed to create id")
    };

    let mut con_config = ConnectOptions::new(args.address)
        .version(tsclientlib::Version::Linux_3_3_2)
        .name(String::from("PokeBot"))
        .identity(id)
        .log_commands(args.verbose >= 1)
        .log_packets(args.verbose >= 2)
        .log_udp_packets(args.verbose >= 3);

    if let Some(channel) = args.default_channel {
        con_config = con_config.channel(channel);
    }

    //let (disconnect_send, disconnect_recv) = mpsc::unbounded();
    let conn = Connection::new(con_config).compat().await.unwrap();

    let state = State::new(conn.clone());
    {
        let packet = conn.lock().server.set_subscribed(true);
        conn.send_packet(packet).compat().await.unwrap();
    }
    //con.add_on_disconnect(Box::new( || {
    //disconnect_send.unbounded_send(()).unwrap()
    //}));
    let inner_state = state.clone();
    conn.add_event_listener(
        String::from("listener"),
        Box::new(move |e| {
            if let ConEvents(conn, events) = e {
                for event in *events {
                    handle_event(&inner_state, &conn, event);
                }
            }
        }),
    );

    loop {
        state.poll().await;
    }
    //let ctrl_c = tokio_signal::ctrl_c().flatten_stream();

    //let dc_fut = disconnect_recv.into_future().compat().fuse();
    //let ctrlc_fut = ctrl_c.into_future().compat().fuse();
    //ctrlc_fut.await.map_err(|(e, _)| e).unwrap();

    //conn.disconnect(DisconnectOptions::new())
        //.compat()
        //.await
        //.unwrap();

    // TODO Should not be required
    //std::process::exit(0);
}

fn handle_event<'a>(state: &State, conn: &ConnectionLock<'a>, event: &Event) {
    match event {
        Event::Message {
            from: target,
            invoker: sender,
            message: msg,
        } => {
            if let MessageTarget::Poke(who) = target {
                let channel = conn
                    .clients
                    .get(&who)
                    .expect("can find poke sender")
                    .channel;
                tokio::spawn(
                    conn.to_mut()
                        .get_client(&conn.own_client)
                        .expect("can get myself")
                        .set_channel(channel)
                        .map_err(|e| error!("Failed to switch channel: {}", e)),
                );
            } else if sender.id != conn.own_client {
                if msg.starts_with("!") {
                    let tokens = msg[1..].split_whitespace().collect::<Vec<_>>();
                    match tokens.get(0).map(|t| *t) {
                        Some("test") => {
                            tokio::spawn(
                                conn.to_mut()
                                    .send_message(*target, "works :)")
                                    .map_err(|_| ()),
                            );
                        }
                        Some("add") => {
                            let mut invalid = false;
                            if let Some(url) = &tokens.get(1) {
                                if url.len() > 11 {
                                    tokio::spawn(
                                        conn.to_mut().set_name("PokeBot - Loading").map_err(|_| ()),
                                    );
                                    let trimmed = url[5..url.len() - 6].to_owned();
                                    let inner_state = state.clone();
                                    tokio::spawn(
                                        async move { inner_state.add_audio(trimmed).await }
                                            .unit_error()
                                            .boxed()
                                            .compat(),
                                    );
                                } else {
                                    invalid = true;
                                }
                            } else {
                                invalid = true;
                            }
                            if invalid {
                                tokio::spawn(
                                    conn.to_mut()
                                        .send_message(MessageTarget::Channel, "Invalid Url")
                                        .map_err(|_| ()),
                                );
                            }
                        }
                        Some("volume") => {
                            if let Ok(volume) = f64::from_str(tokens[1]) {
                                if 0.0 <= volume && volume <= 100.0 {
                                    state.volume(volume);
                                } else {
                                    tokio::spawn(
                                        conn.to_mut()
                                            .send_message(
                                                MessageTarget::Channel,
                                                "Volume must be between 0 and 100",
                                            )
                                            .map_err(|_| ()),
                                    );
                                }
                            }
                        }
                        Some("play") => {
                            state.play();
                        }
                        Some("skip") => {
                            state.next();
                        }
                        Some("clear") => {
                            state.clear();
                        }
                        Some("pause") => {
                            state.pause();
                        }
                        Some("stop") => {
                            state.stop();
                        }
                        _ => (),
                    };
                }
            }
        }
        _ => (),
    }
}
