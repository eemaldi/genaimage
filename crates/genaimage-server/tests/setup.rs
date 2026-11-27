mod common;

use std::{fs, time::Duration};

use common::ApiHarness;
use genaimage_api::{
    client::Client, DetectStatus, DetectedServiceKind, SettingSource, SetupReasonCode,
};
use genaimage_test_support::{FakeLlmScript, FakeLlmServer};
use tokio::time::timeout;

const SECRET: &str = "sk-setup-api-4711";

fn client(api: &ApiHarness) -> Client {
    Client::with_base_url(&api.origin).unwrap()
}

fn remove_config_file(api: &ApiHarness) {
    fs::remove_file(api.core.config().settings_file()).unwrap();
}

#[tokio::test]
async fn config_file_exists_follows_the_disk_and_drives_needs_setup() {
    timeout(Duration::from_secs(30), async {
        let api = ApiHarness::start().await;
        let client = client(&api);
        assert!(client.settings().await.unwrap().config_file_exists);
        let status = client.setup_status().await.unwrap();
        assert!(
            status.config_file_exists && !status.needs_setup,
            "{status:?}"
        );
        assert!(!status.endpoints_env_owned);
        assert!(matches!(status.comfyui.source, SettingSource::File { .. }));

        remove_config_file(&api);
        assert!(!client.settings().await.unwrap().config_file_exists);
        let status = client.setup_status().await.unwrap();
        assert!(
            status.needs_setup && !status.config_file_exists,
            "{status:?}"
        );
        let reason = &status.reasons[0];
        assert_eq!(reason.code, SetupReasonCode::NoConfigFile);
        assert!(reason.blocking);
    })
    .await
    .expect("test is bounded");
}

#[tokio::test]
async fn environment_owned_endpoints_do_not_need_setup_without_a_file() {
    timeout(Duration::from_secs(30), async {
        let url = "http://127.0.0.1:1";
        let api = ApiHarness::start_with_config(5, &[("GENAIMAGE_COMFYUI_URL", url)]).await;
        remove_config_file(&api);
        let status = client(&api).setup_status().await.unwrap();
        assert!(!status.needs_setup, "{status:?}");
        assert!(status.endpoints_env_owned && !status.config_file_exists);
        assert_eq!(status.comfyui.configured_url, url);
        assert!(matches!(
            &status.comfyui.source,
            SettingSource::Env { variable } if variable == "GENAIMAGE_COMFYUI_URL"
        ));
        assert!(status.reasons.iter().all(|reason| !reason.blocking));
        assert!(status
            .reasons
            .iter()
            .any(|reason| reason.code == SetupReasonCode::EndpointsFromEnvironment));
    })
    .await
    .expect("test is bounded");
}

#[tokio::test]
async fn detect_reports_the_saved_endpoints_and_skips_unlisted_compose_hosts() {
    timeout(Duration::from_secs(30), async {
        let llm = FakeLlmServer::start(FakeLlmScript::default().with_models(["assistant"])).await;
        let url = llm.base_url();
        let api = ApiHarness::start_with_config(
            5,
            &[
                ("GENAIMAGE_LLM_URL", &url),
                ("GENAIMAGE_LLM_API_KEY", SECRET),
            ],
        )
        .await;
        let response = client(&api).detect_services().await.unwrap();
        let services = &response.services;
        let configured_comfyui = services
            .iter()
            .find(|service| service.kind == DetectedServiceKind::Comfyui && service.configured)
            .unwrap();
        assert_eq!(
            configured_comfyui.status,
            DetectStatus::Found,
            "{configured_comfyui:?}"
        );
        let configured_llm = services
            .iter()
            .find(|service| service.kind == DetectedServiceKind::Llm && service.configured)
            .unwrap();
        assert_eq!(
            configured_llm.status,
            DetectStatus::Found,
            "{configured_llm:?}"
        );
        for (host, setting) in [
            ("comfyui", "comfyui.allowed_hosts"),
            ("ollama", "llm.allowed_hosts"),
        ] {
            let compose = services
                .iter()
                .find(|service| service.url.contains(&format!("//{host}:")))
                .unwrap();
            assert_eq!(compose.status, DetectStatus::Skipped, "{compose:?}");
            assert!(compose.fix.as_deref().unwrap().contains(setting));
        }
        assert!(!serde_json::to_string(&response).unwrap().contains(SECRET));
    })
    .await
    .expect("test is bounded");
}

#[tokio::test]
async fn detect_refuses_cross_origin_browser_posts() {
    timeout(Duration::from_secs(30), async {
        let api = ApiHarness::start().await;
        let response = reqwest::Client::new()
            .post(format!("{}/api/v1/setup/detect", api.origin))
            .header("Origin", "http://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
    })
    .await
    .expect("test is bounded");
}

#[tokio::test]
async fn status_is_not_cached_and_reports_a_pending_restart_after_a_save() {
    timeout(Duration::from_secs(30), async {
        let api = ApiHarness::start().await;
        let url = format!("{}/api/v1/setup/status", api.origin);
        let response = reqwest::get(&url).await.unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");

        let client = client(&api);
        let before = client.setup_status().await.unwrap();
        assert!(before.restart_required.is_empty(), "{before:?}");
        let saved = api
            .request(reqwest::Method::PUT, "/api/v1/settings")
            .json(&serde_json::json!({"llm.api_key": SECRET}))
            .send()
            .await
            .unwrap();
        assert!(saved.status().is_success(), "{:?}", saved.text().await);
        let after = client.setup_status().await.unwrap();
        assert!(!after.needs_setup, "{after:?}");
        assert!(
            after.restart_required.contains(&"llm.api_key".to_owned()),
            "{after:?}"
        );
    })
    .await
    .expect("test is bounded");
}
