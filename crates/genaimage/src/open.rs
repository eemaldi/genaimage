use std::io;
use std::process::Command;

use genaimage_api::client::Client;
use serde::Serialize;

use crate::output::{Error, Output, Status};

#[derive(Serialize)]
struct OpenResponse {
    url: String,
}

pub(crate) async fn run(
    client: &Client,
    output: Output,
    base_url: &str,
    print: bool,
) -> Result<Status, Error> {
    client.health().await?;

    let url = format!("{}/", base_url.trim_end_matches('/'));
    if !print {
        spawn_browser(&url).map_err(|error| {
            Error::Browser(format!(
                "Could not open {url} in a browser: {error}. Open it manually, or use `genaimage open --print`."
            ))
        })?;
    }

    let response = OpenResponse { url: url.clone() };
    output.write(&response, |out| {
        if print {
            writeln!(out, "{url}")
        } else {
            writeln!(out, "Opening {url} in your default browser.")
        }
    })?;
    Ok(Status::Success)
}

#[cfg(target_os = "windows")]
fn spawn_browser(url: &str) -> io::Result<()> {
    Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn spawn_browser(url: &str) -> io::Result<()> {
    Command::new("open").arg(url).spawn().map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_browser(url: &str) -> io::Result<()> {
    Command::new("xdg-open").arg(url).spawn().map(|_| ())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
fn spawn_browser(_url: &str) -> io::Result<()> {
    Err(io::Error::other(
        "no default-browser launcher is available on this platform",
    ))
}
