use std::time::Duration;

use std::process::Stdio;
use tokio::process::Command;

use serde::{Deserialize, Serialize};

use tracing::{debug, Span};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AudioMetadata {
    #[serde(rename = "url")]
    pub uri: String,
    pub webpage_url: Option<String>,
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
    span: &Span,
) -> Result<AudioMetadata, String> {
    //youtube-dl sometimes just fails, so we give it a second try
    let ytdl_output = match run_youtube_dl(&url, span).await {
        Ok(o) => o,
        Err(e) => {
            if e.contains("Unable to extract video data") {
                run_youtube_dl(&url, span).await?
            } else {
                return Err(e);
            }
        }
    };

    let output = serde_json::from_str(&ytdl_output).map_err(|e| e.to_string())?;

    Ok(output)
}

async fn run_youtube_dl(url: &str, span: &Span) -> Result<String, String> {
    let ytdl_args = ["--no-playlist", "-f", "bestaudio/best", "-j", url];

    let mut command = Command::new("yt-dlp");
    command.args(ytdl_args);
    command.stdin(Stdio::null());

    debug!(parent: span, ?command, "running yt-dlp");
    let ytdl_output = command.output().await.unwrap();

    if !ytdl_output.status.success() {
        let s = String::from_utf8(ytdl_output.stderr).unwrap();
        return Err(s);
    }

    let output_str = String::from_utf8(ytdl_output.stdout).unwrap();

    Ok(output_str)
}
