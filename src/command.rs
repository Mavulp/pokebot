use structopt::clap::AppSettings::*;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(
    rename_all = "kebab-case",
    template = "Try one of these commands:\n{subcommands}",
    raw(global_settings = "&[VersionlessSubcommands, ColorNever]",)
)]
pub enum Command {
    /// Adds url to playlist
    Add { url: String },
    /// Starts audio playback
    Play,
    /// Pauses audio playback
    Pause,
    /// Stops audio playback
    Stop,
    /// Switches to the next queue entry
    Next,
    /// Clears the playback queue
    Clear,
    /// Changes the volume to the specified value
    Volume { percent: f64 },
}
