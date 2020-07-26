use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

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
#[structopt(global_settings = &[AppSettings::ColoredHelp])]
pub struct Args {
    #[structopt(short = "l", long = "local", help = "Run locally in text mode")]
    local: bool,
    #[structopt(
        short = "g",
        long = "generate-identities",
        help = "Generate 'count' identities"
    )]
    gen_id_count: Option<u8>,
    /// Increases the security level of all identities in the config file
    #[structopt(short, long = "increase-security-level")]
    wanted_level: Option<u8>,
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

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        println!("Error: {}", e);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    log4rs::init_file("log4rs.yml", Default::default()).unwrap();

    // Parse command line options
    let args = Args::from_args();

    let mut file = File::open(&args.config_path)?;
    let mut toml = String::new();
    file.read_to_string(&mut toml)?;

    let mut config: MasterArgs = toml::from_str(&toml)?;

    if config.id.is_none() {
        let id = Identity::create().expect("Failed to create id");
        config.id = Some(id);
    }

    if let Some(count) = args.gen_id_count {
        for _ in 0..count {
            let id = Identity::create().expect("Failed to create id");
            if let Some(ids) = &mut config.ids {
                ids.push(id);
            } else {
                config.ids = Some(vec![id]);
            }
        }

        let toml = toml::to_string(&config)?;
        let mut file = File::create(&args.config_path)?;
        file.write_all(toml.as_bytes())?;

        return Ok(());
    }

    if let Some(level) = args.wanted_level {
        if let Some(id) = &mut config.id {
            info!("Upgrading master identity");
            id.upgrade_level(level).expect("can upgrade level");
        }

        if let Some(ids) = &mut config.ids {
            let len = ids.len();
            for (i, id) in ids.iter_mut().enumerate() {
                info!("Upgrading bot identity {}/{}", i + 1, len);
                id.upgrade_level(level).expect("can upgrade level");
            }
        }

        let toml = toml::to_string(&config)?;
        let mut file = File::create(&args.config_path)?;
        file.write_all(toml.as_bytes())?;

        return Ok(());
    }

    if config.id.is_none() || config.ids.is_none() {
        error!("Failed to find required identites, try running with `-g`");
        return Ok(());
    }

    let bot_args = config.merge(args);

    info!("Starting PokeBot!");
    debug!("Received CLI arguments: {:?}", std::env::args());

    if bot_args.local {
        let name = bot_args.names[0].clone();
        let id = bot_args.ids.expect("identies should exists")[0].clone();

        let disconnect_cb = Box::new(move |_, _, _| {});

        let bot_args = MusicBotArgs {
            name,
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
        // Keep tokio running while the bot disconnects
        tokio::time::delay_for(Duration::from_secs(1)).await;
    }

    Ok(())
}
