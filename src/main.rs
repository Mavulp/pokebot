use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::thread;

use slog::{debug, error, info, o, Drain, Logger};
use structopt::clap::AppSettings;
use structopt::StructOpt;
#[cfg(unix)]
use tokio::signal::unix::*;
use tsclientlib::Identity;

mod audio_player;
mod bot;
mod command;
mod log_bridge;
mod playlist;
mod teamspeak;
mod web_server;
mod youtube_dl;

use bot::{MasterArgs, MasterBot, MusicBot, MusicBotArgs, Quit};
use log_bridge::LogBridge;

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
    // 0. Print nothing
    // 1. Print command string
    // 2. Print packets
    // 3. Print udp packets
    #[structopt(
        short = "v",
        long = "verbose",
        help = "Print the content of all packets",
        parse(from_occurrences)
    )]
    verbose: u8,
}

#[tokio::main]
async fn main() {
    let root_logger = {
        let config = log4rs::load_config_file("log4rs.yml", Default::default()).unwrap();
        let drain = LogBridge(log4rs::Logger::new(config)).fuse();
        // slog_async adds a channel because log4rs if not unwind safe
        let drain = slog_async::Async::new(drain).build().fuse();

        Logger::root(drain, o!())
    };

    let scope_guard = slog_scope::set_global_logger(root_logger.clone());
    // On SIGTERM the logger resets for some reason which makes the bot panic
    // if it tries to log anything
    scope_guard.cancel_reset();

    slog_stdlog::init().unwrap();

    if let Err(e) = run(root_logger.clone()).await {
        error!(root_logger, "{}", e);
    }
}

async fn run(root_logger: Logger) -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line options
    let args = Args::from_args();

    // Set up signal handlers
    let ctrl_c = tokio::task::spawn(tokio::signal::ctrl_c());
    #[cfg(unix)]
    let (sighup, sigterm, sigquit) = (
        tokio::task::spawn(hangup()),
        tokio::task::spawn(terminate()),
        tokio::task::spawn(quit()),
    );

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
            info!(root_logger, "Upgrading master identity");
            id.upgrade_level(level).expect("can upgrade level");
        }

        if let Some(ids) = &mut config.ids {
            let len = ids.len();
            for (i, id) in ids.iter_mut().enumerate() {
                info!(root_logger, "Upgrading bot identity"; "current" => i + 1, "amount" => len);
                id.upgrade_level(level).expect("can upgrade level");
            }
        }

        let toml = toml::to_string(&config)?;
        let mut file = File::create(&args.config_path)?;
        file.write_all(toml.as_bytes())?;

        return Ok(());
    }

    if config.id.is_none() || config.ids.is_none() {
        error!(
            root_logger,
            "Failed to find required identites, try running with `-g`"
        );
        return Ok(());
    }

    let local = args.local;
    let bot_args = config.merge(args);

    info!(root_logger, "Starting PokeBot!");
    debug!(root_logger, "Received CLI arguments"; "args" => ?std::env::args());

    if local {
        let name = bot_args.names[0].clone();
        let identity = bot_args.ids.expect("identies should exists")[0].clone();

        let bot_args = MusicBotArgs {
            name,
            master: None,
            local: true,
            address: bot_args.address.clone(),
            identity,
            channel: String::from("local"),
            verbose: bot_args.verbose,
            logger: root_logger,
        };
        MusicBot::spawn(bot_args).await;

        ctrl_c.await??;
    } else {
        let domain = bot_args.domain.clone();
        let bind_address = bot_args.bind_address.clone();
        let bot_name = bot_args.master_name.clone();
        let bot_logger = root_logger.new(o!("master" => bot_name.clone()));
        let bot = MasterBot::spawn(bot_args, bot_logger).await;

        let web_args = web_server::WebServerArgs {
            domain,
            bind_address,
            bot: bot.downgrade(),
        };
        spawn_web_server(web_args, root_logger.new(o!("webserver" => bot_name)));

        #[cfg(unix)]
        tokio::select! {
            res = ctrl_c => {
                res??;
                info!(root_logger, "Received signal, shutting down"; "signal" => "SIGINT");
            }
            _ = sigterm => {
                info!(root_logger, "Received signal, shutting down"; "signal" => "SIGTERM");
            }
            _ = sighup => {
                info!(root_logger, "Received signal, shutting down"; "signal" => "SIGHUP");
            }
            _ = sigquit => {
                info!(root_logger, "Received signal, shutting down"; "signal" => "SIGQUIT");
            }
        };

        #[cfg(windows)]
        ctrl_c.await??;

        bot.send(Quit(String::from("Stopping")))
            .await
            .unwrap()
            .unwrap();
    }

    Ok(())
}

pub fn spawn_web_server(args: web_server::WebServerArgs, logger: Logger) {
    thread::spawn(move || {
        if let Err(e) = web_server::start(args, logger.clone()) {
            error!(logger, "Error in web server"; "error" => %e);
        }
    });
}

#[cfg(unix)]
pub async fn terminate() -> std::io::Result<()> {
    signal(SignalKind::terminate())?.recv().await;
    Ok(())
}

#[cfg(unix)]
pub async fn hangup() -> std::io::Result<()> {
    signal(SignalKind::hangup())?.recv().await;
    Ok(())
}

#[cfg(unix)]
pub async fn quit() -> std::io::Result<()> {
    signal(SignalKind::quit())?.recv().await;
    Ok(())
}
