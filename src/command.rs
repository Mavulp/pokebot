use std::time::Duration;

use structopt::clap::AppSettings::*;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(
    rename_all = "kebab-case",
    template = "{subcommands}",
    raw(global_settings = "&[VersionlessSubcommands,
                            DisableHelpFlags,
                            DisableVersion,
                            ColorNever,
                            NoBinaryName,
                            AllowLeadingHyphen]",)
)]
pub enum Command {
    /// Adds url to playlist
    Add { url: String },
    /// Adds the first video found on YouTube
    Search { query: Vec<String> },
    /// Starts audio playback
    Play,
    /// Pauses audio playback
    Pause,
    /// Seeks by a specified amount
    Seek { amount: Seek },
    /// Stops audio playback
    Stop,
    /// Switches to the next playlist entry
    #[structopt(alias = "skip")]
    Next,
    /// Clears the playback queue
    Clear,
    /// Changes the volume to the specified value
    Volume { volume: VolumeChange },
    /// Leaves the channel
    Leave,
}

#[derive(Copy, Clone, Debug)]
pub enum Seek {
    Positive(Duration),
    Negative(Duration),
    Absolute(Duration),
}

impl std::str::FromStr for Seek {
    type Err = humantime::DurationError;

    fn from_str(mut amount: &str) -> std::result::Result<Self, Self::Err> {
        let sign = match amount.chars().next() {
            Some('+') => 1,
            Some('-') => -1,
            _ => 0,
        };
        let is_relative = sign != 0;

        if is_relative {
            amount = &amount[1..];
        }

        let duration = humantime::parse_duration(amount)?;

        match sign {
            1 => Ok(Seek::Positive(duration)),
            -1 => Ok(Seek::Negative(duration)),
            _ => Ok(Seek::Absolute(duration)),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum VolumeChange {
    Positive(f64),
    Negative(f64),
    Absolute(f64),
}

// TODO This runs twice, report to clap?
impl std::str::FromStr for VolumeChange {
    type Err = std::num::ParseFloatError;

    fn from_str(mut amount: &str) -> std::result::Result<Self, Self::Err> {
        let sign = match amount.chars().next() {
            Some('+') => 1,
            Some('-') => -1,
            _ => 0,
        };
        let is_relative = sign != 0;

        if is_relative {
            amount = &amount[1..];
        }

        let amount = f64::from_str(amount)? * 0.01;

        match sign {
            1 => Ok(VolumeChange::Positive(amount)),
            -1 => Ok(VolumeChange::Negative(amount)),
            _ => Ok(VolumeChange::Absolute(amount)),
        }
    }
}
