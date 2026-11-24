use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::net::TcpListener;

use crate::{FakeComfyServer, FakeComfyState};

/// Shared API fixture setup. Callers construct the real harness and serve the
/// listener themselves so test support does not depend on production crates.
pub struct ApiFixture {
    pub workspace: TempDir,
    pub fake: FakeComfyState,
    pub backend: FakeComfyServer,
    pub listener: TcpListener,
    pub config_path: PathBuf,
    pub origin: String,
}

impl ApiFixture {
    pub async fn start(seconds: u64, max_upload_bytes: usize) -> Self {
        let workspace = TempDir::new().expect("temporary workspace");
        let fake = FakeComfyState::default();
        let backend = FakeComfyServer::start(fake.clone()).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind API");
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let config_path = workspace.path().join("genaimage.toml");
        std::fs::write(
            &config_path,
            format!(
                "# API fixture configuration\n[server]\nport = {} # listener\nmax_upload_bytes = {}\n\
                 [comfyui]\nbase_url = \"http://{}\"\nrequest_timeout_seconds = {}\n\
                 websocket_reconnect_seconds = 1\n\
                 [paths]\nstate_database = \"state.sqlite3\"\nworkflows = \"workflows\"\n\
                 runs = \"runs\"\nuploads = \"uploads\"\n",
                address.port(), max_upload_bytes, backend.address(), seconds
            ),
        )
        .unwrap();
        Self {
            workspace,
            fake,
            backend,
            listener,
            config_path,
            origin,
        }
    }
}

pub fn install_contract_workflow(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("workflow.api.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.api.json"),
    )
    .unwrap();
    std::fs::write(
        dir.join("contract.genaimage.json"),
        include_str!("../../genaimage-core/tests/fixtures/contract-workflow.genaimage.json"),
    )
    .unwrap();
}
