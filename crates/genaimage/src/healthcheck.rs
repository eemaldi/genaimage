use std::{process::ExitCode, time::Duration};

use genaimage_api::client::Client;
use tokio::time::timeout;

use crate::Cli;

const TOTAL_TIMEOUT: Duration = Duration::from_secs(2);

/// Succeeds when the server answers `/api/v1/health`; ComfyUI and LLM status do not matter.
pub(crate) async fn execute(cli: &Cli) -> ExitCode {
    let Ok(client) = Client::with_base_url(&cli.server_url()) else {
        return ExitCode::FAILURE;
    };
    match timeout(TOTAL_TIMEOUT, client.health()).await {
        Ok(Ok(_)) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
