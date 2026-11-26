use std::process::ExitCode;

use genaimage_api::client::Client;

use crate::Cli;

/// Succeeds when the server answers `/api/v1/health`; ComfyUI and LLM status do not matter.
pub(crate) async fn execute(cli: &Cli) -> ExitCode {
    let Ok(client) = Client::with_base_url(&cli.server) else {
        return ExitCode::FAILURE;
    };
    match client.health().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
