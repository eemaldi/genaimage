//! First-run service detection over a fixed, bounded candidate list.

use std::{net::SocketAddr, time::Duration};

use genaimage_api::{
    DetectStatus, DetectedService, DetectedServiceKind as Kind, LlmFlavour, SettingSource,
    SetupEndpoint, SetupReason, SetupReasonCode, SetupStatus,
};
use genaimage_core::{
    config::{validate_endpoint, validate_endpoint_addresses, SettingSource as Source},
    setup::{self, SetupAssessment},
    ResolvedConfig,
};
use reqwest::{header, redirect::Policy, Client, StatusCode, Url};
use serde_json::Value;

const PROBE_DEADLINE: Duration = Duration::from_millis(1500);
const MAX_BODY_BYTES: usize = 256 * 1024;

const COMFYUI_CANDIDATES: [(&str, &str); 3] = [
    ("ComfyUI", "http://127.0.0.1:8188"),
    ("ComfyUI (localhost)", "http://localhost:8188"),
    ("ComfyUI (Compose)", "http://comfyui:8188"),
];
const LLM_CANDIDATES: [(&str, &str); 4] = [
    ("Ollama", "http://127.0.0.1:11434/v1"),
    ("Ollama (Compose)", "http://ollama:11434/v1"),
    ("LM Studio", "http://127.0.0.1:1234/v1"),
    ("llama.cpp", "http://127.0.0.1:8080/v1"),
];

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub(crate) kind: Kind,
    pub(crate) label: String,
    pub(crate) url: String,
    pub(crate) configured: bool,
}

impl Candidate {
    fn key(&self) -> &'static str {
        match self.kind {
            Kind::Comfyui => "comfyui.base_url",
            Kind::Llm => "llm.base_url",
        }
    }

    fn allow_list(&self) -> &'static str {
        match self.kind {
            Kind::Comfyui => "comfyui.allowed_hosts",
            Kind::Llm => "llm.allowed_hosts",
        }
    }

    fn env_allow_list(&self) -> &'static str {
        match self.kind {
            Kind::Comfyui => "GENAIMAGE_COMFYUI_ALLOWED_HOSTS",
            Kind::Llm => "GENAIMAGE_LLM_ALLOWED_HOSTS",
        }
    }

    fn flavour(&self) -> Option<LlmFlavour> {
        (self.kind == Kind::Llm).then(|| {
            if Url::parse(&self.url).is_ok_and(|url| url.port() == Some(11434)) {
                LlmFlavour::Ollama
            } else {
                LlmFlavour::Openai
            }
        })
    }
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The fixed list plus the saved URLs. Nothing else is ever probed.
pub(crate) fn candidates(config: &ResolvedConfig) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (kind, fixed, configured) in [
        (
            Kind::Comfyui,
            &COMFYUI_CANDIDATES[..],
            &config.comfyui.base_url,
        ),
        (Kind::Llm, &LLM_CANDIDATES[..], &config.llm.base_url),
    ] {
        let mut label = "Configured".to_owned();
        let mut entries = Vec::new();
        for (name, url) in fixed {
            let is_configured = same_url(url, configured);
            if is_configured {
                label = (*name).to_owned();
            }
            entries.push(Candidate {
                kind,
                label: (*name).to_owned(),
                url: (*url).to_owned(),
                configured: is_configured,
            });
        }
        if !configured.is_empty() && !entries.iter().any(|entry| entry.configured) {
            entries.insert(
                0,
                Candidate {
                    kind,
                    label,
                    url: configured.clone(),
                    configured: true,
                },
            );
        }
        found.extend(entries);
    }
    found
}

pub(crate) fn status(config: &ResolvedConfig) -> SetupStatus {
    let SetupAssessment {
        needs_setup,
        reasons,
        config_file_exists,
        endpoints_env_owned,
    } = setup::assess(config);
    let endpoint = |key: &str, url: &str| SetupEndpoint {
        configured_url: url.to_owned(),
        source: match config.setting_source(key) {
            Source::Default => SettingSource::Default,
            Source::File(path) => SettingSource::File {
                file: path.to_string_lossy().into_owned(),
            },
            Source::Env(variable) => SettingSource::Env {
                variable: variable.into(),
            },
        },
    };
    SetupStatus {
        needs_setup,
        reasons: reasons
            .into_iter()
            .map(|reason| SetupReason {
                code: match reason.code {
                    setup::SetupReasonCode::NoConfigFile => SetupReasonCode::NoConfigFile,
                    setup::SetupReasonCode::InvalidConfig => SetupReasonCode::InvalidConfig,
                    setup::SetupReasonCode::EndpointsFromEnvironment => {
                        SetupReasonCode::EndpointsFromEnvironment
                    }
                    setup::SetupReasonCode::LlmNotConfigured => SetupReasonCode::LlmNotConfigured,
                    setup::SetupReasonCode::LlmTextModelUnset => SetupReasonCode::LlmTextModelUnset,
                },
                summary: reason.summary,
                blocking: reason.blocking,
                fix: reason.fix,
                items: reason.items,
            })
            .collect(),
        config_file: config.settings_file().to_string_lossy().into_owned(),
        config_file_exists,
        endpoints_env_owned,
        comfyui: endpoint("comfyui.base_url", &config.comfyui.base_url),
        llm: endpoint("llm.base_url", &config.llm.base_url),
    }
}

/// Probe every candidate concurrently. `api_key` is sent only to the configured LLM candidate.
pub(crate) async fn detect(
    candidates: Vec<Candidate>,
    comfyui_hosts: &[String],
    llm_hosts: &[String],
    api_key: Option<&str>,
) -> Vec<DetectedService> {
    futures_util::future::join_all(candidates.into_iter().map(|candidate| {
        let hosts = match candidate.kind {
            Kind::Comfyui => comfyui_hosts,
            Kind::Llm => llm_hosts,
        };
        let key = (candidate.kind == Kind::Llm && candidate.configured)
            .then_some(api_key)
            .flatten();
        probe(candidate, hosts, key)
    }))
    .await
}

fn result(
    candidate: &Candidate,
    status: DetectStatus,
    detail: impl Into<String>,
    fix: Option<String>,
) -> DetectedService {
    DetectedService {
        kind: candidate.kind,
        label: candidate.label.clone(),
        url: candidate.url.clone(),
        configured: candidate.configured,
        status,
        detail: detail.into(),
        fix,
        version: None,
        flavour: candidate.flavour(),
    }
}

async fn probe(candidate: Candidate, hosts: &[String], api_key: Option<&str>) -> DetectedService {
    let url = match validate_endpoint(candidate.key(), &candidate.url, hosts) {
        Ok(url) => url,
        Err(error) => return skipped(&candidate, &error.to_string()),
    };
    match tokio::time::timeout(PROBE_DEADLINE, request(&candidate, hosts, url, api_key)).await {
        Ok(found) => found,
        Err(_) => result(
            &candidate,
            DetectStatus::NotFound,
            "Nothing answered within 1.5 seconds.",
            Some(not_found_fix(&candidate)),
        ),
    }
}

fn skipped(candidate: &Candidate, reason: &str) -> DetectedService {
    let host = Url::parse(&candidate.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    let fix = match host {
        Some(host) if reason.contains("allowed_hosts") => format!(
            "Add \"{host}\" to {} (or set {}={host} in the environment), then detect again.",
            candidate.allow_list(),
            candidate.env_allow_list()
        ),
        _ => format!("Correct {} so it is a valid local URL.", candidate.key()),
    };
    result(
        candidate,
        DetectStatus::Skipped,
        format!("Not probed: {reason}"),
        Some(fix),
    )
}

fn not_found_fix(candidate: &Candidate) -> String {
    match candidate.kind {
        Kind::Comfyui => "Start ComfyUI, or enter its address manually.".into(),
        Kind::Llm => {
            "Start the LLM server, or enter its address manually. The assistant is optional.".into()
        }
    }
}

fn wrong_protocol(candidate: &Candidate, detail: impl Into<String>) -> DetectedService {
    let fix = match candidate.kind {
        Kind::Comfyui => "Check the port: something else answers here, not ComfyUI.",
        Kind::Llm => "Use the OpenAI-compatible base URL, usually ending in /v1.",
    };
    result(candidate, DetectStatus::Error, detail, Some(fix.into()))
}

async fn request(
    candidate: &Candidate,
    hosts: &[String],
    url: Url,
    api_key: Option<&str>,
) -> DetectedService {
    let host = url
        .host_str()
        .expect("validated host")
        .trim_matches(['[', ']']);
    let port = url.port_or_known_default().expect("validated scheme");
    let Ok(addresses) = tokio::net::lookup_host((host, port)).await else {
        return result(
            candidate,
            DetectStatus::NotFound,
            "The host name does not resolve.",
            Some(not_found_fix(candidate)),
        );
    };
    let addresses: Vec<SocketAddr> = addresses.collect();
    if let Err(error) =
        validate_endpoint_addresses(candidate.key(), &candidate.url, hosts, &addresses)
    {
        return skipped(candidate, &error.to_string());
    }
    let Ok(client) = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .resolve_to_addrs(url.host_str().expect("validated host"), &addresses)
        .timeout(PROBE_DEADLINE)
        .build()
    else {
        return result(
            candidate,
            DetectStatus::Error,
            "The probe client could not be built.",
            None,
        );
    };
    let mut base = url;
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    let path = match candidate.kind {
        Kind::Comfyui => "system_stats",
        Kind::Llm => "models",
    };
    let mut request = client
        .get(base.join(path).expect("static relative path"))
        .header(header::ACCEPT, "application/json");
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        request = request.bearer_auth(key);
    }
    let Ok(mut response) = request.send().await else {
        return result(
            candidate,
            DetectStatus::NotFound,
            "Nothing answered at this address.",
            Some(not_found_fix(candidate)),
        );
    };
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return result(
            candidate,
            DetectStatus::Error,
            "The server answered but requires an API key.",
            Some(if candidate.configured {
                "Check the LLM API key in the settings.".into()
            } else {
                "Enter this address manually, together with its API key.".into()
            }),
        );
    }
    if !response.status().is_success() {
        return wrong_protocol(
            candidate,
            format!(
                "Answered HTTP {} instead of the expected API.",
                response.status().as_u16()
            ),
        );
    }
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return wrong_protocol(candidate, "The answer is too large to be the expected API.");
        }
        body.extend_from_slice(&chunk);
    }
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return wrong_protocol(candidate, "Answered, but not with JSON.");
    };
    match candidate.kind {
        Kind::Comfyui => comfyui_answer(candidate, &value),
        Kind::Llm => llm_answer(candidate, &value),
    }
}

fn comfyui_answer(candidate: &Candidate, value: &Value) -> DetectedService {
    let Some(system) = value.get("system").and_then(Value::as_object) else {
        return wrong_protocol(
            candidate,
            "Answered JSON without the ComfyUI `system` object.",
        );
    };
    let version = system
        .get("comfyui_version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut found = result(
        candidate,
        DetectStatus::Found,
        match &version {
            Some(version) => format!("ComfyUI {version} answered."),
            None => "ComfyUI answered.".into(),
        },
        None,
    );
    found.version = version;
    found
}

fn llm_answer(candidate: &Candidate, value: &Value) -> DetectedService {
    let Some(models) = value.get("data").and_then(Value::as_array) else {
        return wrong_protocol(candidate, "Answered JSON that is not an OpenAI model list.");
    };
    if !models
        .iter()
        .all(|model| model.get("id").is_some_and(Value::is_string))
    {
        return wrong_protocol(
            candidate,
            "Answered a model list with entries lacking an `id`.",
        );
    }
    result(
        candidate,
        DetectStatus::Found,
        format!(
            "OpenAI-compatible server listing {} model(s).",
            models.len()
        ),
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use genaimage_core::HarnessConfig;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };

    use super::*;

    const SECRET: &str = "sk-setup-secret-4711";

    struct Fake {
        url: String,
        hits: Arc<Mutex<Vec<String>>>,
    }

    async fn fake(status: u16, content_type: &str, body: &str, delay: Duration) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let recorded = hits.clone();
        let reply = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (recorded, reply) = (recorded.clone(), reply.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buffer = [0; 1024];
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => head.extend_from_slice(&buffer[..read]),
                        }
                    }
                    recorded
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).to_lowercase());
                    tokio::time::sleep(delay).await;
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
        });
        Fake { url, hits }
    }

    async fn comfy(body: &str) -> Fake {
        fake(200, "application/json", body, Duration::ZERO).await
    }

    fn candidate(kind: Kind, url: &str, configured: bool) -> Candidate {
        Candidate {
            kind,
            label: "Test".into(),
            url: url.into(),
            configured,
        }
    }

    async fn run(
        candidates: Vec<Candidate>,
        hosts: &[String],
        key: Option<&str>,
    ) -> Vec<DetectedService> {
        timeout(
            Duration::from_secs(10),
            detect(candidates, hosts, hosts, key),
        )
        .await
        .expect("detection is bounded")
    }

    #[tokio::test]
    async fn comfyui_with_system_stats_is_found_with_its_version() {
        let server = comfy(r#"{"system":{"comfyui_version":"0.3.60"}}"#).await;
        let found = run(
            vec![candidate(Kind::Comfyui, &server.url, false)],
            &[],
            None,
        )
        .await;
        assert_eq!(found[0].status, DetectStatus::Found, "{found:?}");
        assert_eq!(found[0].version.as_deref(), Some("0.3.60"));
        assert!(server.hits.lock().unwrap()[0].starts_with("get /system_stats "));
    }

    #[tokio::test]
    async fn wrong_protocols_are_never_found() {
        for (kind, status, content_type, body) in [
            (Kind::Comfyui, 200, "text/html", "<html>hello</html>"),
            (Kind::Comfyui, 200, "application/json", r#"{"ok":true}"#),
            (Kind::Comfyui, 200, "application/json", r#"{"system":"x"}"#),
            (Kind::Comfyui, 404, "text/plain", "nope"),
            (Kind::Llm, 200, "text/html", "<html></html>"),
            (Kind::Llm, 200, "application/json", r#"{"data":"x"}"#),
            (
                Kind::Llm,
                200,
                "application/json",
                r#"{"data":[{"name":"a"}]}"#,
            ),
            (Kind::Llm, 500, "application/json", "{}"),
        ] {
            let server = fake(status, content_type, body, Duration::ZERO).await;
            let found = run(vec![candidate(kind, &server.url, false)], &[], None).await;
            assert_eq!(found[0].status, DetectStatus::Error, "{body}: {found:?}");
            assert!(found[0].fix.is_some());
        }
    }

    #[tokio::test]
    async fn openai_model_list_is_found_even_when_empty() {
        let server = comfy(r#"{"object":"list","data":[]}"#).await;
        let url = format!("{}/v1", server.url);
        let found = run(vec![candidate(Kind::Llm, &url, false)], &[], None).await;
        assert_eq!(found[0].status, DetectStatus::Found, "{found:?}");
        assert_eq!(found[0].flavour, Some(LlmFlavour::Openai));
        assert!(server.hits.lock().unwrap()[0].starts_with("get /v1/models "));
    }

    #[tokio::test]
    async fn silence_and_refusal_are_not_found() {
        let slow = fake(200, "application/json", "{}", Duration::from_secs(5)).await;
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let found = run(
            vec![
                candidate(Kind::Comfyui, &slow.url, false),
                candidate(Kind::Comfyui, &closed_url, false),
            ],
            &[],
            None,
        )
        .await;
        assert!(
            found
                .iter()
                .all(|found| found.status == DetectStatus::NotFound),
            "{found:?}"
        );
        assert!(found.iter().all(|found| found.fix.is_some()));
    }

    #[tokio::test]
    async fn api_key_reaches_only_the_configured_llm_and_never_the_output() {
        let configured = comfy(r#"{"data":[{"id":"m"}]}"#).await;
        let other = comfy(r#"{"data":[{"id":"m"}]}"#).await;
        let comfy_server = comfy(r#"{"system":{}}"#).await;
        let found = run(
            vec![
                candidate(Kind::Llm, &other.url, false),
                candidate(Kind::Llm, &configured.url, true),
                candidate(Kind::Comfyui, &comfy_server.url, true),
            ],
            &[],
            Some(SECRET),
        )
        .await;
        assert!(
            found
                .iter()
                .all(|found| found.status == DetectStatus::Found),
            "{found:?}"
        );
        assert!(configured.hits.lock().unwrap()[0].contains(&format!("bearer {SECRET}")));
        for server in [&other, &comfy_server] {
            let hits = server.hits.lock().unwrap();
            assert_eq!(hits.len(), 1);
            assert!(!hits[0].contains("authorization") && !hits[0].contains(SECRET));
        }
        assert!(!serde_json::to_string(&found).unwrap().contains(SECRET));
    }

    #[tokio::test]
    async fn disallowed_hosts_are_skipped_with_the_setting_to_add_and_not_contacted() {
        let found = run(
            vec![
                candidate(Kind::Comfyui, "http://comfyui:8188", false),
                candidate(Kind::Llm, "http://ollama:11434/v1", false),
                candidate(Kind::Comfyui, "http://8.8.8.8:8188", false),
            ],
            &[],
            None,
        )
        .await;
        assert!(
            found
                .iter()
                .all(|found| found.status == DetectStatus::Skipped),
            "{found:?}"
        );
        let fix = found[0].fix.as_deref().unwrap();
        assert!(
            fix.contains("comfyui.allowed_hosts")
                && fix.contains("GENAIMAGE_COMFYUI_ALLOWED_HOSTS=comfyui"),
            "{fix}"
        );
        let fix = found[1].fix.as_deref().unwrap();
        assert!(
            fix.contains("llm.allowed_hosts") && fix.contains("GENAIMAGE_LLM_ALLOWED_HOSTS=ollama"),
            "{fix}"
        );
        assert_eq!(found[1].flavour, Some(LlmFlavour::Ollama));
    }

    #[tokio::test]
    async fn allow_listed_compose_host_is_probed_not_skipped() {
        let hosts = vec!["comfyui".to_owned()];
        let found = run(
            vec![candidate(Kind::Comfyui, "http://comfyui:8188", false)],
            &hosts,
            None,
        )
        .await;
        assert_ne!(found[0].status, DetectStatus::Skipped, "{found:?}");
        assert_ne!(found[0].status, DetectStatus::Found, "{found:?}");
    }

    #[test]
    fn candidate_list_is_the_fixed_list_plus_the_saved_urls_without_duplicates() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().display().to_string();
        let load = |env: &'static [(&'static str, &'static str)]| {
            let vars: HashMap<&str, String> = env
                .iter()
                .map(|(k, v)| (*k, (*v).to_owned()))
                .chain(
                    ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"]
                        .into_iter()
                        .map(|name| (name, root.clone())),
                )
                .collect();
            HarnessConfig::load_with(temp.path(), None, temp.path(), |key| vars.get(key).cloned())
                .unwrap()
        };
        let urls = |config: &ResolvedConfig| -> Vec<(String, bool)> {
            candidates(config)
                .into_iter()
                .map(|candidate| (candidate.url, candidate.configured))
                .collect()
        };
        let defaults = urls(&load(&[]));
        assert_eq!(
            defaults.len(),
            COMFYUI_CANDIDATES.len() + LLM_CANDIDATES.len()
        );
        assert_eq!(
            defaults
                .iter()
                .filter(|(_, configured)| *configured)
                .count(),
            1
        );
        let custom = urls(&load(&[
            ("GENAIMAGE_COMFYUI_URL", "http://127.0.0.1:9999"),
            ("GENAIMAGE_LLM_URL", "http://127.0.0.1:11434/v1/"),
        ]));
        assert_eq!(custom[0], ("http://127.0.0.1:9999".to_owned(), true));
        assert_eq!(custom.len(), defaults.len() + 1);
        assert!(custom.contains(&("http://127.0.0.1:11434/v1".to_owned(), true)));
    }
}
