mod common;

use std::fs;

use common::ApiHarness;
use genaimage_api::{
    client::{Client, Error},
    Template, TemplateCreateRequest, TemplateListResponse, TemplateRenderRequest,
    TemplateReplaceRequest, TemplateScope, TemplateVariableInput,
};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

fn portrait() -> Value {
    json!({
        "id": "portrait-studio",
        "title": "Studio portrait",
        "text": "A {{subject}} photographed in a studio, {{lighting}} lighting",
        "variables": {
            "subject": {"description": "Who or what", "required": true},
            "lighting": {"default": "soft"}
        }
    })
}

async fn create(api: &ApiHarness, body: &Value) -> Template {
    let response = api
        .request(Method::POST, "/api/v1/templates")
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json().await.unwrap()
}

async fn list(api: &ApiHarness) -> TemplateListResponse {
    api.get_json("/api/v1/templates").await
}

fn write_pack_template(api: &ApiHarness, pack: &str, declared: &[&str], files: &[(&str, &str)]) {
    let dir = api.core.config().packs[0].join(pack);
    fs::create_dir_all(dir.join("t")).unwrap();
    let declared: Vec<String> = declared.iter().map(|d| format!("'{d}'")).collect();
    fs::write(
        dir.join("pack.toml"),
        format!(
            "id = '{pack}'\nversion = '1.0.0'\ntemplates = [{}]\n",
            declared.join(", ")
        ),
    )
    .unwrap();
    for (name, content) in files {
        fs::write(dir.join(name), content).unwrap();
    }
}

#[tokio::test]
async fn an_empty_default_configuration_lists_nothing_and_creates_the_directory_on_first_write() {
    let api = ApiHarness::start().await;
    let directory = api.core.config().templates.clone();
    assert!(!directory.exists());
    let listing = list(&api).await;
    assert!(listing.templates.is_empty() && listing.diagnostics.is_empty());
    let created = create(&api, &portrait()).await;
    assert!(directory.join("portrait-studio.toml").is_file());
    assert_eq!(created.reference, "user:portrait-studio");
    assert_eq!(created.scope, TemplateScope::User);
    assert_eq!(created.pack_id, None);
    assert_eq!(
        serde_json::to_value(&created).unwrap(),
        json!({
            "ref": "user:portrait-studio",
            "scope": "user",
            "pack_id": null,
            "id": "portrait-studio",
            "title": "Studio portrait",
            "description": null,
            "variables": [
                {"name": "lighting", "description": null, "required": false, "default": "soft"},
                {"name": "subject", "description": "Who or what", "required": true, "default": null}
            ],
            "sha256": created.sha256,
            "text": "A {{subject}} photographed in a studio, {{lighting}} lighting"
        })
    );
    assert_eq!(list(&api).await.templates, std::slice::from_ref(&created));
    api.shutdown().await;
}

#[tokio::test]
async fn create_is_no_clobber_and_reports_validation_errors() {
    let api = ApiHarness::start().await;
    create(&api, &portrait()).await;
    let file = api.core.config().templates.join("portrait-studio.toml");
    let before = fs::read(&file).unwrap();
    let mut other = portrait();
    other["text"] = json!("changed {{subject}} {{lighting}}");
    let response = api
        .request(Method::POST, "/api/v1/templates")
        .json(&other)
        .send()
        .await
        .unwrap();
    ApiHarness::assert_typed_problem(
        response,
        StatusCode::CONFLICT,
        "urn:genaimage:problem:template-exists",
    )
    .await;
    assert_eq!(fs::read(&file).unwrap(), before);

    for id in ["../escape", "a/b", "Upper", "", "a.toml"] {
        let mut bad = portrait();
        bad["id"] = json!(id);
        let response = api
            .request(Method::POST, "/api/v1/templates")
            .json(&bad)
            .send()
            .await
            .unwrap();
        let problem = ApiHarness::assert_typed_problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "urn:genaimage:problem:template-invalid",
        )
        .await;
        assert_eq!(problem.errors[0].key, "id", "{id}");
    }
    let bad = json!({"id": "x", "title": "T", "text": "{{a}} {{b}}", "variables": {"a": {"required": true}, "c": {"required": true, "default": "d"}}});
    let response = api
        .request(Method::POST, "/api/v1/templates")
        .json(&bad)
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "urn:genaimage:problem:template-invalid",
    )
    .await;
    let found: Vec<(&str, &str)> = problem
        .errors
        .iter()
        .map(|e| (e.key.as_str(), e.reason.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            (
                "variables.c",
                "is required and also has a default; remove one"
            ),
            (
                "text",
                "uses the undeclared placeholder '{{b}}'; declare it under [variables.b]"
            ),
            ("variables.c", "is declared but not used in the text"),
        ]
    );
    assert!(!api.core.config().templates.join("x.toml").exists());
    assert!(!api
        .core
        .config()
        .templates
        .parent()
        .unwrap()
        .join("escape.toml")
        .exists());
    let unknown = json!({"id": "x", "title": "T", "text": "t", "surprise": 1});
    let response = api
        .request(Method::POST, "/api/v1/templates")
        .json(&unknown)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    api.shutdown().await;
}

#[tokio::test]
async fn put_with_a_stale_hash_conflicts_and_leaves_the_file_untouched() {
    let api = ApiHarness::start().await;
    let created = create(&api, &portrait()).await;
    let file = api.core.config().templates.join("portrait-studio.toml");
    let body = |sha: &str, text: &str| {
        json!({
            "expected_sha256": sha, "title": "New title", "text": text,
            "variables": {"who": {"required": true}}
        })
    };
    let response = api
        .request(Method::PUT, "/api/v1/templates/user/portrait-studio")
        .json(&body(&created.sha256, "Hello {{who}}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let replaced: Template = response.json().await.unwrap();
    let written = fs::read(&file).unwrap();
    assert_eq!(
        String::from_utf8(written.clone()).unwrap(),
        "id = \"portrait-studio\"\ntitle = \"New title\"\ntext = \"Hello {{who}}\"\n\n[variables.who]\nrequired = true\n"
    );
    assert_ne!(replaced.sha256, created.sha256);

    let response = api
        .request(Method::PUT, "/api/v1/templates/user/portrait-studio")
        .json(&body(&created.sha256, "Stale {{who}}"))
        .send()
        .await
        .unwrap();
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::CONFLICT,
        "urn:genaimage:problem:template-changed",
    )
    .await;
    assert_eq!(
        problem.current_sha256.as_deref(),
        Some(replaced.sha256.as_str())
    );
    assert_eq!(fs::read(&file).unwrap(), written);

    let response = api
        .request(Method::PUT, "/api/v1/templates/user/missing")
        .json(&body(&created.sha256, "x {{who}}"))
        .send()
        .await
        .unwrap();
    ApiHarness::assert_typed_problem(
        response,
        StatusCode::NOT_FOUND,
        "urn:genaimage:problem:template-not-found",
    )
    .await;
    api.shutdown().await;
}

#[tokio::test]
async fn delete_requires_the_current_hash() {
    let api = ApiHarness::start().await;
    let created = create(&api, &portrait()).await;
    let file = api.core.config().templates.join("portrait-studio.toml");
    let url = "/api/v1/templates/user/portrait-studio";
    let response = api.request(Method::DELETE, url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(file.exists());
    let stale = format!("{url}?expected_sha256={}", "0".repeat(64));
    let response = api.request(Method::DELETE, &stale).send().await.unwrap();
    ApiHarness::assert_typed_problem(
        response,
        StatusCode::CONFLICT,
        "urn:genaimage:problem:template-changed",
    )
    .await;
    assert!(file.exists());
    let current = format!("{url}?expected_sha256={}", created.sha256);
    let response = api.request(Method::DELETE, &current).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!file.exists());
    let response = api.request(Method::DELETE, &current).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = api
        .request(Method::GET, "/api/v1/templates/user/..%2Fx")
        .send()
        .await
        .unwrap();
    ApiHarness::assert_typed_problem(
        response,
        StatusCode::BAD_REQUEST,
        "urn:genaimage:problem:template-ref-invalid",
    )
    .await;
    api.shutdown().await;
}

async fn render(api: &ApiHarness, body: Value) -> reqwest::Response {
    api.request(Method::POST, "/api/v1/templates/render")
        .json(&body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn render_applies_defaults_keeps_empty_strings_and_is_pinned() {
    let api = ApiHarness::start().await;
    let created = create(&api, &portrait()).await;
    let ok = |variables: Value| json!({"ref": "user:portrait-studio", "sha256": created.sha256, "variables": variables});
    let response = render(&api, ok(json!({"subject": "cat"}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({
            "text": "A cat photographed in a studio, soft lighting",
            "ref": "user:portrait-studio",
            "sha256": created.sha256
        })
    );
    let body: Value = render(&api, ok(json!({"subject": " cat ", "lighting": ""})))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["text"], "A  cat  photographed in a studio,  lighting");

    for (variables, errors) in [
        (
            json!({}),
            json!([{"key": "subject", "reason": "is required and was not supplied"}]),
        ),
        (
            json!({"subject": "x", "nope": "y"}),
            json!([{"key": "nope", "reason": "is not a variable of this template"}]),
        ),
        (
            json!({"subject": 7}),
            json!([{"key": "subject", "reason": "must be a string"}]),
        ),
    ] {
        let response = render(&api, ok(variables)).await;
        let problem = ApiHarness::assert_typed_problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "urn:genaimage:problem:template-render-invalid",
        )
        .await;
        assert_eq!(serde_json::to_value(&problem.errors).unwrap(), errors);
    }

    let response = render(
        &api,
        json!({"ref": "user:portrait-studio", "sha256": "1".repeat(64), "variables": {"subject": "x"}}),
    )
    .await;
    let problem = ApiHarness::assert_typed_problem(
        response,
        StatusCode::CONFLICT,
        "urn:genaimage:problem:template-changed",
    )
    .await;
    assert_eq!(
        problem.current_sha256.as_deref(),
        Some(created.sha256.as_str())
    );
    for reference in ["user:../x", "nope", "pack:a"] {
        let response = render(&api, json!({"ref": reference, "sha256": created.sha256})).await;
        ApiHarness::assert_typed_problem(
            response,
            StatusCode::BAD_REQUEST,
            "urn:genaimage:problem:template-ref-invalid",
        )
        .await;
    }
    let response = render(&api, json!({"ref": "user:gone", "sha256": created.sha256})).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    api.shutdown().await;
}

#[tokio::test]
async fn pack_templates_are_listed_read_only_and_invalid_ones_become_diagnostics() {
    let api = ApiHarness::start().await;
    write_pack_template(
        &api,
        "demo",
        &["t/good.toml", "t/bad.toml", "t/notoml.md", "t/missing.toml"],
        &[
            (
                "t/good.toml",
                "id = 'pack-good'\ntitle = 'Good'\ntext = 'hi {{who}}'\n[variables.who]\nrequired = true\n",
            ),
            ("t/bad.toml", "id = 'pack-bad'\ntitle = 'Bad'\ntext = 'hi {{who}} {{'\n"),
            ("t/notoml.md", "# not a template\n"),
        ],
    );
    let listing = list(&api).await;
    assert_eq!(listing.templates.len(), 1);
    let pack = &listing.templates[0];
    assert_eq!(pack.reference, "pack:demo:t/good.toml");
    assert_eq!(pack.scope, TemplateScope::Pack);
    assert_eq!(pack.pack_id.as_deref(), Some("demo"));
    assert_eq!(pack.id, "pack-good");
    let found: Vec<(Option<&str>, &str)> = listing
        .diagnostics
        .iter()
        .map(|d| (d.reference.as_deref(), d.error.as_str()))
        .collect();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].0, Some("pack:demo:t/bad.toml"));
    assert!(
        found[0].1.starts_with("text: malformed '{{' at byte 11:"),
        "{found:?}"
    );
    assert_eq!(found[1].0, Some("pack:demo:t/notoml.md"));
    assert!(found[1].1.starts_with("file: "), "{found:?}");

    let one: Template = api
        .get_json("/api/v1/templates/pack/demo/t%2Fgood.toml")
        .await;
    assert_eq!(&one, pack);
    let body: Value = render(
        &api,
        json!({"ref": pack.reference, "sha256": pack.sha256, "variables": {"who": "you"}}),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(body["text"], "hi you");

    let url = "/api/v1/templates/pack/demo/t%2Fgood.toml";
    let response = api
        .request(Method::PUT, url)
        .json(&json!({"expected_sha256": pack.sha256, "title": "x", "text": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = api.request(Method::DELETE, url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = api
        .request(
            Method::DELETE,
            "/api/v1/templates/user/pack-good?expected_sha256=00",
        )
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let untouched =
        fs::read_to_string(api.core.config().packs[0].join("demo/t/good.toml")).unwrap();
    assert!(untouched.starts_with("id = 'pack-good'"));

    for gone in [
        "pack:demo:t/missing.toml",
        "pack:demo:../x",
        "pack:other:t/good.toml",
    ] {
        let response = render(&api, json!({"ref": gone, "sha256": pack.sha256})).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{gone}");
    }
    api.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_pack_template_symlinked_outside_the_pack_is_never_read() {
    let api = ApiHarness::start().await;
    let outside = api.workspace().join("outside.toml");
    fs::write(&outside, "id = 'leak'\ntitle = 'Leak'\ntext = 'secret'\n").unwrap();
    write_pack_template(&api, "demo", &["t/link.toml"], &[]);
    let _ = std::os::unix::fs::symlink(
        &outside,
        api.core.config().packs[0].join("demo/t/link.toml"),
    );
    let listing = list(&api).await;
    assert!(listing.templates.is_empty(), "{:?}", listing.templates);
    api.shutdown().await;
}

#[tokio::test]
async fn the_client_round_trips_every_operation() {
    let api = ApiHarness::start().await;
    let client = Client::with_base_url(&api.origin).unwrap();
    let request = TemplateCreateRequest {
        id: "c1".into(),
        title: "C".into(),
        description: Some("d".into()),
        text: "{{a}}".into(),
        variables: [(
            "a".to_owned(),
            TemplateVariableInput {
                required: true,
                ..Default::default()
            },
        )]
        .into(),
    };
    let created = client.create_template(&request).await.unwrap();
    assert!(matches!(
        client.create_template(&request).await,
        Err(Error::Problem(problem)) if problem.problem_type == "urn:genaimage:problem:template-exists"
    ));
    assert_eq!(client.template("user:c1").await.unwrap(), created);
    assert_eq!(
        client.templates().await.unwrap().templates,
        std::slice::from_ref(&created)
    );
    let rendered = client
        .render_template(&TemplateRenderRequest {
            reference: "user:c1".into(),
            sha256: created.sha256.clone(),
            variables: [("a".to_owned(), json!("v"))].into(),
        })
        .await
        .unwrap();
    assert_eq!(rendered.text, "v");
    let replaced = client
        .replace_template(
            "c1",
            &TemplateReplaceRequest {
                expected_sha256: created.sha256.clone(),
                title: "C2".into(),
                description: None,
                text: "{{a}}!".into(),
                variables: request.variables.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(replaced.title, "C2");
    assert!(client.delete_template("c1", &created.sha256).await.is_err());
    client
        .delete_template("c1", &replaced.sha256)
        .await
        .unwrap();
    assert!(client.templates().await.unwrap().templates.is_empty());
    api.shutdown().await;
}

#[tokio::test]
async fn a_pack_template_is_fetched_by_the_client_with_an_encoded_path() {
    let api = ApiHarness::start().await;
    write_pack_template(
        &api,
        "demo",
        &["t/good.toml"],
        &[("t/good.toml", "id = 'g'\ntitle = 'G'\ntext = 'plain'\n")],
    );
    let client = Client::with_base_url(&api.origin).unwrap();
    let template = client.template("pack:demo:t/good.toml").await.unwrap();
    assert_eq!(template.text, "plain");
    api.shutdown().await;
}
