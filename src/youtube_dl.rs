use std::time::Duration;

use std::process::Stdio;
use tokio::process::Command;

use serde::{Deserialize, Serialize};

use slog::{debug, Logger};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AudioMetadata {
    #[serde(rename = "url")]
    pub uri: String,
    pub webpage_url: String,
    pub title: String,
    pub thumbnail: Option<String>,
    #[serde(default, deserialize_with = "duration_deserialize")]
    pub duration: Option<Duration>,
    #[serde(skip)]
    pub added_by: String,
}

fn duration_deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let dur: Option<f64> = Deserialize::deserialize(deserializer)?;

    Ok(dur.map(Duration::from_secs_f64))
}

pub async fn get_audio_download_from_url(
    url: String,
    logger: &Logger,
) -> Result<AudioMetadata, String> {
    //youtube-dl sometimes just fails, so we give it a second try
    let ytdl_output = match run_youtube_dl(&url, &logger).await {
        Ok(o) => o,
        Err(e) => {
            if e.contains("Unable to extract video data") {
                run_youtube_dl(&url, &logger).await?
            } else {
                return Err(e);
            }
        }
    };

    let output = serde_json::from_str(&ytdl_output).map_err(|e| e.to_string())?;

    Ok(output)
}

async fn run_youtube_dl(url: &str, logger: &Logger) -> Result<String, String> {
    let ytdl_args = ["--no-playlist", "-f", "bestaudio/best", "-j", &url];

    let mut cmd = Command::new("youtube-dl");
    cmd.args(&ytdl_args);
    cmd.stdin(Stdio::null());

    debug!(logger, "running yt-dl"; "command" => ?cmd);
    let ytdl_output = cmd.output().await.unwrap();

    if !ytdl_output.status.success() {
        let s = String::from_utf8(ytdl_output.stderr).unwrap();
        return Err(s);
    }

    let output_str = String::from_utf8(ytdl_output.stdout).unwrap();

    Ok(output_str)
}
