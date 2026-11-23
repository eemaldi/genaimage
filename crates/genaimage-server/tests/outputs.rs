mod common;

use std::{io::Cursor, path::PathBuf, sync::atomic::Ordering};

use common::{install_workflow, ApiHarness};
use genaimage_api::OutputListResponse;
use genaimage_core::{RunRecord, RunRequest, RunStatus};
use reqwest::{header, Method, StatusCode};
use serde_json::{json, Value};

const OUTPUTS: &str = "/api/v1/outputs";

async fn completed(api: &ApiHarness, tag: &str) -> RunRecord {
    let path = api.core.config().workflows.join("contract.genaimage.json");
    let mut workflow: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    workflow["tags"] = json!([tag]);
    std::fs::write(&path, serde_json::to_vec(&workflow).unwrap()).unwrap();
    api.fake.complete.store(false, Ordering::SeqCst);
    let run = api
        .submit_run(RunRequest {
            workflow: "contract-workflow".into(),
            preset: Some("fast".into()),
            parameters: Default::default(),
            version: None,
        })
        .await
        .unwrap();
    api.fake.complete.store(true, Ordering::SeqCst);
    let run = api.core.reconcile(&run.id).await.unwrap().run;
    assert_eq!(run.status, RunStatus::Completed);
    run
}

#[tokio::test]
async fn paginates_and_filters_by_run_workflow_dates_and_archived_tags() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let older = completed(&api, "Gallery").await;
    let newer = completed(&api, "Other").await;
    let first: OutputListResponse = api.get_json(&format!("{OUTPUTS}?limit=1")).await;
    assert_eq!(first.outputs[0].run_id, newer.id);
    let second: OutputListResponse = api
        .get_json(&format!(
            "{OUTPUTS}?limit=1&cursor={}",
            first.next_cursor.unwrap()
        ))
        .await;
    assert!(second.next_cursor.is_none());
    let output = &second.outputs[0];
    assert_eq!(output.run_id, older.id);
    assert_eq!(output.tags, ["Gallery"]);
    let request = api
        .request(Method::GET, OUTPUTS)
        .query(&[
            ("run", older.id.as_str()),
            ("workflow", "contract-workflow"),
            ("tag", "Gallery"),
            ("created_from", &output.created_at.to_rfc3339()),
            ("created_to", &output.created_at.to_rfc3339()),
        ])
        .send()
        .await
        .unwrap();
    let filtered: OutputListResponse = request.error_for_status().unwrap().json().await.unwrap();
    assert_eq!(filtered.outputs.len(), 1);
    assert_eq!(filtered.outputs[0].id, output.id);
    for query in [
        "run=missing",
        "workflow=missing",
        "tag=gallery",
        "created_from=2999-01-01T00:00:00Z",
    ] {
        let empty: OutputListResponse = api.get_json(&format!("{OUTPUTS}?{query}")).await;
        assert!(empty.outputs.is_empty(), "{query}");
    }
    let response = api
        .request(Method::GET, &format!("{OUTPUTS}?cursor=invalid"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    api.shutdown().await;
}

#[tokio::test]
async fn serves_verified_originals_thumbnails_and_stored_manifests() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(320, 160)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    *api.fake.output_bytes.lock().unwrap() = Some(png.clone());
    let run = completed(&api, "gallery").await;
    let outputs: OutputListResponse = api.get_json(OUTPUTS).await;
    let output = &outputs.outputs[0];
    let path = format!("{OUTPUTS}/{}", output.id);
    let response = api.request(Method::GET, &path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    let etag = format!("\"{}\"", output.sha256);
    assert_eq!(response.headers()[header::ETAG], etag);
    assert_eq!(response.bytes().await.unwrap(), png);
    let response = api
        .request(Method::GET, &path)
        .header(header::IF_NONE_MATCH, format!("\"other\", W/{etag}"))
        .header(header::RANGE, "bytes=1-3")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(response.bytes().await.unwrap().is_empty());
    for (range, start, end) in [
        ("bytes=1-3", 1, 3),
        ("bytes=2-", 2, png.len() - 1),
        ("bytes=-4", png.len() - 4, png.len() - 1),
    ] {
        let response = api
            .request(Method::GET, &path)
            .header(header::RANGE, range)
            .header(header::IF_RANGE, &etag)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes {start}-{end}/{}", png.len())
        );
        assert_eq!(response.bytes().await.unwrap(), &png[start..=end]);
    }
    let response = api
        .request(Method::GET, &path)
        .header(header::RANGE, "bytes=1-3")
        .header(header::IF_RANGE, "\"old\"")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    for range in ["bytes=999999-", "bytes=0-1,3-4", "bytes=-0"] {
        let response = api
            .request(Method::GET, &path)
            .header(header::RANGE, range)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes */{}", png.len())
        );
        ApiHarness::assert_problem(response, StatusCode::RANGE_NOT_SATISFIABLE).await;
    }
    let thumb_path = format!("{path}/thumbnail?size=128");
    let response = api.request(Method::GET, &thumb_path).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/webp");
    let thumb_etag = response.headers()[header::ETAG].clone();
    let thumbnail = response.bytes().await.unwrap();
    let thumbnail =
        image::load_from_memory_with_format(&thumbnail, image::ImageFormat::WebP).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (128, 64));
    let response = api
        .request(Method::GET, &thumb_path)
        .header(header::IF_NONE_MATCH, thumb_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    for query in ["", "?size=127", "?size=-1", "?size=128&size=512"] {
        let response = api
            .request(Method::GET, &format!("{path}/thumbnail{query}"))
            .send()
            .await
            .unwrap();
        ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    }
    let manifest = PathBuf::from(run.manifest_path.unwrap());
    let manifest_url = format!("/api/v1/runs/{}/manifest", run.id);
    let response = api
        .request(Method::GET, &manifest_url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let manifest_etag = response.headers()[header::ETAG].clone();
    let stored = std::fs::read(&manifest).unwrap();
    assert_eq!(response.bytes().await.unwrap(), stored);
    let response = api
        .request(Method::GET, &manifest_url)
        .header(header::IF_NONE_MATCH, manifest_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    // Even valid JSON for the correct run must not replace the stored manifest.
    std::fs::write(&manifest, [stored.as_slice(), b"\n"].concat()).unwrap();
    let response = api
        .request(Method::GET, &manifest_url)
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    let provenance = api.core.repository().outputs(&run.id).unwrap();
    let mut changed = png;
    changed[0] ^= 1;
    std::fs::write(&provenance[0].local_path, changed).unwrap();
    let response = api
        .request(Method::GET, &path)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    let response = api
        .request(Method::GET, &format!("{path}/thumbnail?size=256"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
    for path in [
        format!("{OUTPUTS}/unknown"),
        format!("{OUTPUTS}/unknown/thumbnail?size=128"),
        "/api/v1/runs/unknown/manifest".into(),
    ] {
        let response = api.request(Method::GET, &path).send().await.unwrap();
        ApiHarness::assert_problem(response, StatusCode::NOT_FOUND).await;
    }
    api.shutdown().await;
}
