mod common;

use std::{
    fs,
    sync::atomic::{AtomicU32, Ordering},
};

use common::{install_workflow, ApiHarness};
use genaimage_api::{
    CharacterListResponse, CharacterOutputListResponse, DecisionListResponse, OutputListResponse,
    ProblemDetails, ReferenceListResponse, RunListResponse, RunResponse, SpecVersionListResponse,
    UploadResponse,
};
use reqwest::{
    header,
    multipart::{Form, Part},
    Method, Response, StatusCode,
};
use serde_json::{json, Value};

const CHARACTERS: &str = "/api/v1/characters";
const RUNS: &str = "/api/v1/runs";
const PNG: &[u8] = include_bytes!("fixtures/reference.png");
const PROBLEM: &str = "urn:genaimage:problem:";

static KEYS: AtomicU32 = AtomicU32::new(0);

fn spec(summary: &str) -> Value {
    json!({
        "summary": summary,
        "elements": [{
            "id": "cloak", "category": "clothing",
            "description": "A hooded crimson cloak", "requirement": "mandatory"
        }],
        "palette": [{"name": "crimson", "hex": "#dc143c", "role": "cloak"}],
        "views": ["front", "three_quarter"],
        "style_notes": "Flat shading"
    })
}

async fn send(
    api: &ApiHarness,
    method: Method,
    path: &str,
    key: Option<&str>,
    body: Option<&Value>,
) -> Response {
    let mut request = api.request(method, path);
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    request.send().await.unwrap()
}

async fn post(api: &ApiHarness, path: &str, key: Option<&str>, body: &Value) -> Response {
    send(api, Method::POST, path, key, Some(body)).await
}

async fn ok(response: Response, status: StatusCode) -> Value {
    assert_eq!(response.status(), status);
    response.json().await.unwrap()
}

async fn problem(response: Response, status: StatusCode, kind: &str) -> ProblemDetails {
    ApiHarness::assert_typed_problem(response, status, &format!("{PROBLEM}{kind}")).await
}

async fn character(api: &ApiHarness, name: &str) -> String {
    let created = ok(
        post(
            api,
            CHARACTERS,
            None,
            &json!({"name": name, "spec": spec("v1")}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    created["character"]["id"].as_str().unwrap().to_owned()
}

async fn upload(api: &ApiHarness) -> String {
    let response = api
        .request(Method::POST, "/api/v1/uploads")
        .multipart(
            Form::new().part(
                "file",
                Part::bytes(PNG.to_vec())
                    .file_name("sketch.png")
                    .mime_str("image/png")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<UploadResponse>().await.unwrap().upload_id
}

fn run_request(character: Option<Value>) -> Value {
    let mut body =
        json!({"workflow": "contract-workflow", "preset": "fast", "parameters": {"seed": 99}});
    if let Some(character) = character {
        body["character"] = character;
    }
    body
}

async fn submit(api: &ApiHarness, body: &Value) -> Response {
    let key = format!("run-{}", KEYS.fetch_add(1, Ordering::SeqCst));
    post(api, RUNS, Some(&key), body).await
}

/// Submits a run, lets ComfyUI finish it and returns the run and its output ID.
async fn completed_run(api: &ApiHarness, link: Option<Value>) -> (RunResponse, String) {
    api.fake.complete.store(false, Ordering::SeqCst);
    let response = submit(api, &run_request(link)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let run: RunResponse = response.json().await.unwrap();
    api.fake.complete.store(true, Ordering::SeqCst);
    settle(api, &run.id).await;
    let outputs: OutputListResponse = api
        .get_json(&format!("/api/v1/outputs?run={}", run.id))
        .await;
    assert_eq!(outputs.outputs.len(), 1);
    (run, outputs.outputs[0].id.clone())
}

async fn settle(api: &ApiHarness, run_id: &str) {
    for _ in 0..100 {
        let result = api.core.reconcile(run_id).await.unwrap();
        if result.run.status == genaimage_core::RunStatus::Completed {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("run {run_id} did not complete");
}

fn link(id: &str, version: u32) -> Value {
    json!({"id": id, "spec_version": version})
}

#[tokio::test]
async fn creates_reads_lists_and_pages_characters() {
    let api = ApiHarness::start().await;
    let response = post(
        &api,
        CHARACTERS,
        None,
        &json!({"name": "Mara", "spec": spec("first"), "note": "initial"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let created: Value = response.json().await.unwrap();
    let id = created["character"]["id"].as_str().unwrap();
    assert!(id.starts_with("char-"));
    assert_eq!(location, format!("{CHARACTERS}/{id}"));
    let version = &created["spec_version"];
    assert_eq!(
        *version,
        json!({
            "character_id": id, "version": 1, "created_at": version["created_at"],
            "based_on": null, "spec": spec("first"), "note": "initial"
        })
    );
    assert_eq!(
        created["character"],
        json!({
            "id": id, "name": "Mara", "created_at": created["character"]["created_at"],
            "latest_spec_version": 1
        })
    );
    assert_eq!(api.get_json::<Value>(&location).await, created["character"]);

    let bare = ok(
        post(&api, CHARACTERS, None, &json!({"name": "Bare"})).await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(bare["spec_version"], Value::Null);
    assert_eq!(bare["character"]["latest_spec_version"], Value::Null);
    let third = character(&api, "Third").await;

    let first: CharacterListResponse = api.get_json(&format!("{CHARACTERS}?limit=2")).await;
    assert_eq!(
        first
            .characters
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["Third", "Bare"]
    );
    assert_eq!(first.characters[0].id, third);
    let cursor = first.next_cursor.unwrap();
    let second: CharacterListResponse = api
        .get_json(&format!("{CHARACTERS}?limit=2&cursor={cursor}"))
        .await;
    assert_eq!(second.characters.len(), 1);
    assert_eq!(second.characters[0].name, "Mara");
    assert_eq!(second.next_cursor, None);

    for query in ["limit=0", "limit=201", "cursor=not-a-cursor"] {
        let response = api
            .request(Method::GET, &format!("{CHARACTERS}?{query}"))
            .send()
            .await
            .unwrap();
        problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-character-request",
        )
        .await;
    }
    let response = api
        .request(Method::GET, &format!("{CHARACTERS}?limit=2.0"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    let response = api
        .request(Method::GET, &format!("{CHARACTERS}?unknown=1"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    api.shutdown().await;
}

#[tokio::test]
async fn unknown_characters_are_not_found_and_invalid_requests_write_nothing() {
    let api = ApiHarness::start().await;
    for path in [
        "char-nope",
        "char-nope/specs",
        "char-nope/specs/1",
        "char-nope/references",
        "char-nope/decisions",
        "char-nope/outputs",
        "char-nope/runs",
    ] {
        let response = api
            .request(Method::GET, &format!("{CHARACTERS}/{path}"))
            .send()
            .await
            .unwrap();
        let found = problem(response, StatusCode::NOT_FOUND, "character-not-found").await;
        assert!(found.detail.contains("char-nope"), "{path}");
    }
    for (path, body) in [
        ("specs", json!({"spec": spec("x")})),
        (
            "references",
            json!({"upload_id": "sha256:x", "role": "sketch"}),
        ),
        (
            "decisions",
            json!({"output_id": "run:0", "spec_version": 1, "decision": "approved"}),
        ),
    ] {
        let response = post(&api, &format!("{CHARACTERS}/char-nope/{path}"), None, &body).await;
        problem(response, StatusCode::NOT_FOUND, "character-not-found").await;
    }
    let response = send(
        &api,
        Method::POST,
        &format!("{CHARACTERS}/char-nope/references/charref-x/remove"),
        None,
        None,
    )
    .await;
    problem(response, StatusCode::NOT_FOUND, "character-not-found").await;

    for body in [
        json!({"name": ""}),
        json!({"name": "a\nb"}),
        json!({"name": "x".repeat(201)}),
        json!({"name": "Note without spec", "note": "orphan"}),
        json!({"name": "Bad spec", "spec": {"summary": ""}}),
        json!({"name": "Bad spec", "spec": {"summary": "x", "views": ["front", "front"]}}),
        json!({"name": "Bad hex", "spec": {"summary": "x", "palette": [{"name": "a", "hex": "red"}]}}),
    ] {
        let response = post(&api, CHARACTERS, None, &body).await;
        let invalid = problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-character-request",
        )
        .await;
        assert!(invalid.recovery.contains("Nothing was written"), "{body}");
    }
    for body in [
        json!({"name": "x", "surprise": true}),
        json!({"name": "x", "spec": {"summary": "x", "elements": [{"id": "a", "category": "hat", "description": "d", "requirement": "mandatory"}]}}),
        json!({"name": 7}),
    ] {
        let response = post(&api, CHARACTERS, None, &body).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
    }
    let response = api
        .request(Method::POST, CHARACTERS)
        .header(header::CONTENT_TYPE, "text/plain")
        .body("{}")
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::UNSUPPORTED_MEDIA_TYPE).await;
    let response = api
        .request(Method::POST, CHARACTERS)
        .json(&json!({"name": "huge", "note": "x".repeat(2 << 20)}))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::PAYLOAD_TOO_LARGE).await;
    let none: CharacterListResponse = api.get_json(CHARACTERS).await;
    assert!(none.characters.is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn spec_versions_are_append_only_with_a_conflict_that_names_the_latest() {
    let api = ApiHarness::start().await;
    let id = character(&api, "Mara").await;
    let specs = format!("{CHARACTERS}/{id}/specs");

    let response = post(
        &api,
        &specs,
        None,
        &json!({"based_on": 1, "spec": spec("second"), "note": "tweak"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers()[header::LOCATION],
        format!("{specs}/2").as_str()
    );
    let second: Value = response.json().await.unwrap();
    assert_eq!(
        second,
        json!({
            "character_id": id, "version": 2, "created_at": second["created_at"],
            "based_on": 1, "spec": spec("second"), "note": "tweak"
        })
    );

    for based_on in [json!({"based_on": 1}), json!({}), json!({"based_on": 5})] {
        let mut body = json!({"spec": spec("stale")});
        body.as_object_mut()
            .unwrap()
            .extend(based_on.as_object().unwrap().clone());
        let conflict = problem(
            post(&api, &specs, None, &body).await,
            StatusCode::CONFLICT,
            "character-spec-conflict",
        )
        .await;
        assert_eq!(conflict.latest, Some(2), "{body}");
        assert!(conflict.recovery.contains("Nothing was written"));
    }
    let versions: SpecVersionListResponse = api.get_json(&specs).await;
    assert_eq!(
        versions
            .versions
            .iter()
            .map(|v| v.version)
            .collect::<Vec<_>>(),
        [2, 1]
    );

    let paged: SpecVersionListResponse = api.get_json(&format!("{specs}?limit=1")).await;
    assert_eq!(paged.versions[0].version, 2);
    let next: SpecVersionListResponse = api
        .get_json(&format!(
            "{specs}?limit=1&cursor={}",
            paged.next_cursor.unwrap()
        ))
        .await;
    assert_eq!(next.versions[0].version, 1);
    assert_eq!(next.next_cursor, None);

    assert_eq!(api.get_json::<Value>(&format!("{specs}/2")).await, second);
    assert_eq!(
        api.get_json::<Value>(&format!("{specs}/1")).await["spec"],
        spec("v1")
    );
    for version in ["0", "3"] {
        let response = api
            .request(Method::GET, &format!("{specs}/{version}"))
            .send()
            .await
            .unwrap();
        problem(
            response,
            StatusCode::NOT_FOUND,
            "character-spec-version-not-found",
        )
        .await;
    }
    for version in ["01", "+1", "2.0", "1e0", "-1", "x", "4294967296"] {
        let response = api
            .request(Method::GET, &format!("{specs}/{version}"))
            .send()
            .await
            .unwrap();
        ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    }

    for based_on in [json!(2.0), json!("2"), json!(-1)] {
        let response = post(
            &api,
            &specs,
            None,
            &json!({"based_on": based_on, "spec": spec("x")}),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{based_on}"
        );
    }
    let oversized = json!({
        "summary": "s".repeat(2_000),
        "elements": (0..64).map(|n| json!({
            "id": format!("e{n}"), "category": "other",
            "description": "d".repeat(1_000), "requirement": "flexible"
        })).collect::<Vec<_>>(),
        "style_notes": "n".repeat(4_000)
    });
    let invalid = problem(
        post(
            &api,
            &specs,
            None,
            &json!({"based_on": 2, "spec": oversized}),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid-character-request",
    )
    .await;
    assert!(
        invalid.detail.contains("65536") || invalid.detail.contains("64 KiB"),
        "{}",
        invalid.detail
    );
    let character: Value = api.get_json(&format!("{CHARACTERS}/{id}")).await;
    assert_eq!(character["latest_spec_version"], 2);
    api.shutdown().await;
}

#[tokio::test]
async fn references_are_attached_soft_removed_and_listed() {
    let api = ApiHarness::start().await;
    let id = character(&api, "Mara").await;
    let references = format!("{CHARACTERS}/{id}/references");

    let unknown = problem(
        post(
            &api,
            &references,
            None,
            &json!({"upload_id": "sha256:feed", "role": "sketch"}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "character-upload-not-found",
    )
    .await;
    assert!(unknown.detail.contains("sha256:feed"));
    assert!(api
        .get_json::<ReferenceListResponse>(&references)
        .await
        .references
        .is_empty());

    let upload_id = upload(&api).await;
    let added = ok(
        post(
            &api,
            &references,
            None,
            &json!({"upload_id": upload_id, "role": "sketch", "note": "pose"}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let reference_id = added["id"].as_str().unwrap().to_owned();
    assert!(reference_id.starts_with("charref-"));
    assert_eq!(
        added,
        json!({
            "id": reference_id, "character_id": id, "upload_id": upload_id, "role": "sketch",
            "note": "pose", "added_at": added["added_at"], "removed_at": null
        })
    );
    problem(
        post(
            &api,
            &references,
            None,
            &json!({"upload_id": upload_id, "role": "sketch"}),
        )
        .await,
        StatusCode::CONFLICT,
        "character-conflict",
    )
    .await;
    let mask = ok(
        post(
            &api,
            &references,
            None,
            &json!({"upload_id": upload_id, "role": "mask"}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    for body in [
        json!({"upload_id": upload_id, "role": "photo"}),
        json!({"upload_id": upload_id}),
        json!({"upload_id": upload_id, "role": "mask", "extra": 1}),
    ] {
        assert_eq!(
            post(&api, &references, None, &body).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    problem(
        post(
            &api,
            &references,
            None,
            &json!({"upload_id": "x".repeat(129), "role": "mask"}),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid-character-request",
    )
    .await;

    let remove = format!("{references}/{reference_id}/remove");
    let removed = ok(
        send(&api, Method::POST, &remove, None, None).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(removed["id"], reference_id.as_str());
    assert!(removed["removed_at"].is_string());
    let again = ok(
        send(&api, Method::POST, &remove, None, None).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(again, removed);

    let active: ReferenceListResponse = api.get_json(&references).await;
    assert_eq!(active.references.len(), 1);
    assert_eq!(active.references[0].id, mask["id"].as_str().unwrap());
    let all: ReferenceListResponse = api
        .get_json(&format!("{references}?include_removed=true"))
        .await;
    assert_eq!(all.references.len(), 2);
    assert_eq!(all.references[1].id, reference_id);
    assert!(all.references[1].removed_at.is_some());
    let paged: ReferenceListResponse = api
        .get_json(&format!("{references}?include_removed=true&limit=1"))
        .await;
    assert!(paged.next_cursor.is_some());

    problem(
        send(
            &api,
            Method::POST,
            &format!("{references}/charref-missing/remove"),
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
        "character-reference-not-found",
    )
    .await;
    let other = character(&api, "Other").await;
    problem(
        send(
            &api,
            Method::POST,
            &format!("{CHARACTERS}/{other}/references/{reference_id}/remove"),
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
        "character-reference-not-found",
    )
    .await;
    api.shutdown().await;
}

#[tokio::test]
async fn decisions_derive_current_states_joined_with_outputs() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let id = character(&api, "Mara").await;
    let rival = character(&api, "Rival").await;
    let decisions = format!("{CHARACTERS}/{id}/decisions");
    let (_, first) = completed_run(&api, Some(link(&id, 1))).await;
    let (_, second) = completed_run(&api, Some(link(&id, 1))).await;
    let (_, third) = completed_run(&api, None).await;
    let (_, foreign) = completed_run(&api, Some(link(&rival, 1))).await;

    let decide = |output: &str, version: u32, decision: &str| json!({"output_id": output, "spec_version": version, "decision": decision, "note": "n"});
    let approved = ok(
        post(&api, &decisions, None, &decide(&first, 1, "approved")).await,
        StatusCode::CREATED,
    )
    .await;
    assert!(approved["id"].as_str().unwrap().starts_with("chardec-"));
    assert_eq!(
        approved,
        json!({
            "id": approved["id"], "character_id": id, "output_id": first, "spec_version": 1,
            "decision": "approved", "note": "n", "decided_at": approved["decided_at"]
        })
    );
    ok(
        post(&api, &decisions, None, &decide(&second, 1, "rejected")).await,
        StatusCode::CREATED,
    )
    .await;
    ok(
        post(&api, &decisions, None, &decide(&third, 1, "approved")).await,
        StatusCode::CREATED,
    )
    .await;
    ok(
        post(&api, &decisions, None, &decide(&third, 1, "cleared")).await,
        StatusCode::CREATED,
    )
    .await;
    ok(
        post(&api, &decisions, None, &decide(&second, 1, "approved")).await,
        StatusCode::CREATED,
    )
    .await;

    let outputs = format!("{CHARACTERS}/{id}/outputs");
    let current: CharacterOutputListResponse = api.get_json(&outputs).await;
    assert_eq!(
        current
            .outputs
            .iter()
            .map(|o| o.output.id.as_str())
            .collect::<Vec<_>>(),
        [second.as_str(), first.as_str()]
    );
    assert!(current
        .outputs
        .iter()
        .all(|o| o.decision.decision == genaimage_api::CharacterDecision::Approved));
    let joined = &current.outputs[1];
    let metadata: Value = api
        .get_json(&format!("/api/v1/outputs/{first}/metadata"))
        .await;
    assert_eq!(serde_json::to_value(&joined.output).unwrap(), metadata);
    assert_eq!(joined.decision.id, approved["id"]);

    let rejected: CharacterOutputListResponse =
        api.get_json(&format!("{outputs}?state=rejected")).await;
    assert!(rejected.outputs.is_empty());
    ok(
        post(&api, &decisions, None, &decide(&first, 1, "rejected")).await,
        StatusCode::CREATED,
    )
    .await;
    let rejected: CharacterOutputListResponse =
        api.get_json(&format!("{outputs}?state=rejected")).await;
    assert_eq!(rejected.outputs.len(), 1);
    assert_eq!(rejected.outputs[0].output.id, first);
    let approved_only: CharacterOutputListResponse =
        api.get_json(&format!("{outputs}?state=approved")).await;
    assert_eq!(approved_only.outputs.len(), 1);
    assert_eq!(approved_only.outputs[0].output.id, second);
    let paged: CharacterOutputListResponse = api.get_json(&format!("{outputs}?limit=1")).await;
    assert_eq!(paged.outputs.len(), 1);
    let rest: CharacterOutputListResponse = api
        .get_json(&format!(
            "{outputs}?limit=1&cursor={}",
            paged.next_cursor.unwrap()
        ))
        .await;
    assert_eq!(rest.outputs.len(), 1);
    assert_eq!(rest.next_cursor, None);
    assert_ne!(rest.outputs[0].output.id, paged.outputs[0].output.id);
    for query in ["state=cleared", "state=approved&spec_version=2.0"] {
        let response = api
            .request(Method::GET, &format!("{outputs}?{query}"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error(), "{query}");
    }
    let none: CharacterOutputListResponse =
        api.get_json(&format!("{outputs}?spec_version=7")).await;
    assert!(none.outputs.is_empty());

    let history: DecisionListResponse = api.get_json(&decisions).await;
    assert_eq!(
        history
            .decisions
            .iter()
            .map(|d| (d.output_id.as_str(), format!("{:?}", d.decision)))
            .collect::<Vec<_>>(),
        [
            (first.as_str(), "Rejected".to_owned()),
            (second.as_str(), "Approved".to_owned()),
            (third.as_str(), "Cleared".to_owned()),
            (third.as_str(), "Approved".to_owned()),
            (second.as_str(), "Rejected".to_owned()),
            (first.as_str(), "Approved".to_owned()),
        ]
    );
    let of_third: DecisionListResponse = api
        .get_json(&format!("{decisions}?output_id={third}"))
        .await;
    assert_eq!(of_third.decisions.len(), 2);
    let paged: DecisionListResponse = api.get_json(&format!("{decisions}?limit=4")).await;
    assert_eq!(paged.decisions.len(), 4);
    assert!(paged.next_cursor.is_some());

    let before = history.decisions.len();
    let conflict = problem(
        post(&api, &decisions, None, &decide(&foreign, 1, "approved")).await,
        StatusCode::CONFLICT,
        "character-conflict",
    )
    .await;
    assert!(conflict.detail.contains(&rival) && conflict.recovery.contains("Nothing was written"));
    problem(
        post(&api, &decisions, None, &decide(&third, 1, "cleared")).await,
        StatusCode::CONFLICT,
        "character-conflict",
    )
    .await;
    for output in ["../x", "run:01", "", &format!("{first} ")] {
        problem(
            post(&api, &decisions, None, &decide(output, 1, "approved")).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-character-request",
        )
        .await;
    }
    problem(
        post(
            &api,
            &decisions,
            None,
            &decide("run-missing:0", 1, "approved"),
        )
        .await,
        StatusCode::NOT_FOUND,
        "output-not-found",
    )
    .await;
    problem(
        post(&api, &decisions, None, &decide(&first, 9, "approved")).await,
        StatusCode::NOT_FOUND,
        "character-spec-version-not-found",
    )
    .await;
    for version in [json!(1.0), json!("1"), json!(-1), json!(4294967296_u64)] {
        let body = json!({"output_id": first, "spec_version": version, "decision": "approved"});
        assert_eq!(
            post(&api, &decisions, None, &body).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{version}"
        );
    }
    assert_eq!(
        api.get_json::<DecisionListResponse>(&decisions)
            .await
            .decisions
            .len(),
        before
    );
    api.shutdown().await;
}

#[tokio::test]
async fn every_post_replays_with_its_key_and_refuses_another_request() {
    let api = ApiHarness::start().await;
    install_workflow(&api);

    let body = json!({"name": "Keyed", "spec": spec("v1")});
    let created = ok(
        post(&api, CHARACTERS, Some("create"), &body).await,
        StatusCode::CREATED,
    )
    .await;
    let id = created["character"]["id"].as_str().unwrap().to_owned();
    let replay = post(&api, CHARACTERS, Some("create"), &body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay.headers()[header::LOCATION],
        format!("{CHARACTERS}/{id}").as_str()
    );
    assert_eq!(replay.json::<Value>().await.unwrap(), created);
    problem(
        post(&api, CHARACTERS, Some("create"), &json!({"name": "Other"})).await,
        StatusCode::CONFLICT,
        "idempotency-conflict",
    )
    .await;
    assert_eq!(
        api.get_json::<CharacterListResponse>(CHARACTERS)
            .await
            .characters
            .len(),
        1
    );

    let specs = format!("{CHARACTERS}/{id}/specs");
    let body = json!({"based_on": 1, "spec": spec("v2")});
    let version = ok(
        post(&api, &specs, Some("spec"), &body).await,
        StatusCode::CREATED,
    )
    .await;
    let replay = post(&api, &specs, Some("spec"), &body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<Value>().await.unwrap(), version);
    problem(
        post(
            &api,
            &specs,
            Some("spec"),
            &json!({"based_on": 1, "spec": spec("different")}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency-conflict",
    )
    .await;
    let after: SpecVersionListResponse = api.get_json(&specs).await;
    assert_eq!(after.versions.len(), 2);

    let references = format!("{CHARACTERS}/{id}/references");
    let body = json!({"upload_id": upload(&api).await, "role": "reference"});
    let reference = ok(
        post(&api, &references, Some("reference"), &body).await,
        StatusCode::CREATED,
    )
    .await;
    let replay = post(&api, &references, Some("reference"), &body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<Value>().await.unwrap(), reference);
    problem(
        post(
            &api,
            &references,
            Some("reference"),
            &json!({"upload_id": body["upload_id"], "role": "mask"}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency-conflict",
    )
    .await;
    assert_eq!(
        api.get_json::<ReferenceListResponse>(&references)
            .await
            .references
            .len(),
        1
    );

    let remove = format!("{references}/{}/remove", reference["id"].as_str().unwrap());
    let removed = ok(
        send(&api, Method::POST, &remove, Some("remove"), None).await,
        StatusCode::OK,
    )
    .await;
    let replay = ok(
        send(&api, Method::POST, &remove, Some("remove"), None).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(replay, removed);
    problem(
        send(
            &api,
            Method::POST,
            &format!("{references}/charref-other/remove"),
            Some("remove"),
            None,
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency-conflict",
    )
    .await;

    let (_, output) = completed_run(&api, Some(link(&id, 1))).await;
    let decisions = format!("{CHARACTERS}/{id}/decisions");
    let body = json!({"output_id": output, "spec_version": 1, "decision": "approved"});
    let decision = ok(
        post(&api, &decisions, Some("decision"), &body).await,
        StatusCode::CREATED,
    )
    .await;
    let replay = post(&api, &decisions, Some("decision"), &body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<Value>().await.unwrap(), decision);
    problem(
        post(
            &api,
            &decisions,
            Some("decision"),
            &json!({"output_id": output, "spec_version": 1, "decision": "rejected"}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency-conflict",
    )
    .await;
    assert_eq!(
        api.get_json::<DecisionListResponse>(&decisions)
            .await
            .decisions
            .len(),
        1
    );

    // A key belongs to one operation, wherever else it is tried.
    let endpoints = [
        ("create", CHARACTERS.to_owned(), json!({"name": "Reused"})),
        (
            "spec",
            specs.clone(),
            json!({"based_on": 2, "spec": spec("reused")}),
        ),
        (
            "reference",
            references.clone(),
            json!({"upload_id": "sha256:feed", "role": "mask"}),
        ),
        (
            "decision",
            decisions.clone(),
            json!({"output_id": output, "spec_version": 1, "decision": "cleared"}),
        ),
    ];
    for (own, path, body) in &endpoints {
        for (key, ..) in endpoints.iter().filter(|(key, ..)| key != own) {
            let response = post(&api, path, Some(key), body).await;
            problem(response, StatusCode::CONFLICT, "idempotency-conflict").await;
        }
    }
    problem(
        post(
            &api,
            &specs,
            Some("create"),
            &json!({"based_on": 2, "spec": spec("reused")}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency-conflict",
    )
    .await;
    assert_eq!(
        api.get_json::<CharacterListResponse>(CHARACTERS)
            .await
            .characters
            .len(),
        1
    );
    assert_eq!(
        api.get_json::<SpecVersionListResponse>(&specs)
            .await
            .versions
            .len(),
        2
    );

    // A failed write records no key, so a corrected retry with the same key is evaluated afresh.
    problem(
        post(
            &api,
            &specs,
            Some("stale"),
            &json!({"based_on": 1, "spec": spec("stale")}),
        )
        .await,
        StatusCode::CONFLICT,
        "character-spec-conflict",
    )
    .await;
    ok(
        post(
            &api,
            &specs,
            Some("stale"),
            &json!({"based_on": 2, "spec": spec("fresh")}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;

    for key in ["", "a,b", "white space", &"k".repeat(129)] {
        let response = post(&api, CHARACTERS, Some(key), &json!({"name": "Bad key"})).await;
        ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    }
    let response = api
        .request(Method::POST, CHARACTERS)
        .header("Idempotency-Key", "one")
        .header("Idempotency-Key", "two")
        .json(&json!({"name": "Two keys"}))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_problem(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(
        api.get_json::<CharacterListResponse>(CHARACTERS)
            .await
            .characters
            .len(),
        1
    );
    api.shutdown().await;
}

#[tokio::test]
async fn runs_link_to_a_character_version_and_record_it() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let id = character(&api, "Mara").await;
    ok(
        post(
            &api,
            &format!("{CHARACTERS}/{id}/specs"),
            None,
            &json!({"based_on": 1, "spec": spec("v2")}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;

    api.fake.complete.store(false, Ordering::SeqCst);
    let linked = submit(&api, &run_request(Some(link(&id, 2)))).await;
    assert_eq!(linked.status(), StatusCode::ACCEPTED);
    let linked: RunResponse = linked.json().await.unwrap();
    assert_eq!(
        serde_json::to_value(&linked.character).unwrap(),
        link(&id, 2)
    );
    let shown: Value = api.get_json(&format!("{RUNS}/{}", linked.id)).await;
    assert_eq!(shown["character"], link(&id, 2));
    let listed: RunListResponse = api.get_json(RUNS).await;
    let in_list = listed.runs.iter().find(|run| run.id == linked.id).unwrap();
    assert_eq!(
        serde_json::to_value(&in_list.character).unwrap(),
        link(&id, 2)
    );

    let record = api.core.repository().get(&linked.id).unwrap();
    let preflight: Value = serde_json::from_slice(
        &fs::read(std::path::Path::new(&record.output_dir).join("preflight.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(preflight["character"], link(&id, 2));
    api.fake.complete.store(true, Ordering::SeqCst);
    settle(&api, &linked.id).await;
    let manifest: Value = api
        .get_json(&format!("{RUNS}/{}/manifest", linked.id))
        .await;
    assert_eq!(manifest["character"], link(&id, 2));

    let plain: RunResponse = submit(&api, &run_request(None)).await.json().await.unwrap();
    settle(&api, &plain.id).await;
    let shown: Value = api.get_json(&format!("{RUNS}/{}", plain.id)).await;
    assert!(shown.get("character").is_none());
    let plain_manifest: Value = api.get_json(&format!("{RUNS}/{}/manifest", plain.id)).await;
    assert!(plain_manifest.get("character").is_none());
    let listed: RunListResponse = api.get_json(RUNS).await;
    assert!(listed
        .runs
        .iter()
        .find(|run| run.id == plain.id)
        .unwrap()
        .character
        .is_none());

    let of_character: RunListResponse = api.get_json(&format!("{CHARACTERS}/{id}/runs")).await;
    assert_eq!(of_character.runs.len(), 1);
    assert_eq!(of_character.runs[0].id, linked.id);
    let filtered: RunListResponse = api
        .get_json(&format!("{CHARACTERS}/{id}/runs?spec_version=1"))
        .await;
    assert!(filtered.runs.is_empty());
    let filtered: RunListResponse = api
        .get_json(&format!(
            "{CHARACTERS}/{id}/runs?spec_version=2&status=completed"
        ))
        .await;
    assert_eq!(filtered.runs.len(), 1);
    let filtered: RunListResponse = api
        .get_json(&format!("{CHARACTERS}/{id}/runs?status=failed"))
        .await;
    assert!(filtered.runs.is_empty());

    let rerun = api
        .request(Method::POST, &format!("{RUNS}/{}/rerun", linked.id))
        .header("Idempotency-Key", "rerun-linked")
        .send()
        .await
        .unwrap();
    assert_eq!(rerun.status(), StatusCode::CREATED);
    let rerun: RunResponse = rerun.json().await.unwrap();
    assert_eq!(
        serde_json::to_value(&rerun.character).unwrap(),
        link(&id, 2)
    );
    assert_eq!(
        api.get_json::<RunListResponse>(&format!("{CHARACTERS}/{id}/runs"))
            .await
            .runs
            .len(),
        2
    );
    api.shutdown().await;
}

#[tokio::test]
async fn an_invalid_link_writes_no_run_and_keeps_the_key_free() {
    let api = ApiHarness::start().await;
    install_workflow(&api);
    let id = character(&api, "Mara").await;
    let before = fs::read_dir(api.core.config().runs.clone())
        .map(|dir| dir.count())
        .unwrap_or(0);

    let missing = problem(
        post(
            &api,
            RUNS,
            Some("link"),
            &run_request(Some(link("char-nope", 1))),
        )
        .await,
        StatusCode::NOT_FOUND,
        "character-not-found",
    )
    .await;
    assert!(missing.recovery.contains("No run was written"));
    problem(
        post(&api, RUNS, Some("link"), &run_request(Some(link(&id, 2)))).await,
        StatusCode::NOT_FOUND,
        "character-spec-version-not-found",
    )
    .await;
    for version in [json!(1.0), json!("1"), json!(-1), json!(null)] {
        let response = post(
            &api,
            RUNS,
            Some("link"),
            &run_request(Some(json!({"id": id, "spec_version": version}))),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{version}"
        );
    }
    let response = post(
        &api,
        RUNS,
        Some("link"),
        &run_request(Some(json!({"id": id, "spec_version": 1, "x": 1}))),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(api.get_json::<RunListResponse>(RUNS).await.runs.is_empty());
    assert_eq!(api.fake.prompt_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        fs::read_dir(api.core.config().runs.clone())
            .map(|dir| dir.count())
            .unwrap_or(0),
        before
    );

    let accepted = post(&api, RUNS, Some("link"), &run_request(Some(link(&id, 1)))).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let replay = post(&api, RUNS, Some("link"), &run_request(Some(link(&id, 1)))).await;
    assert_eq!(replay.status(), StatusCode::OK);
    problem_or_conflict(post(&api, RUNS, Some("link"), &run_request(None)).await).await;

    let mut batch = json!({"workflow": "contract-workflow", "preset": "fast", "seeds": {"parameter": "seed", "values": [1, 2]}});
    batch["character"] = link(&id, 1);
    let response = post(&api, "/api/v1/batches/preview", None, &batch).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    api.shutdown().await;
}

async fn problem_or_conflict(response: Response) {
    ApiHarness::assert_problem(response, StatusCode::CONFLICT).await;
}

#[tokio::test]
async fn removing_a_reference_rejects_a_request_body_and_keeps_it_active() {
    let api = ApiHarness::start().await;
    let id = character(&api, "Mara").await;
    let references = format!("{CHARACTERS}/{id}/references");
    let upload_id = upload(&api).await;
    let added = ok(
        post(
            &api,
            &references,
            None,
            &json!({"upload_id": upload_id, "role": "sketch"}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let remove = format!("{references}/{}/remove", added["id"].as_str().unwrap());
    let rejected = ApiHarness::assert_problem(
        send(
            &api,
            Method::POST,
            &remove,
            None,
            Some(&json!({"note": "late"})),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert!(rejected.detail.contains("does not accept a request body"));
    let listed: ReferenceListResponse = api.get_json(&references).await;
    assert_eq!(listed.references.len(), 1);
    assert!(listed.references[0].removed_at.is_none());
    ok(
        send(&api, Method::POST, &remove, None, None).await,
        StatusCode::OK,
    )
    .await;
    api.shutdown().await;
}

#[tokio::test]
async fn characters_and_links_survive_a_restart() {
    let mut api = ApiHarness::start().await;
    install_workflow(&api);
    let created = ok(
        post(
            &api,
            CHARACTERS,
            Some("persist"),
            &json!({"name": "Mara", "spec": spec("v1")}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let id = created["character"]["id"].as_str().unwrap().to_owned();
    let specs = format!("{CHARACTERS}/{id}/specs");
    ok(
        post(
            &api,
            &specs,
            None,
            &json!({"based_on": 1, "spec": spec("v2")}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let references = format!("{CHARACTERS}/{id}/references");
    let added = ok(
        post(
            &api,
            &references,
            Some("ref"),
            &json!({"upload_id": upload(&api).await, "role": "sketch"}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    let removed = ok(
        send(
            &api,
            Method::POST,
            &format!("{references}/{}/remove", added["id"].as_str().unwrap()),
            None,
            None,
        )
        .await,
        StatusCode::OK,
    )
    .await;
    let (run, output) = completed_run(&api, Some(link(&id, 2))).await;
    let decisions = format!("{CHARACTERS}/{id}/decisions");
    let body = json!({"output_id": output, "spec_version": 2, "decision": "approved"});
    let decision = ok(
        post(&api, &decisions, Some("decide"), &body).await,
        StatusCode::CREATED,
    )
    .await;

    let paths = [
        CHARACTERS.to_owned(),
        format!("{CHARACTERS}/{id}"),
        specs.clone(),
        format!("{references}?include_removed=true"),
        decisions.clone(),
        format!("{CHARACTERS}/{id}/outputs"),
        format!("{CHARACTERS}/{id}/runs"),
        format!("{RUNS}/{}", run.id),
    ];
    let mut before = Vec::new();
    for path in &paths {
        before.push(api.get_json::<Value>(path).await);
    }
    api.restart().await;
    for (path, expected) in paths.iter().zip(&before) {
        assert_eq!(&api.get_json::<Value>(path).await, expected, "{path}");
    }
    assert_eq!(before[7]["character"], link(&id, 2));

    let replay = post(
        &api,
        CHARACTERS,
        Some("persist"),
        &json!({"name": "Mara", "spec": spec("v1")}),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<Value>().await.unwrap(), created);
    let replay = post(&api, &decisions, Some("decide"), &body).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<Value>().await.unwrap(), decision);
    let replay = send(
        &api,
        Method::POST,
        &format!("{references}/{}/remove", added["id"].as_str().unwrap()),
        None,
        None,
    )
    .await;
    assert_eq!(replay.json::<Value>().await.unwrap(), removed);
    problem(
        post(
            &api,
            &specs,
            None,
            &json!({"based_on": 1, "spec": spec("stale")}),
        )
        .await,
        StatusCode::CONFLICT,
        "character-spec-conflict",
    )
    .await;
    let next = ok(
        post(
            &api,
            &specs,
            None,
            &json!({"based_on": 2, "spec": spec("v3")}),
        )
        .await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(next["version"], 3);
    api.shutdown().await;
}
