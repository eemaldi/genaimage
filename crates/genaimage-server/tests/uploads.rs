mod common;

use std::{io::Write, path::PathBuf, sync::atomic::Ordering};

use common::{install_workflow, ApiHarness};
use genaimage_api::{RunResponse, RunStatus, UploadResponse, WorkflowValidationResponse};
use reqwest::{
    header,
    multipart::{Form, Part},
    Method, Response, StatusCode,
};
use serde_json::{json, Value};

const UPLOADS: &str = "/api/v1/uploads";
const PNG: &[u8] = include_bytes!("fixtures/reference.png");

async fn upload(api: &ApiHarness, name: &str, bytes: &[u8], mime: &str) -> Response {
    api.request(Method::POST, UPLOADS)
        .multipart(
            Form::new().part(
                "file",
                Part::bytes(bytes.to_vec())
                    .file_name(name.to_owned())
                    .mime_str(mime)
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap()
}

async fn store_png(api: &ApiHarness) -> UploadResponse {
    let result = upload(api, "reference ?.png", PNG, "image/png").await;
    assert_eq!(result.status(), StatusCode::CREATED);
    result.json().await.unwrap()
}

#[tokio::test]
async fn detects_image_types_and_deduplicates_content() {
    let api = ApiHarness::start().await;
    for (bytes, expected) in [
        (PNG, "image/png"),
        (&include_bytes!("fixtures/reference.jpg")[..], "image/jpeg"),
        (&include_bytes!("fixtures/reference.webp")[..], "image/webp"),
    ] {
        let response = upload(&api, "../../unsafe name.txt", bytes, "text/plain").await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned();
        let result: UploadResponse = response.json().await.unwrap();
        assert_eq!(result.upload_id, format!("sha256:{}", result.sha256));
        assert_eq!(result.byte_size, bytes.len() as u64);
        assert_eq!(result.media_type, expected);
        assert_eq!(result.original_filename, "unsafe_name.txt");
        assert_eq!(api.get_json::<UploadResponse>(&location).await, result);
        let duplicate = upload(&api, "duplicate.png", bytes, "image/png").await;
        assert_eq!(duplicate.status(), StatusCode::CREATED);
        assert_eq!(duplicate.json::<UploadResponse>().await.unwrap(), result);
    }
    assert_eq!(
        std::fs::read_dir(&api.core.config().uploads)
            .unwrap()
            .count(),
        3
    );
    api.shutdown().await;
}

#[tokio::test]
async fn rejects_spoofed_content_and_multiple_parts_without_publishing() {
    let api = ApiHarness::start().await;
    let result = upload(&api, "fake.png", b"not an image", "image/png").await;
    ApiHarness::assert_problem(result, StatusCode::UNSUPPORTED_MEDIA_TYPE).await;
    let result = api
        .request(Method::POST, UPLOADS)
        .multipart(
            Form::new()
                .part("file", Part::bytes(PNG).file_name("first.png"))
                .part("file", Part::bytes(PNG).file_name("second.png")),
        )
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(result, StatusCode::BAD_REQUEST).await;
    assert_eq!(
        std::fs::read_dir(&api.core.config().uploads)
            .unwrap()
            .count(),
        0
    );
    api.shutdown().await;
}

#[tokio::test]
async fn enforces_the_limit_on_chunked_bodies_before_end_of_stream() {
    let api =
        ApiHarness::start_with_config(5, &[("GENAIMAGE_SERVER_MAX_UPLOAD_BYTES", "512")]).await;
    for prefix in ["", "\r\n--boundary--\r\n"] {
        let address = api.origin.strip_prefix("http://").unwrap();
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut first = format!("POST {UPLOADS} HTTP/1.1\r\nHost: {address}\r\nTransfer-Encoding: chunked\r\nContent-Type: multipart/form-data; boundary=boundary\r\nConnection: close\r\n\r\n").into_bytes();
        let mut part = b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\n\r\n".to_vec();
        part.extend_from_slice(PNG);
        part.extend_from_slice(prefix.as_bytes());
        write!(first, "{:x}\r\n", part.len()).unwrap();
        first.extend(part);
        first.extend_from_slice(b"\r\n");
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        connection.write_all(&first).await.unwrap();
        tokio::task::yield_now().await;
        let mut last = b"200\r\n".to_vec();
        last.extend(vec![b'x'; 512]);
        last.extend_from_slice(b"\r\n");
        connection.write_all(&last).await.unwrap();
        // Deliberately omit the terminating HTTP chunk.
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            connection.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        assert!(response.contains("application/problem+json"));
        assert!(response.contains("\"recovery\""));
    }
    if let Ok(entries) = std::fs::read_dir(&api.core.config().uploads) {
        assert_eq!(entries.count(), 0);
    }
    api.shutdown().await;
}

#[tokio::test]
async fn rejects_oversized_framing_without_waiting_for_its_terminator() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let api = ApiHarness::start().await;
    for prefix in [
        &b""[..],
        &b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nX-Large: "[..],
        &b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\n\r\n\x89PNG\r\n\x1a\nimage\r\n--boundary\r\nX-Large: "[..],
    ] {
        let address = api.origin.strip_prefix("http://").unwrap();
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut request = format!("POST {UPLOADS} HTTP/1.1\r\nHost: {address}\r\nTransfer-Encoding: chunked\r\nContent-Type: multipart/form-data; boundary=boundary\r\nConnection: close\r\n\r\n").into_bytes();
        // This is well under the whole-body limit. No framing or HTTP terminator is sent.
        write!(request, "{:x}\r\n", prefix.len() + 24 * 1024).unwrap();
        request.extend_from_slice(prefix);
        request.resize(request.len() + 24 * 1024, b'x');
        request.extend_from_slice(b"\r\n");
        connection.write_all(&request).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(3), connection.read_to_string(&mut response))
            .await.unwrap().unwrap();
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        assert!(response.contains("application/problem+json"));
        assert!(response.contains("multipart framing exceeds 16 KiB"));
        assert!(response.contains("\"recovery\""));
    }
    if let Ok(entries) = std::fs::read_dir(&api.core.config().uploads) {
        assert_eq!(entries.count(), 0);
    }
    api.shutdown().await;
}

#[tokio::test]
async fn framing_limit_does_not_limit_file_bytes() {
    let api = ApiHarness::start().await;
    let mut image = PNG.to_vec();
    // Payload length exceeds the framing limit; the API only checks the signature.
    image.resize(64 * 1024, 0);
    let result = upload(&api, "large.png", &image, "image/png").await;
    assert_eq!(result.status(), StatusCode::CREATED);
    let metadata: UploadResponse = result.json().await.unwrap();
    assert_eq!(metadata.byte_size, image.len() as u64);
    api.shutdown().await;
}

#[tokio::test]
async fn duplicate_upload_repairs_missing_and_corrupt_images_without_changing_metadata() {
    let api = ApiHarness::start().await;
    let original = store_png(&api).await;
    let image = api
        .core
        .config()
        .uploads
        .join(&original.sha256)
        .join("image");
    std::fs::remove_file(&image).unwrap();
    for damaged in [None, Some(vec![1]), Some(vec![1; PNG.len()])] {
        if let Some(bytes) = damaged {
            std::fs::write(&image, bytes).unwrap();
        }
        let result = upload(&api, "second.png", PNG, "image/png").await;
        assert_eq!(result.status(), StatusCode::CREATED);
        assert_eq!(result.json::<UploadResponse>().await.unwrap(), original);
        assert_eq!(std::fs::read(&image).unwrap(), PNG);
    }
    std::fs::remove_file(&image).unwrap();
    std::fs::create_dir(&image).unwrap();
    let result = upload(&api, "unrepairable.png", PNG, "image/png").await;
    ApiHarness::assert_problem(result, StatusCode::INTERNAL_SERVER_ERROR).await;
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn refuses_symlinked_upload_entries_without_touching_external_files() {
    use std::{fs, os::unix::fs::symlink};

    let api = ApiHarness::start().await;
    let original = store_png(&api).await;
    let digest = api.core.config().uploads.join(&original.sha256);
    let external = api.workspace().join("external");
    fs::rename(&digest, &external).unwrap();
    let external_image = external.join("image");
    let external_metadata = external.join("metadata.json");
    let original_metadata = fs::read(&external_metadata).unwrap();
    fs::write(&external_image, b"leave untouched").unwrap();
    symlink(&external, &digest).unwrap();

    for entry in [None, Some("image"), Some("metadata.json")] {
        if let Some(name) = entry {
            if digest.is_symlink() {
                fs::remove_file(&digest).unwrap();
                fs::create_dir(&digest).unwrap();
            } else {
                fs::remove_file(digest.join("image")).unwrap();
                fs::remove_file(digest.join("metadata.json")).unwrap();
            }
            symlink(external.join(name), digest.join(name)).unwrap();
            let other = if name == "image" {
                "metadata.json"
            } else {
                "image"
            };
            fs::copy(external.join(other), digest.join(other)).unwrap();
        }
        let result = upload(&api, "second.png", PNG, "image/png").await;
        let problem = ApiHarness::assert_problem(result, StatusCode::INTERNAL_SERVER_ERROR).await;
        assert!(problem.recovery.contains("move the unexpected entry"));
        assert_eq!(fs::read(&external_image).unwrap(), b"leave untouched");
        assert_eq!(fs::read(&external_metadata).unwrap(), original_metadata);
    }
    api.shutdown().await;
}

fn install_image_workflow(api: &ApiHarness) {
    install_workflow(api);
    let dir = &api.core.config().workflows;
    for (name, field, content) in [
        (
            "workflow.api.json",
            "12",
            json!({"class_type":"LoadImage","inputs":{"image":"original.png"}}),
        ),
        (
            "contract.genaimage.json",
            "reference",
            json!({"node":"12","input":"image","kind":"image_upload","required":true}),
        ),
    ] {
        let path = dir.join(name);
        let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if field == "12" {
            doc[field] = content;
        } else {
            doc["parameters"][field] = content;
        }
        std::fs::write(path, serde_json::to_vec(&doc).unwrap()).unwrap();
    }
}

async fn submit(api: &ApiHarness, key: &str, id: &str) -> Response {
    api.request(Method::POST, "/api/v1/runs")
        .header("Idempotency-Key", key)
        .json(
            &json!({"workflow":"contract-workflow","preset":"fast","parameters":{"reference":id}}),
        )
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn records_verified_bytes_and_backend_filename_without_reuploading_on_replay() {
    let api = ApiHarness::start().await;
    install_image_workflow(&api);
    let upload = store_png(&api).await;
    api.fake.complete.store(true, Ordering::SeqCst);
    let (a, b) = tokio::join!(
        submit(&api, "original", &upload.upload_id),
        submit(&api, "original", &upload.upload_id),
    );
    let (first, duplicate) = if a.status() == StatusCode::ACCEPTED {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(duplicate.status(), StatusCode::OK);
    let first: RunResponse = first.json().await.unwrap();
    assert_eq!(duplicate.json::<RunResponse>().await.unwrap().id, first.id);
    assert_eq!(first.parameters["reference"], upload.upload_id);
    let run_path = format!("/api/v1/runs/{}", first.id);
    let done: RunResponse = api
        .post_json(&format!("{run_path}/reconcile"), &Value::Null)
        .await;
    assert_eq!(done.status, RunStatus::Completed);
    let replay = submit(&api, "original", &upload.upload_id).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<RunResponse>().await.unwrap().id, first.id);
    assert_eq!(api.fake.upload_calls.load(Ordering::SeqCst), 1);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        api.fake.uploaded_image.lock().unwrap().as_deref(),
        Some(PNG)
    );
    let dir = PathBuf::from(api.core.repository().get(&first.id).unwrap().output_dir);
    let read =
        |file| serde_json::from_slice::<Value>(&std::fs::read(dir.join(file)).unwrap()).unwrap();
    let provenance = read("run-manifest.json");
    assert_eq!(
        provenance["source_inputs"][0],
        json!({
            "parameter":"reference", "upload_id":upload.upload_id,
        "sha256":"d0ec496d90eea2acef544e74d00b3c2590c7cacbb0f5dba95d90749a0fd5a268",
            "byte_size":upload.byte_size, "original_filename":"reference__.png", "uploaded_name":"uploaded-reference.png"
        })
    );
    assert_eq!(
        read("resolved-workflow.json")["12"]["inputs"]["image"],
        "uploaded-reference.png"
    );
    api.shutdown().await;
}

async fn rerun(api: &ApiHarness, id: &str, key: Option<&str>) -> Response {
    let request = api.request(Method::POST, &format!("/api/v1/runs/{id}/rerun"));
    let request = match key {
        Some(key) => request.header("Idempotency-Key", key),
        None => request,
    };
    request.send().await.unwrap()
}

#[tokio::test]
async fn rerun_preserves_upload_and_seed_and_monitors_the_new_run_only() {
    let api = ApiHarness::start().await;
    install_image_workflow(&api);
    let upload = store_png(&api).await;
    api.fake.complete.store(true, Ordering::SeqCst);
    let first: RunResponse = submit(&api, "original", &upload.upload_id)
        .await
        .json()
        .await
        .unwrap();
    let source_path = format!("/api/v1/runs/{}", first.id);
    let source: RunResponse = api
        .post_json(&format!("{source_path}/reconcile"), &Value::Null)
        .await;
    assert_eq!(source.status, RunStatus::Completed);
    let before: Value = api.get_json(&source_path).await;
    let original_manifest: Value = api.get_json(&format!("{source_path}/manifest")).await;
    let mut events = api.subscribe();
    let (a, b) = tokio::join!(
        rerun(&api, &first.id, Some("rerun")),
        rerun(&api, &first.id, Some("rerun"))
    );
    let (created, replay) = if a.status() == StatusCode::CREATED {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(replay.status(), StatusCode::OK);
    let location = created.headers()[reqwest::header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        replay.headers()[reqwest::header::LOCATION],
        location.as_str()
    );
    let created: RunResponse = created.json().await.unwrap();
    assert_eq!(location, format!("/api/v1/runs/{}", created.id));
    assert_eq!(replay.json::<RunResponse>().await.unwrap().id, created.id);
    assert_eq!(created.rerun_of.as_deref(), Some(first.id.as_str()));
    assert_ne!(created.id, first.id);
    assert_ne!(created.prompt_id, first.prompt_id);
    assert_eq!(created.parameters, first.parameters);
    assert_eq!(created.parameters["seed"], 42);
    assert_eq!(created.parameters["reference"], upload.upload_id);
    ApiHarness::await_event(&mut events, std::time::Duration::from_secs(5), |envelope| {
        matches!(&envelope.event, genaimage_core::Event::RunCompleted { run } if run.id == created.id)
    }).await;
    let manifest: Value = api.get_json(&format!("{location}/manifest")).await;
    assert_eq!(manifest["rerun_of"], first.id);
    assert_eq!(
        manifest["source_inputs"],
        original_manifest["source_inputs"]
    );
    assert_eq!(manifest["preset"], "fast");
    assert_eq!(api.get_json::<Value>(&source_path).await, before);
    assert_eq!(
        api.get_json::<Value>(&format!("{source_path}/manifest"))
            .await,
        original_manifest
    );
    ApiHarness::assert_problem(
        rerun(&api, &created.id, Some("rerun")).await,
        StatusCode::CONFLICT,
    )
    .await;
    ApiHarness::assert_problem(
        rerun(&api, &first.id, Some("original")).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 2);
    assert_eq!(api.fake.upload_calls.load(Ordering::SeqCst), 2);
    api.shutdown().await;
}

#[tokio::test]
async fn rerun_refuses_changed_workflow_bindings_or_input_without_sending_a_prompt() {
    let api = ApiHarness::start().await;
    install_image_workflow(&api);
    let upload = store_png(&api).await;
    let first: RunResponse = submit(&api, "original", &upload.upload_id)
        .await
        .json()
        .await
        .unwrap();
    let workflows = &api.core.config().workflows;
    let graph = workflows.join("workflow.api.json");
    let sidecar = api
        .core
        .load_workflow_by_id("contract-workflow")
        .unwrap()
        .unwrap()
        .manifest_path;
    let image = api.core.config().uploads.join(&upload.sha256).join("image");
    for (path, kind) in [(graph, "graph"), (sidecar, "bindings"), (image, "image")] {
        let original = std::fs::read(&path).unwrap();
        let changed = if kind == "image" {
            let mut bytes = original.clone();
            *bytes.last_mut().unwrap() ^= 1;
            bytes
        } else {
            let mut doc: Value = serde_json::from_slice(&original).unwrap();
            if kind == "graph" {
                doc["9"]["inputs"]["filename_prefix"] = json!("changed");
            } else {
                doc["presets"]["fast"]["steps"] = json!(11);
            }
            serde_json::to_vec(&doc).unwrap()
        };
        std::fs::write(&path, changed).unwrap();
        let response = rerun(&api, &first.id, Some("not-reserved")).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response.headers()[reqwest::header::CONTENT_TYPE],
            "application/problem+json"
        );
        let problem: genaimage_api::ProblemDetails = response.json().await.unwrap();
        assert_eq!(problem.problem_type, "urn:genaimage:problem:rerun-mismatch");
        assert!(problem.recovery.contains("Remix"));
        std::fs::write(path, original).unwrap();
    }
    ApiHarness::assert_problem(rerun(&api, &first.id, None).await, StatusCode::BAD_REQUEST).await;
    ApiHarness::assert_problem(
        rerun(&api, "run-missing", Some("not-reserved")).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(api.fake.upload_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        rerun(&api, &first.id, Some("not-reserved")).await.status(),
        StatusCode::CREATED
    );
    api.shutdown().await;
}

#[tokio::test]
async fn unknown_ids_are_reported_per_parameter_without_reserving_the_key() {
    let api = ApiHarness::start().await;
    install_image_workflow(&api);
    let unknown = format!("sha256:{}", "0".repeat(64));
    let validation: WorkflowValidationResponse = api
        .post_json(
            "/api/v1/workflows/contract-workflow/validate",
            &json!({"preset":"fast", "parameters":{"reference":unknown}}),
        )
        .await;
    assert!(!validation.valid);
    assert_eq!(validation.parameter_errors.len(), 1);
    assert_eq!(validation.parameter_errors[0].parameter, "reference");
    assert!(validation.parameter_errors[0]
        .reason
        .contains("Unknown upload ID"));
    let result = submit(&api, "correctable", &unknown).await;
    let problem = ApiHarness::assert_problem(result, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert!(problem.detail.contains("reference: Unknown upload ID"));
    assert_eq!(api.fake.upload_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    let upload = store_png(&api).await;
    assert_eq!(
        submit(&api, "correctable", &upload.upload_id)
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    api.shutdown().await;
}

#[tokio::test]
async fn backend_rejection_and_corrupt_bytes_fail_without_sending_a_prompt() {
    let api = ApiHarness::start().await;
    install_image_workflow(&api);
    let upload = store_png(&api).await;
    api.fake.reject_upload.store(true, Ordering::SeqCst);
    let rejected = submit(&api, "reject", &upload.upload_id).await;
    assert_eq!(rejected.status(), StatusCode::ACCEPTED);
    let rejected: RunResponse = rejected.json().await.unwrap();
    assert_eq!(rejected.status, RunStatus::Failed);
    assert!(rejected
        .error
        .unwrap()
        .contains("failed before prompt submission"));
    let replay = submit(&api, "reject", &upload.upload_id).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<RunResponse>().await.unwrap().id, rejected.id);
    let mut corrupt = PNG.to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(
        api.core.config().uploads.join(&upload.sha256).join("image"),
        corrupt,
    )
    .unwrap();
    let corrupt = submit(&api, "corrupt", &upload.upload_id).await;
    assert_eq!(corrupt.status(), StatusCode::ACCEPTED);
    let corrupt: RunResponse = corrupt.json().await.unwrap();
    assert_eq!(corrupt.status, RunStatus::Failed);
    assert!(corrupt.error.unwrap().contains("SHA-256"));
    assert_eq!(api.fake.upload_calls.load(Ordering::SeqCst), 1);
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    api.shutdown().await;
}
