use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::thread;

use futures::future::{FutureExt, TryFutureExt};
use log::{debug, error, info};
use structopt::clap::AppSettings;
use structopt::StructOpt;
use tsclientlib::Identity;

mod audio_player;
mod bot;
mod command;
mod playlist;
mod teamspeak;
mod web_server;
mod youtube_dl;

use bot::{MasterArgs, MasterBot, MusicBot, MusicBotArgs};

#[derive(StructOpt, Debug)]
#[structopt(raw(global_settings = "&[AppSettings::ColoredHelp]"))]
pub struct Args {
    #[structopt(short = "l", long = "local", help = "Run locally in text mode")]
    local: bool,
    #[structopt(
        short = "g",
        long = "generate-identities",
        help = "Generate 'count' identities"
    )]
    gen_id_count: Option<u8>,
    #[structopt(
        short = "a",
        long = "address",
        help = "The address of the server to connect to"
    )]
    address: Option<String>,
    #[structopt(
        help = "Configuration file",
        parse(from_os_str),
        default_value = "config.toml"
    )]
    config_path: PathBuf,
    #[structopt(
        short = "d",
        long = "master_channel",
        help = "The channel the master bot should connect to"
    )]
    master_channel: Option<String>,
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
    if let Err(e) = run() {
        println!("Error: {}", e);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    log4rs::init_file("log4rs.yml", Default::default()).unwrap();

    // Parse command line options
    let args = Args::from_args();

    let mut file = File::open(&args.config_path)?;
    let mut toml = String::new();
    file.read_to_string(&mut toml)?;

    let mut config: MasterArgs = toml::from_str(&toml)?;

    if let Some(count) = args.gen_id_count {
        for _ in 0..count {
            let id = Identity::create().expect("Failed to create id");
            config.ids.push(id);
        }

        let toml = toml::to_string(&config)?;
        let mut file = File::create(&args.config_path)?;
        file.write_all(toml.as_bytes())?;

        return Ok(());
    }

    let bot_args = config.merge(args);

    info!("Starting PokeBot!");
    debug!("Received CLI arguments: {:?}", std::env::args());

    tokio::run(
        async {
            if bot_args.local {
                let name = bot_args.names[0].clone();
                let id = bot_args.ids[0].clone();

                let disconnect_cb = Box::new(move |_, _, _| {});

                let bot_args = MusicBotArgs {
                    name: name,
                    name_index: 0,
                    id_index: 0,
                    local: true,
                    address: bot_args.address.clone(),
                    id,
                    channel: String::from("local"),
                    verbose: bot_args.verbose,
                    disconnect_cb,
                };
                MusicBot::new(bot_args).await.1.await;
            } else {
                let domain = bot_args.domain.clone();
                let bind_address = bot_args.bind_address.clone();
                let (bot, fut) = MasterBot::new(bot_args).await;

                thread::spawn(|| {
                    let web_args = web_server::WebServerArgs {
                        domain,
                        bind_address,
                        bot,
                    };
                    if let Err(e) = web_server::start(web_args) {
                        error!("Error in web server: {}", e);
                    }
                });

                fut.await;
            }
        }
        .unit_error()
        .boxed()
        .compat(),
    );

    Ok(())
}
