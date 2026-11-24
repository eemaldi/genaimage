use std::{io, process::Stdio, time::Duration};

use genaimage_api::client::Client;
use serde::Serialize;
use tokio::{
    process::Command,
    time::{sleep, Instant},
};

use crate::output::{Error, Output, Status};

const LAUNCHER_EXIT_TIMEOUT: Duration = Duration::from_millis(500);
const LAUNCHER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Serialize)]
struct OpenResponse {
    url: String,
}

pub(crate) async fn run(client: &Client, output: Output, print: bool) -> Result<Status, Error> {
    client.health().await?;

    let url = client.base_url().as_str().to_owned();
    if !print {
        spawn_browser(&url).await.map_err(|error| {
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

async fn spawn_browser(url: &str) -> io::Result<()> {
    let mut command = browser_command(url)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + LAUNCHER_EXIT_TIMEOUT;

    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "browser launcher exited with status {status}"
                )))
            };
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        sleep(LAUNCHER_POLL_INTERVAL).await;
    }
}

fn browser_command(url: &str) -> io::Result<Command> {
    if let Some(browser) = std::env::var_os("BROWSER").filter(|browser| !browser.is_empty()) {
        let mut command = Command::new(browser);
        command.arg(url);
        return Ok(command);
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        Ok(command)
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(url);
        Ok(command)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        Ok(command)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = url;
        Err(io::Error::other(
            "no default-browser launcher is available on this platform",
        ))
    }
}
