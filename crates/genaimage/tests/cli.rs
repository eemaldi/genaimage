use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use genaimage_api::client::Client;
use genaimage_comfyui::ComfyUiClient;
use genaimage_core::{config::ServerConfig, Harness, HarnessConfig};
use genaimage_server::{serve_on_listener, AppState};
use genaimage_test_support::{
    install_contract_workflow, ApiFixture, FakeComfyServer, FakeComfyState,
};

use serde_json::{json, Value};
use tempfile::TempDir;

fn write(path: &Path, value: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, value).expect("write file");
}

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_genaimage"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GENAIMAGE_") {
            command.env_remove(name);
        }
    }
    command
}

struct Server {
    origin: String,
    workflows: PathBuf,
    fake: FakeComfyState,
    state: AppState,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    _workspace: TempDir,
    _backend: FakeComfyServer,
}

impl Server {
    async fn start() -> Self {
        let ApiFixture {
            workspace,
            fake,
            backend,
            listener,
            config_path,
            origin,
        } = ApiFixture::start(5, ServerConfig::default().max_upload_bytes).await;
        let config = HarnessConfig::load_with(
            workspace.path(),
            Some(&config_path),
            workspace.path(),
            |key| (key == "HOME").then(|| workspace.path().display().to_string()),
        )
        .unwrap();
        let workflows = config.workflows.clone();
        let adapter = Arc::new(ComfyUiClient::new(&config.comfyui).unwrap());
        let harness = Arc::new(Harness::new_async(config, adapter).await.unwrap());
        let state = AppState::new(harness);
        let server_state = state.clone();
        let shutdown = state.clone();
        let task = tokio::spawn(async move {
            serve_on_listener(
                server_state,
                listener,
                shutdown.wait_for_shutdown(),
                Duration::from_secs(2),
            )
            .await
        });
        Self {
            origin,
            workflows,
            fake,
            state,
            task,
            _workspace: workspace,
            _backend: backend,
        }
    }

    async fn cli(&self, args: &[&str]) -> Output {
        let cwd = TempDir::new().unwrap();
        let mut command = cli();
        command
            .current_dir(cwd.path())
            .args(["--server", &self.origin])
            .args(args);
        // Ported commands must not read the CLI's local configuration.
        command.env("GENAIMAGE_CONFIG", cwd.path().join("does-not-exist.toml"));
        cli_output(command).await
    }

    async fn shutdown(mut self) {
        self.state.begin_shutdown();
        tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.state.begin_shutdown();
    }
}

async fn cli_output(command: Command) -> Output {
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::from(command)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("CLI timed out")
    .expect("run CLI")
}

fn report(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.split(|byte| *byte == b'\n').count(), 2);
    serde_json::from_slice(&output.stdout).expect("one JSON report on stdout")
}

fn error(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr.split(|byte| *byte == b'\n').count(), 2);
    serde_json::from_slice(&output.stderr).expect("one JSON error on stderr")
}

#[tokio::test]
async fn workflows_are_discovered_inspected_and_validated_by_id() {
    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    write(&server.workflows.join("broken.genaimage.json"), "{");
    write(
        &server.workflows.join("legacy.genaimage.json"),
        r#"{"schema_version":1,"id":"legacy","version":"1.0.0","workflow":"workflow.api.json","required_gates":["gate3"]}"#,
    );
    let client = Client::with_base_url(&server.origin).unwrap();
    let catalogue = report(server.cli(&["--json", "workflow", "list"]).await, 0);
    assert_eq!(
        catalogue,
        serde_json::to_value(client.workflows().await.unwrap()).unwrap()
    );
    assert_eq!(catalogue["workflows"][0]["id"], "contract-workflow");
    assert_eq!(catalogue["diagnostics"].as_array().unwrap().len(), 2);
    assert!(catalogue["diagnostics"][1]["error"]
        .as_str()
        .unwrap()
        .contains("upgrade to version 2"));

    let details = report(
        server
            .cli(&["workflow", "inspect", "contract-workflow", "--json"])
            .await,
        0,
    );
    assert_eq!(
        details,
        serde_json::to_value(client.workflow("contract-workflow").await.unwrap()).unwrap()
    );
    assert_eq!(details["parameters"]["seed"]["node"], "3");
    let presets = report(
        server
            .cli(&["presets", "contract-workflow", "--json"])
            .await,
        0,
    );
    assert_eq!(presets, json!({"fast": {"seed": 42, "steps": 10}}));

    let args = [
        "workflow",
        "validate",
        "contract-workflow",
        "--preset",
        "fast",
        "--set",
        "seed=99",
        "--json",
    ];
    assert_eq!(
        report(server.cli(&args).await, 0),
        json!({
            "valid": true, "parameter_errors": [],
            "node_check": {"status": "available", "missing_classes": [], "reason": null}
        })
    );
    let invalid = report(
        server
            .cli(&[
                "workflow",
                "validate",
                "contract-workflow",
                "--preset",
                "fast",
                "--set",
                "steps=101",
                "--json",
            ])
            .await,
        6,
    );
    assert_eq!(invalid["valid"], false);
    assert_eq!(invalid["parameter_errors"][0]["parameter"], "steps");
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 0);

    // API commands never interpret input as a path, even if it matches a server file.
    let args = ["workflow", "validate", "contract.genaimage.json"];
    let problem = error(server.cli(&[&args[..], &["--json"]].concat()).await, 4);
    assert_eq!(problem["kind"], "server_problem");
    assert_eq!(problem["problem"]["status"], 404);
    let human = server.cli(&args).await;
    let stderr = String::from_utf8(human.stderr).unwrap();
    for key in ["title", "detail", "recovery"] {
        assert!(
            stderr.contains(problem["problem"][key].as_str().unwrap()),
            "{stderr}"
        );
    }
    let list = server.cli(&["workflow", "list"]).await;
    assert!(list.status.success());
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(
        stdout.contains("contract-workflow  Contract workflow (1.0.0)")
            && stdout.contains("Skipped ")
    );
    server.shutdown().await;
}

#[tokio::test]
async fn integer_boundaries_are_enforced_by_the_server() {
    let server = Server::start().await;
    let workflow_dir = &server.workflows;
    write(
        &workflow_dir.join("api.json"),
        r#"{"3":{"class_type":"KSampler","inputs":{"seed":1}}}"#,
    );
    for kind in ["integer", "seed"] {
        for (value, valid) in [
            ("-9223372036854775808", true),
            ("18446744073709551615", true),
            ("-9223372036854775809", false),
            ("18446744073709551616", false),
        ] {
            let manifest = format!(
                r#"{{"schema_version":2,"id":"example","version":"1.0.0","workflow":"api.json","parameters":{{"seed":{{"node":"3","input":"seed","kind":"{kind}","default":{value}}}}}}}"#
            );
            write(&workflow_dir.join("example.genaimage.json"), &manifest);
            let output = server
                .cli(&["--json", "workflow", "validate", "example"])
                .await;
            assert_eq!(
                output.status.success(),
                valid,
                "{kind}/{value}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    server.shutdown().await;
}

#[tokio::test]
async fn doctor_and_config_use_the_server_and_support_origin_precedence() {
    let server = Server::start().await;
    let mut command = cli();
    command
        .env("GENAIMAGE_SERVER", &server.origin)
        .args(["doctor", "--json"]);
    assert_eq!(
        report(cli_output(command).await, 0),
        json!({"self": "ok", "image_backend": "ok", "llm": "not_configured"})
    );

    let mut command = cli();
    command.env("GENAIMAGE_SERVER", "invalid-origin").args([
        "config",
        "show",
        "--json",
        "--server",
        &server.origin,
    ]);
    let settings = report(cli_output(command).await, 0);
    let client = Client::with_base_url(&server.origin).unwrap();
    assert_eq!(
        settings,
        serde_json::to_value(client.settings().await.unwrap()).unwrap()
    );
    assert_eq!(
        settings["settings"]["server.port"]["value"].to_string(),
        server.origin.rsplit(':').next().unwrap()
    );

    server.fake.health_unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        report(server.cli(&["--json", "doctor"]).await, 6),
        json!({
            "self": "ok", "image_backend": "unavailable", "llm": "not_configured", "image_backend_reason": "probe_failed"
        })
    );
    let human = server.cli(&["doctor"]).await;
    assert_eq!(human.status.code(), Some(6));
    assert!(String::from_utf8(human.stdout)
        .unwrap()
        .contains("ComfyUI: unavailable"));
    server.shutdown().await;
}

#[tokio::test]
async fn refused_connections_have_an_actionable_error_and_distinct_exit_code() {
    // Keep an ephemeral port reserved without listening so no other test can take it.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    let mut command = cli();
    command.args(["--server", &origin, "doctor"]);
    let human = cli_output(command).await;
    assert_eq!(human.status.code(), Some(3));
    assert!(human.stdout.is_empty());
    let message = String::from_utf8(human.stderr).unwrap();
    for hint in [&origin, "genaimage serve", "--server", "GENAIMAGE_SERVER"] {
        assert!(message.contains(hint), "{message}");
    }
    let mut command = cli();
    command
        .env("GENAIMAGE_SERVER", &origin)
        .args(["doctor", "--json"]);
    assert_eq!(
        error(cli_output(command).await, 3),
        json!({
            "kind": "server_not_running", "message": message.trim().strip_prefix("Error: ").unwrap()
        })
    );
    let mut command = cli();
    command.args(["--server", "not-an-origin", "--json", "doctor"]);
    assert_eq!(error(cli_output(command).await, 5)["kind"], "client_error");
}

#[test]
fn run_by_id_refuses_to_choose_between_valid_and_legacy_sidecars() {
    let temp = TempDir::new().expect("temp");
    let workflows = temp.path().join("workflows/comfyui");
    write(
        &workflows.join("api.json"),
        r#"{"3":{"class_type":"KSampler","inputs":{"seed":1}}}"#,
    );
    write(
        &workflows.join("valid.genaimage.json"),
        r#"{"schema_version":2,"id":"shared","version":"1.0.0","workflow":"api.json"}"#,
    );
    write(
        &workflows.join("legacy.genaimage.json"),
        r#"{"schema_version":1,"id":"shared","version":"1.0.0","workflow":"api.json","required_gates":["gate3"]}"#,
    );
    let run_cli = |args: &[&str]| {
        cli()
            .current_dir(temp.path())
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .arg("--workspace")
            .arg(temp.path())
            .arg("--json")
            .args(args)
            .output()
            .expect("run CLI")
    };
    for valid_name in ["valid.genaimage.json", "shared.genaimage.json"] {
        let rejected = run_cli(&["run", "shared"]);
        assert!(!rejected.status.success());
        let error: Value = serde_json::from_slice(&rejected.stderr).expect("error JSON");
        let message = error["error"].as_str().expect("error message");
        assert!(message.contains("workflow id 'shared'"), "{message}");
        assert!(message.contains("legacy.genaimage.json"), "{message}");
        assert!(
            message.contains("select an explicit manifest path"),
            "{message}"
        );
        if valid_name == "valid.genaimage.json" {
            fs::rename(
                workflows.join(valid_name),
                workflows.join("shared.genaimage.json"),
            )
            .expect("move valid sidecar to the conventional filename");
        }
    }
}

#[test]
fn legacy_keys_are_rejected_with_migration_guidance() {
    let temp = TempDir::new().expect("temp");
    write(
        &temp.path().join("genaimage.toml"),
        "[paths]\ncharacters = \"characters\"\n",
    );
    let output = cli()
        .current_dir(temp.path())
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "jobs", "list"])
        .output()
        .expect("run CLI");
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    let message = error["error"].as_str().expect("message");
    assert!(message.contains("characters") && message.contains("migrate"));
}

#[test]
fn local_jobs_do_not_require_a_backend_or_server_connection() {
    let temp = TempDir::new().unwrap();
    write(&temp.path().join("genaimage.toml"), "[comfyui]\nbase_url = 'http://missing.invalid:8188'\nallowed_hosts = ['missing.invalid']\nrequest_timeout_seconds = 1\n");
    let output = cli()
        .current_dir(temp.path())
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "jobs", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_wait_preserves_json_and_human_progress() {
    use serde_json::json;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        process::Stdio,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let stopping = Arc::new(AtomicBool::new(false));
    let server = {
        let stopping = Arc::clone(&stopping);
        thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fake server accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (header_end, content_length) = loop {
                    let mut chunk = [0; 4096];
                    let n = stream.read(&mut chunk).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while bytes.len() < header_end + content_length {
                    let mut chunk = [0; 4096];
                    let n = stream.read(&mut chunk).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let header = String::from_utf8_lossy(&bytes[..header_end]);
                let path = header
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let (status, body) = if path == "/object_info" {
                    ("200 OK", json!({"KSampler": {}}))
                } else if path == "/prompt" {
                    let request: Value =
                        serde_json::from_slice(&bytes[header_end..header_end + content_length])
                            .unwrap();
                    (
                        "200 OK",
                        json!({"prompt_id": request["prompt_id"], "node_errors": {}}),
                    )
                } else if path == "/queue" {
                    ("200 OK", json!({"queue_running": [], "queue_pending": []}))
                } else if let Some(prompt_id) = path.strip_prefix("/history/") {
                    (
                        "200 OK",
                        json!({prompt_id: {"status": {"status_str": "success", "completed": true}, "outputs": {}}}),
                    )
                } else {
                    ("400 Bad Request", json!({}))
                };
                let body = body.to_string();
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        })
    };
    let temp = TempDir::new().unwrap();
    write(
        &temp.path().join("genaimage.toml"),
        &format!("[comfyui]\nbase_url = \"http://{address}\"\n"),
    );
    let workflows = temp.path().join("workflows/comfyui");
    write(
        &workflows.join("api.json"),
        r#"{"3":{"class_type":"KSampler","inputs":{"seed":1}}}"#,
    );
    write(
        &workflows.join("test.genaimage.json"),
        r#"{"schema_version":2,"id":"test","version":"1.0.0","workflow":"api.json"}"#,
    );

    let test_cli = || {
        let mut command = cli();
        command
            .current_dir(temp.path())
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .arg("--config")
            .arg(temp.path().join("genaimage.toml"));
        command
    };
    for json_mode in [true, false] {
        let mut command = test_cli();
        command.arg("--workspace").arg(temp.path());
        if json_mode {
            command.arg("--json");
        }
        let output = command.args(["run", "test", "--wait"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let records: Vec<Value> = serde_json::Deserializer::from_slice(&output.stdout)
            .into_iter::<Value>()
            .map(|value| value.unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["status"], "queued");
        assert_eq!(records[1]["status"], "completed");
        assert_eq!(records[0]["id"], records[1]["id"]);
        if json_mode {
            assert!(output.stderr.is_empty());
            assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 2);
        } else {
            let progress = String::from_utf8_lossy(&output.stderr);
            let terminal = format!("{}: completed\n", records[0]["id"].as_str().unwrap());
            assert_eq!(progress.matches(&terminal).count(), 1, "{progress}");
            assert!(progress.ends_with(&terminal), "{progress}");
        }
    }
    // Reconcile from a separate CLI process before the waiter's first poll.
    let mut waiter = test_cli()
        .arg("--workspace")
        .arg(temp.path())
        .args(["run", "test", "--wait"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(waiter.stdout.take().unwrap());
    let mut queued_json = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(stdout.read_line(&mut line).unwrap(), 0);
        queued_json.push_str(&line);
        if line == "}\n" {
            break;
        }
    }
    let queued: Value = serde_json::from_str(&queued_json).unwrap();
    assert_eq!(queued["status"], "queued");
    let run_id = queued["id"].as_str().unwrap();
    let other = test_cli()
        .arg("--workspace")
        .arg(temp.path())
        .arg("--json")
        .args(["jobs", "reconcile", run_id])
        .output()
        .unwrap();
    assert!(
        other.status.success(),
        "{}",
        String::from_utf8_lossy(&other.stderr)
    );
    let reconciliation: Value = serde_json::from_slice(&other.stdout).unwrap();
    assert_eq!(reconciliation["run"]["status"], "completed");
    let result = waiter.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut final_json = String::new();
    stdout.read_to_string(&mut final_json).unwrap();
    let completed: Value = serde_json::from_str(&final_json).unwrap();
    assert_eq!(completed["status"], "completed");
    let progress = String::from_utf8_lossy(&result.stderr);
    let terminal = format!("{run_id}: completed\n");
    assert_eq!(progress.matches(&terminal).count(), 1, "{progress}");
    assert!(progress.ends_with(&terminal), "{progress}");
    stopping.store(true, Ordering::SeqCst);
    server.join().unwrap();
}
