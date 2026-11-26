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

#[cfg(target_os = "linux")]
fn use_fake_xdg_open(command: &mut Command, mode: &str) {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-bin");
    let mut path_entries = vec![fixture_dir];
    if let Some(path) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&path));
    }
    command
        .env("PATH", std::env::join_paths(path_entries).unwrap())
        .env("GENAIMAGE_FAKE_XDG_OPEN_MODE", mode);
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

fn doctor_checks(report: &Value) -> std::collections::BTreeMap<&str, &Value> {
    report["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .map(|check| (check["id"].as_str().expect("check id"), check))
        .collect()
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
async fn workflow_inspect_json_preserves_unknown_declarations_from_old_server() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let response = json!({
        "schema_version": 2, "id": "old", "version": "1", "title": "Old",
        "workflow": "api.json", "parameters": {
            "seed": {"node":"1", "input":"seed", "kind":"seed", "required":false, "description":""}
        }, "presets": {}, "outputs": [{"node":"2", "kind":"image"}]
    })
    .to_string();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let count = stream.read(&mut request).await.unwrap();
        assert!(
            String::from_utf8_lossy(&request[..count]).starts_with("GET /api/v1/workflows/old ")
        );
        let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    let mut command = cli();
    command.args(["--server", &origin, "--json", "workflow", "inspect", "old"]);
    let output = report(cli_output(command).await, 0);
    server.await.unwrap();
    assert!(output["parameters"]["seed"]
        .get("declared_required")
        .is_none());
    assert!(output["outputs"][0].get("declared_kind").is_none());
}

#[tokio::test]
async fn workflow_list_reports_reserved_dot_ids_with_recovery() {
    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    let source = server.workflows.join("contract.genaimage.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&source).unwrap()).unwrap();
    for (name, id) in [("dot", "."), ("dotdot", "..")] {
        manifest["id"] = json!(id);
        fs::write(
            server.workflows.join(format!("{name}.genaimage.json")),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }
    let output = report(server.cli(&["--json", "workflow", "list"]).await, 0);
    assert_eq!(output["workflows"].as_array().unwrap().len(), 1);
    let diagnostics = output["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2);
    for diagnostic in diagnostics {
        assert!(diagnostic["error"]
            .as_str()
            .unwrap()
            .contains("choose a different ID"));
    }
    server.shutdown().await;
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
            "revision": details["revision"],
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
    let healthy = report(cli_output(command).await, 0);
    let checks = doctor_checks(&healthy);
    assert_eq!(
        checks
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        genaimage_api::DOCTOR_CHECK_IDS.into_iter().collect()
    );
    for id in ["server", "comfyui.reachable", "comfyui.object_info", "gpu"] {
        assert_eq!(checks[id]["status"], "ok", "{id}: {}", checks[id]);
        assert!(checks[id]["fix"].is_null(), "{id}");
    }
    for id in ["llm.reachable", "llm.text", "llm.vision"] {
        assert_eq!(checks[id]["status"], "skipped", "{id}");
        assert!(checks[id]["fix"].as_str().unwrap().contains("llm.base_url"));
    }
    assert!(
        healthy.get("self").is_none(),
        "the old HealthResponse shape is gone"
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
    let failing = report(server.cli(&["--json", "doctor"]).await, 6);
    let checks = doctor_checks(&failing);
    assert_eq!(checks["comfyui.reachable"]["status"], "error");
    assert!(checks["comfyui.reachable"]["fix"]
        .as_str()
        .unwrap()
        .contains("comfyui.base_url"));
    assert_eq!(checks["comfyui.object_info"]["status"], "skipped");
    let human = server.cli(&["doctor"]).await;
    assert_eq!(human.status.code(), Some(6));
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("[ERROR  ] comfyui.reachable"), "{text}");
    assert!(text.contains("Fix: Start ComfyUI"), "{text}");
    assert!(text.contains("Errors: 1, warnings:"), "{text}");
    server.shutdown().await;
}

#[tokio::test]
async fn shell_completions_are_generated_without_server_or_configuration() {
    let directory = TempDir::new().unwrap();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let mut command = cli();
        command
            .current_dir(directory.path())
            .env("GENAIMAGE_SERVER", "not-a-loopback-url")
            .env("GENAIMAGE_CONFIG", directory.path().join("missing.toml"))
            .args(["--config", "missing-config.toml", "completions", shell]);
        let output = cli_output(command).await;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        assert!(
            !String::from_utf8(output.stdout).unwrap().trim().is_empty(),
            "empty {shell} completion script"
        );
    }
}

#[tokio::test]
async fn open_prints_the_server_root_after_a_health_check() {
    let server = Server::start().await;
    let expected = format!("{}/", server.origin.trim_end_matches('/'));

    let output = server.cli(&["open", "--print"]).await;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{expected}\n")
    );

    assert_eq!(
        report(server.cli(&["--json", "open", "--print"]).await, 0),
        json!({"url": expected})
    );
    server.shutdown().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn open_delegates_browser_specs_and_silences_launcher_output() {
    let server = Server::start().await;
    let expected = format!("{}/", server.origin.trim_end_matches('/'));
    let unnormalised = format!("{}/&calc&x/../", server.origin);
    for browser in ["false:echo", "echo %s"] {
        let mut command = cli();
        command.env("BROWSER", browser);
        use_fake_xdg_open(&mut command, "success");
        command.args(["--server", &unnormalised, "--json", "open"]);

        let result = report(cli_output(command).await, 0);
        assert_eq!(result, json!({"url": expected}));
    }
    server.shutdown().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn open_reports_a_failed_launcher_with_a_print_hint() {
    let server = Server::start().await;
    let mut command = cli();
    use_fake_xdg_open(&mut command, "fail");
    command.args(["--server", &server.origin, "--json", "open"]);

    let result = error(cli_output(command).await, 1);
    assert_eq!(result["kind"], "browser_error");
    assert!(result["message"].as_str().unwrap().contains("--print"));
    server.shutdown().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn open_does_not_wait_for_a_slow_launcher() {
    let server = Server::start().await;
    let mut command = cli();
    use_fake_xdg_open(&mut command, "slow");
    command.args(["--server", &server.origin, "--json", "open"]);

    let started = std::time::Instant::now();
    assert_eq!(
        report(cli_output(command).await, 0)["url"],
        format!("{}/", server.origin.trim_end_matches('/'))
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    server.shutdown().await;
}

#[tokio::test]
async fn open_rejects_non_loopback_servers() {
    let mut command = cli();
    command.args([
        "--server",
        "http://example.com:3000",
        "--json",
        "open",
        "--print",
    ]);
    let result = error(cli_output(command).await, 5);
    assert_eq!(result["kind"], "client_error");
    assert!(result["message"].as_str().unwrap().contains("loopback"));
}

#[tokio::test]
async fn open_reports_the_existing_server_not_running_error() {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    let mut command = cli();
    command.args(["--server", &origin, "--json", "open", "--print"]);
    let result = error(cli_output(command).await, 3);
    assert_eq!(result["kind"], "server_not_running");
    for hint in [&origin, "genaimage serve", "--server", "GENAIMAGE_SERVER"] {
        assert!(result["message"].as_str().unwrap().contains(hint));
    }
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
    assert!(human.stderr.is_empty());
    let message = String::from_utf8(human.stdout).unwrap();
    for hint in [
        &origin,
        "genaimage serve",
        "--server",
        "GENAIMAGE_SERVER",
        "[ERROR  ] server",
    ] {
        assert!(message.contains(hint), "{message}");
    }
    let mut command = cli();
    command
        .env("GENAIMAGE_SERVER", &origin)
        .args(["doctor", "--json"]);
    let offline = report(cli_output(command).await, 3);
    let checks = doctor_checks(&offline);
    assert_eq!(checks.len(), genaimage_api::DOCTOR_CHECK_IDS.len());
    assert_eq!(checks["server"]["status"], "error");
    assert!(checks["server"]["fix"]
        .as_str()
        .unwrap()
        .contains("genaimage serve"));
    assert_eq!(checks["ports"]["status"], "ok");
    for (id, check) in &checks {
        if !matches!(*id, "server" | "ports") {
            assert_eq!(check["status"], "skipped", "{id}");
            assert!(!check["fix"].as_str().unwrap().is_empty(), "{id}");
        }
    }
    let mut command = cli();
    command.args(["--server", "not-an-origin", "--json", "doctor"]);
    assert_eq!(error(cli_output(command).await, 5)["kind"], "client_error");
}

#[tokio::test]
async fn doctor_reports_a_foreign_process_on_the_server_port() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer).await;
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        }
    });
    let mut command = cli();
    command.args([
        "--server",
        &format!("http://127.0.0.1:{port}"),
        "doctor",
        "--json",
    ]);
    let result = report(cli_output(command).await, 6);
    let checks = doctor_checks(&result);
    assert_eq!(checks["server"]["status"], "error");
    assert!(checks["server"]["summary"]
        .as_str()
        .unwrap()
        .contains("not a GenAImage server"));
    assert_eq!(checks["ports"]["status"], "error");
    let fix = checks["ports"]["fix"].as_str().unwrap();
    assert!(
        fix.contains(&port.to_string()) && fix.contains("server.port"),
        "{fix}"
    );
    task.abort();
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
        .args(["--json", "serve"])
        .output()
        .expect("run CLI");
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    let message = error["error"].as_str().expect("message");
    assert!(message.contains("characters") && message.contains("migrate"));
}

#[tokio::test]
async fn run_uploads_explicit_images_and_jobs_show_persisted_details() {
    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    let manifest = server.workflows.join("contract.genaimage.json");
    let mut body: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    body["parameters"]["reference"] =
        json!({"node":"12", "input":"image", "kind":"image_upload", "required":true});
    write(&manifest, &body.to_string());
    let graph = server.workflows.join("workflow.api.json");
    let mut body: Value = serde_json::from_slice(&fs::read(&graph).unwrap()).unwrap();
    body["12"] = json!({"class_type":"LoadImage", "inputs":{"image":""}});
    write(&graph, &body.to_string());
    let image = server._workspace.path().join("client-reference.png");
    let bytes = include_bytes!("../../genaimage-server/tests/fixtures/reference.png");
    fs::write(&image, bytes).unwrap();
    let reference = format!("reference={}", image.display());
    let invalid = server
        .cli(&[
            "--json",
            "run",
            "contract-workflow",
            "--preset",
            "fast",
            "--image",
            &reference,
            "--set",
            "reference=already-specified",
        ])
        .await;
    assert_eq!(error(invalid, 1)["kind"], "input_error");
    let run = report(
        server
            .cli(&[
                "--json",
                "run",
                "contract-workflow",
                "--preset",
                "fast",
                "--image",
                &reference,
                "--set",
                "seed=99",
            ])
            .await,
        0,
    );
    let id = run["id"].as_str().unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["parameters"]["seed"], 99);
    let client = Client::with_base_url(&server.origin).unwrap();
    let upload_id = run["parameters"]["reference"].as_str().unwrap();
    assert_eq!(
        client
            .upload_metadata(upload_id)
            .await
            .unwrap()
            .original_filename,
        "client-reference.png"
    );
    assert_eq!(
        server.fake.uploaded_image.lock().unwrap().as_deref(),
        Some(bytes.as_slice())
    );

    server.fake.complete.store(true, Ordering::SeqCst);
    let finished = report(server.cli(&["--json", "jobs", "wait", id]).await, 0);
    assert_eq!(finished["status"], "completed");
    assert_eq!(
        report(
            server
                .cli(&["--json", "jobs", "list", "--limit", "1"])
                .await,
            0
        ),
        json!({"runs":[finished], "next_cursor":null})
    );
    let details = report(
        server
            .cli(&["--json", "jobs", "show", id, "--events", "1"])
            .await,
        0,
    );
    assert_eq!(details["run"], finished);
    assert_eq!(details["events"].as_array().unwrap().len(), 1);
    assert_eq!(details["outputs"][0]["run_id"], id);
    assert_eq!(details["next_output_cursor"], Value::Null);

    let destination = TempDir::new().unwrap();
    let dir = destination.path().to_str().unwrap();
    let args = ["--json", "jobs", "download", id, "--dir", dir];
    let saved = report(server.cli(&args).await, 0);
    let path = Path::new(saved["outputs"][0]["path"].as_str().unwrap());
    assert_eq!(saved["outputs"][0]["id"], details["outputs"][0]["id"]);
    assert_eq!(fs::read(path).unwrap(), b"not-a-real-png-but-deterministic");
    fs::write(path, b"keep me").unwrap();
    assert_eq!(error(server.cli(&args).await, 1)["kind"], "file_error");
    assert_eq!(fs::read(path).unwrap(), b"keep me");

    let manifest = report(server.cli(&["--json", "jobs", "manifest", id]).await, 0);
    assert_eq!(manifest["run_id"], id);
    assert_eq!(
        manifest["outputs"][0]["sha256"],
        details["outputs"][0]["sha256"]
    );
    assert_eq!(manifest["source_inputs"][0]["upload_id"], upload_id);
    assert!(manifest["workflow"].get("manifest_path").is_none());
    assert!(manifest["outputs"][0].get("local_path").is_none());
    assert!(manifest["source_inputs"][0].get("source_path").is_none());
    let path = destination.path().join("manifest.json");
    let args = [
        "--json",
        "jobs",
        "manifest",
        id,
        "--out",
        path.to_str().unwrap(),
    ];
    assert_eq!(
        report(server.cli(&args).await, 0),
        json!({"run_id": id, "path": path})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        manifest
    );
    fs::write(&path, b"keep manifest").unwrap();
    assert_eq!(error(server.cli(&args).await, 1)["kind"], "file_error");
    assert_eq!(fs::read(&path).unwrap(), b"keep manifest");
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn run_wait_streams_progress_and_polls_after_resync() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    let mut child = tokio::process::Command::from(cli())
        .args([
            "--server",
            &server.origin,
            "run",
            "contract-workflow",
            "--preset",
            "fast",
            "--wait",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    server
        .fake
        .wait_for_progress_listener(Duration::from_secs(5))
        .await
        .unwrap();
    let prompt = server.fake.prompt_id.lock().unwrap().clone().unwrap();
    let progress = json!({"type":"progress", "data":{"prompt_id":prompt, "value":2, "max":10}});
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                _ = tick.tick() => { let _ = server.fake.progress.send(progress.clone()); }
                line = lines.next_line() => if line.unwrap().expect("progress line").contains("progress 2/10") { break; }
            }
        }
    }).await.expect("SSE progress on stderr");
    server.state.harness().events().invalidate();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !lines
            .next_line()
            .await
            .unwrap()
            .expect("fallback line")
            .contains("live updates interrupted")
        {}
    })
    .await
    .expect("resync triggers status polling");
    server.fake.complete.store(true, Ordering::SeqCst);
    let saved = tokio::time::timeout(Duration::from_secs(7), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(saved.status.success());
    assert!(String::from_utf8_lossy(&saved.stdout).contains("contract-workflow  completed"));
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn run_wait_json_reports_terminal_success_or_failure_once() {
    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    for succeeds in [true, false] {
        server.fake.complete.store(false, Ordering::SeqCst);
        let calls = server.fake.prompt_calls.load(Ordering::SeqCst);
        let run = server.cli(&[
            "run",
            "contract-workflow",
            "--preset",
            "fast",
            "--wait",
            "--json",
        ]);
        let finish =
            async {
                let prompt = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if server.fake.prompt_calls.load(Ordering::SeqCst) > calls {
                            if let Some(id) = server.fake.prompt_id.lock().unwrap().clone() {
                                break id;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                if succeeds {
                    server.fake.complete.store(true, Ordering::SeqCst);
                } else {
                    server.fake.history_overrides.lock().unwrap().insert(prompt.clone(), json!({
                    prompt: {"status":{"status_str":"error", "completed":false, "messages":[]}}
                }));
                }
            };
        let (output, ()) = tokio::join!(run, finish);
        let result = report(output, if succeeds { 0 } else { 7 });
        assert_eq!(
            result["status"],
            if succeeds { "completed" } else { "failed" }
        );
        let client = Client::with_base_url(&server.origin).unwrap();
        assert_eq!(
            result,
            serde_json::to_value(client.run(result["id"].as_str().unwrap()).await.unwrap())
                .unwrap()
        );
        assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), calls + 1);
        *server.fake.prompt_id.lock().unwrap() = None;
    }
    server.shutdown().await;
}

#[tokio::test]
async fn ambiguous_submission_is_not_replayed_and_jobs_can_reconcile_and_cancel() {
    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    server
        .fake
        .fail_submit_response
        .store(true, Ordering::SeqCst);
    let uncertain = error(
        server
            .cli(&[
                "--json",
                "run",
                "contract-workflow",
                "--preset",
                "fast",
                "--wait",
            ])
            .await,
        8,
    );
    assert_eq!(uncertain["kind"], "unknown_submission");
    assert_eq!(uncertain["run"]["status"], "unknown_submission");
    let id = uncertain["run"]["id"].as_str().unwrap();
    assert!(uncertain["message"]
        .as_str()
        .unwrap()
        .contains(&format!("jobs reconcile {id}")));
    assert_eq!(
        report(server.cli(&["--json", "jobs", "reconcile", id]).await, 0)["status"],
        "queued"
    );
    let bulk = report(server.cli(&["--json", "jobs", "reconcile"]).await, 0);
    assert_eq!(bulk["runs"][0]["id"], id);
    assert_eq!(bulk["runs"][0]["outcome"], "active");
    server.fake.running.store(true, Ordering::SeqCst);
    let cancelled = report(server.cli(&["--json", "jobs", "cancel", id]).await, 0);
    assert!(matches!(
        cancelled["status"].as_str(),
        Some("cancelling" | "cancelled")
    ));
    let cancelled = report(server.cli(&["--json", "jobs", "reconcile", id]).await, 0);
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["outcome"], "terminal");
    assert_eq!(
        report(server.cli(&["--json", "jobs", "wait", id]).await, 7)["status"],
        "cancelled"
    );
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn failed_submission_reports_a_durable_run_without_waiting() {
    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    let runs = server.state.harness().config().runs.clone();
    write(&runs.join("contract-workflow"), "not a directory");
    let failure = error(
        server
            .cli(&["--json", "run", "contract-workflow", "--preset", "fast"])
            .await,
        7,
    );
    assert_eq!(failure["kind"], "failed_submission");
    assert_eq!(failure["run"]["status"], "failed");
    assert!(failure["message"]
        .as_str()
        .unwrap()
        .contains("jobs reconcile"));
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn server_commands_reject_explicit_local_selectors() {
    let server = Server::start().await;
    for args in [
        &["--json", "--workspace", ".", "run", "contract-workflow"][..],
        &["--json", "config", "show", "--config", "genaimage.toml"][..],
    ] {
        let result = error(server.cli(args).await, 1);
        assert_eq!(result["kind"], "input_error");
        let message = result["message"].as_str().unwrap();
        assert!(message.contains("genaimage serve") && message.contains("--server"));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn config_init_is_local_and_preserves_its_output_and_overwrite_rules() {
    let workspace = TempDir::new().unwrap();
    let destination = workspace.path().join("example.toml");
    let args = [
        "--json",
        "--config",
        destination.to_str().unwrap(),
        "config",
        "init",
    ];
    let mut command = cli();
    command.current_dir(workspace.path()).args(args);
    assert_eq!(
        report(cli_output(command).await, 0),
        json!({"path": destination, "created": true})
    );
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        HarnessConfig::example()
    );

    fs::write(&destination, "keep me").unwrap();
    let mut command = cli();
    command.current_dir(workspace.path()).args(args);
    let failure = error(cli_output(command).await, 1);
    assert!(failure["error"]
        .as_str()
        .unwrap()
        .contains("already exists; use --force"));
    assert_eq!(fs::read_to_string(&destination).unwrap(), "keep me");

    let mut command = cli();
    command
        .current_dir(workspace.path())
        .args(args)
        .arg("--force");
    assert_eq!(report(cli_output(command).await, 0)["created"], true);
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        HarnessConfig::example()
    );

    let mut command = cli();
    command.current_dir(workspace.path()).args([
        "--json",
        "--workspace",
        ".",
        "config",
        "init",
        "--force",
    ]);
    let failure = error(cli_output(command).await, 1);
    assert!(failure["error"]
        .as_str()
        .unwrap()
        .contains("--workspace is only supported"));
    assert!(!workspace.path().join("genaimage.toml").exists());
}

#[tokio::test]
async fn client_commands_do_not_fall_back_to_local_workspace_when_server_stops() {
    let mut server = Server::start().await;
    install_contract_workflow(&server.workflows);
    server.state.begin_shutdown();
    tokio::time::timeout(Duration::from_secs(5), &mut server.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    for args in [
        &["workflow", "list"][..],
        &["workflow", "inspect", "contract-workflow"][..],
        &["run", "contract-workflow", "--preset", "fast"][..],
        &["jobs", "list"][..],
    ] {
        let mut command = cli();
        command
            .current_dir(server._workspace.path())
            .args(["--json", "--server", &server.origin])
            .args(args);
        assert_eq!(
            error(cli_output(command).await, 3)["kind"],
            "server_not_running"
        );
    }
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn server_commands_reject_legacy_selector_environment_variables() {
    let server = Server::start().await;
    let config = server._workspace.path().join("elsewhere.toml");
    write(&config, "[server]\nport = 4321\n");
    let expected = error(
        server
            .cli(&[
                "--json",
                "config",
                "show",
                "--config",
                config.to_str().unwrap(),
            ])
            .await,
        1,
    );
    for value in [config.to_str().unwrap(), ""] {
        let mut command = cli();
        command.env("GENAIMAGE_CONFIG", value).args([
            "--server",
            &server.origin,
            "config",
            "show",
            "--json",
        ]);
        assert_eq!(error(cli_output(command).await, 1), expected);
    }
    let mut command = cli();
    command.env("GENAIMAGE_PROFILE", "other").args([
        "--server",
        &server.origin,
        "config",
        "show",
        "--json",
    ]);
    let result = error(cli_output(command).await, 1);
    assert_eq!(result["kind"], "input_error");
    assert!(result["message"]
        .as_str()
        .unwrap()
        .contains("migrate the selected profile"));
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_bindings_fail_before_upload_or_submission() {
    let server = Server::start().await;
    for (bindings, expected) in [
        (
            &["--set", "prompt=\"unterminated"][..],
            "Invalid JSON for parameter 'prompt'",
        ),
        (
            &["--set", "seed=42", "--set", "seed=43"][..],
            "'seed' was specified more than once",
        ),
    ] {
        let mut args = vec![
            "--json",
            "run",
            "not-installed",
            "--image",
            "reference=missing.png",
        ];
        args.extend_from_slice(bindings);
        let result = error(server.cli(&args).await, 1);
        assert_eq!(result["kind"], "input_error");
        assert!(
            result["message"].as_str().unwrap().contains(expected),
            "{result}"
        );
        let mut args = vec!["--json", "workflow", "validate", "not-installed"];
        args.extend_from_slice(bindings);
        assert!(error(server.cli(&args).await, 1)["message"]
            .as_str()
            .unwrap()
            .contains(expected));
    }
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert_eq!(server.fake.upload_calls.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn wait_times_out_without_mutating_a_stalled_run() {
    let server = Server::start().await;
    install_contract_workflow(&server.workflows);
    let result = error(
        server
            .cli(&[
                "--json",
                "run",
                "contract-workflow",
                "--preset",
                "fast",
                "--wait",
                "--timeout",
                "1",
            ])
            .await,
        9,
    );
    let id = result["run_id"].as_str().unwrap();
    assert_eq!(result["kind"], "wait_timeout");
    assert!(result["message"]
        .as_str()
        .unwrap()
        .contains(&format!("jobs reconcile {id}")));
    let result = error(
        server
            .cli(&["--json", "jobs", "wait", id, "--timeout", "1"])
            .await,
        9,
    );
    assert_eq!(result["run_id"], id);
    let client = Client::with_base_url(&server.origin).unwrap();
    assert_eq!(
        client.run(id).await.unwrap().status,
        genaimage_api::RunStatus::Queued
    );
    assert_eq!(server.fake.prompt_calls.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn an_unanswered_status_request_has_its_own_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut command = cli();
    command.args([
        "--server",
        &format!("http://{}", listener.local_addr().unwrap()),
        "--json",
        "jobs",
        "wait",
        "test-run",
        "--timeout",
        "60",
    ]);
    let (result, ()) = tokio::join!(cli_output(command), async {
        let (connection, _) = listener.accept().await.unwrap();
        // Hold the real HTTP connection open without ever sending a response.
        tokio::time::timeout(Duration::from_secs(8), async {
            use tokio::io::AsyncReadExt;
            let mut connection = connection;
            let _ = connection.read_to_end(&mut Vec::new()).await;
        })
        .await
        .unwrap();
    });
    let result = error(result, 9);
    assert_eq!(result["run_id"], "test-run");
    assert!(result["message"]
        .as_str()
        .unwrap()
        .contains("status request timed out"));
}

async fn stub_health_server(status: &'static str) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buffer = [0; 1024];
                let _ = socket.read(&mut buffer).await;
                let body = if status.starts_with("200") {
                    r#"{"self":"ok","image_backend":"unavailable","llm":"not_configured"}"#
                } else {
                    "{}"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn healthcheck_succeeds_only_when_the_server_answers_health() {
    let ok = stub_health_server("200 OK").await;
    let failing = stub_health_server("500 Internal Server Error").await;
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    for (port, success) in [(ok, true), (failing, false), (closed, false)] {
        let mut command = cli();
        command
            .env("GENAIMAGE_SERVER", format!("http://127.0.0.1:{port}"))
            .arg("healthcheck");
        assert_eq!(
            cli_output(command).await.status.success(),
            success,
            "{port}"
        );
    }
}

#[tokio::test]
async fn healthcheck_derives_its_target_from_the_server_port() {
    let ok = stub_health_server("200 OK").await;
    let mut command = cli();
    command
        .env("GENAIMAGE_SERVER_PORT", ok.to_string())
        .arg("healthcheck");
    assert!(cli_output(command).await.status.success());
}
