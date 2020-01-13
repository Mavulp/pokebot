use std::process::{Command, Stdio};

use log::debug;

pub fn get_audio_download_url(uri: String) -> Result<(String, String), String> {
    let ytdl_args = [
        "--no-playlist",
        "-f",
        "bestaudio/best",
        "-g",
        "--get-filename",
        "-o",
        "%(title)s",
        &uri,
    ];

    let mut cmd = Command::new("youtube-dl");
    cmd.args(&ytdl_args);
    cmd.stdin(Stdio::null());

    debug!("yt-dl command: {:?}", cmd);

    let ytdl_output = cmd.output().unwrap();

    let output = String::from_utf8(ytdl_output.stdout.clone()).unwrap();

    if ytdl_output.status.success() == false {
        return Err(String::from_utf8(ytdl_output.stderr.clone()).unwrap());
    }

    let lines = output.lines().collect::<Vec<_>>();
    let url = lines[0].to_owned();
    let title = lines[1].to_owned();

    Ok((url, title))
}
