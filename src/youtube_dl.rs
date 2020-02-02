use std::time::Duration;

use futures::compat::Future01CompatExt;
use std::process::{Command, Stdio};
use tokio_process::CommandExt;

use serde::{Deserialize, Serialize};

use log::debug;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AudioMetadata {
    pub url: String,
    pub webpage_url: String,
    pub title: String,
    pub thumbnail: Option<String>,
    #[serde(default, deserialize_with = "duration_deserialize")]
    #[serde(serialize_with = "duration_serialize")]
    pub duration: Option<Duration>,
    #[serde(skip)]
    pub added_by: String,
}

fn duration_serialize<S>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match d {
        Some(d) => s.serialize_some(&d.as_secs_f64()),
        None => s.serialize_none(),
    }
}

fn duration_deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let dur: Option<f64> = Deserialize::deserialize(deserializer)?;

    Ok(dur.map(|v| Duration::from_secs_f64(v)))
}

pub async fn get_audio_download_url(uri: String) -> Result<AudioMetadata, String> {
    let ytdl_args = ["--no-playlist", "-f", "bestaudio/best", "-j", &uri];

    let mut cmd = Command::new("youtube-dl");
    cmd.args(&ytdl_args);
    cmd.stdin(Stdio::null());

    debug!("yt-dl command: {:?}", cmd);

    let ytdl_output = cmd.output_async().compat().await.unwrap();

    if ytdl_output.status.success() == false {
        return Err(String::from_utf8(ytdl_output.stderr.clone()).unwrap());
    }

    let output_str = String::from_utf8(ytdl_output.stdout.clone()).unwrap();
    let output = serde_json::from_str(&output_str).map_err(|e| e.to_string())?;

    Ok(output)
}
