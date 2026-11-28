//! Async HTTP client for the loopback GenAImage server (requires the `client` feature).

mod assistant;
mod events;
mod media;
mod models;

pub use assistant::{AssistantEventStream, AssistantStreamEvent};
pub use events::{EventStream, EventsQuery, StreamEvent};
pub use media::{HeadEndpoint, OutputRequest};
pub use models::{ModelPullStream, PullStart};

use std::{error::Error as StdError, io, net::SocketAddr, time::Duration};

use reqwest::{header, Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use thiserror::Error;
use url::{Host, Url};

use crate::{
    AddReferenceRequest, CharacterListQuery, CharacterListResponse, CharacterOutputListResponse,
    CharacterOutputsQuery, CharacterResponse, CharacterRunsQuery, CreateCharacterRequest,
    CreateCharacterResponse, CreateSpecVersionRequest, DecisionListQuery, DecisionListResponse,
    DecisionResponse, DoctorReport, HealthResponse, OutputAnnotationListResponse,
    OutputAnnotationResponse, OutputListQuery, OutputListResponse, PackListResponse,
    PackValidationResponse, ProblemDetails, ReconcileRunResponse, ReconcileRunsResponse,
    ReconcileRunsWithOutcomesResponse, RecordDecisionRequest, ReferenceListQuery,
    ReferenceListResponse, ReferenceResponse, RunEventsQuery, RunEventsResponse, RunListQuery,
    RunListResponse, RunResponse, RunStatus, SettingsPatch, SettingsResponse, SettingsTestResponse,
    SetupDetectResponse, SetupStatus, SpecVersionListResponse, SpecVersionResponse,
    SubmitRunRequest, Template, TemplateCreateRequest, TemplateListResponse, TemplateRenderRequest,
    TemplateRenderResponse, TemplateReplaceRequest, UploadResponse, WorkflowDetails,
    WorkflowImportConfirmRequest, WorkflowImportProposal, WorkflowImportRequest,
    WorkflowImportResponse, WorkflowListResponse, WorkflowValidationRequest,
    WorkflowValidationResponse,
};

const IDEMPOTENCY_KEY: header::HeaderName = header::HeaderName::from_static("idempotency-key");

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3000";

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("Invalid server base URL {0:?}; use a loopback HTTP(S) origin, without a path.")]
    InvalidBaseUrl(String),
    #[error("Invalid resource ID {0:?}.")]
    InvalidId(String),
    #[error("Cannot connect to the GenAImage server at {base_url}. Start it with `genaimage serve` and check the address.")]
    ServerNotRunning {
        base_url: Url,
        #[source]
        source: reqwest::Error,
    },
    #[error("Could not establish an HTTP connection to the GenAImage server at {base_url}. Check the URL scheme and address: {source}")]
    Connect {
        base_url: Url,
        #[source]
        source: reqwest::Error,
    },
    #[error("{}: {} {}", .0.title, .0.detail, .0.recovery)]
    Problem(Box<ProblemDetails>),
    #[error("Unexpected HTTP status {status} from the GenAImage server.")]
    HttpStatus {
        status: StatusCode,
        #[source]
        source: Option<reqwest::Error>,
    },
    #[error("GenAImage HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Submission of run {} is unconfirmed. Reconcile this run instead of submitting it again.", .0.id)]
    UnknownSubmission(Box<RunResponse>),
    #[error("Submission returned failed run {}: {}", .0.id, .0.error.as_deref().unwrap_or("no failure detail supplied"))]
    FailedSubmission(Box<RunResponse>),
    #[error("Expected text/event-stream from the GenAImage server.")]
    UnexpectedEventContentType,
    #[error("Invalid GenAImage event stream: {0}")]
    EventStream(#[from] eventsource_stream::EventStreamError<reqwest::Error>),
    #[error("Invalid JSON in a GenAImage event: {0}")]
    EventJson(#[from] serde_json::Error),
    #[error("Unknown assistant SSE event {0:?}.")]
    UnknownAssistantEvent(String),
}

/// A reusable client and connection pool. Cloning shares that pool.
#[derive(Clone, Debug)]
pub struct Client {
    base_url: Url,
    http: reqwest::Client,
}

impl Client {
    /// Constructs a client for the default loopback server address.
    pub fn new() -> Result<Self, Error> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Returns the validated loopback server origin used by this client.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Constructs a client for a custom loopback origin, for example a different port.
    pub fn with_base_url(base_url: &str) -> Result<Self, Error> {
        let invalid = || Error::InvalidBaseUrl(base_url.into());
        let base_url = Url::parse(base_url).map_err(|_| invalid())?;
        let loopback = match base_url.host() {
            Some(Host::Domain(name)) => name == "localhost",
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if !loopback
            || !matches!(base_url.scheme(), "http" | "https")
            || base_url.path() != "/"
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
        {
            return Err(invalid());
        }
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .no_proxy()
            .resolve_to_addrs(
                "localhost",
                &[
                    SocketAddr::from(([127, 0, 0, 1], 0)),
                    SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
                ],
            )
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()?;
        Ok(Self { base_url, http })
    }

    pub async fn health(&self) -> Result<HealthResponse, Error> {
        self.json(self.get(&["health"])?).await
    }

    /// Runs the server's diagnostics. They probe ComfyUI and the LLM, so allow several seconds.
    pub async fn doctor(&self) -> Result<DoctorReport, Error> {
        self.json(self.get(&["doctor"])?).await
    }

    /// Fetches the OpenAPI document as a JSON object, preserving all schema fields.
    pub async fn openapi(&self) -> Result<Map<String, Value>, Error> {
        self.json(self.get(&["openapi.json"])?).await
    }

    pub async fn workflows(&self) -> Result<WorkflowListResponse, Error> {
        self.json(self.get(&["workflows"])?).await
    }

    /// Forces a workflow catalogue rebuild, including every pack, and returns the listing.
    pub async fn refresh_workflows(&self) -> Result<WorkflowListResponse, Error> {
        self.json(self.request(Method::POST, &["workflows", "refresh"])?)
            .await
    }

    /// Analyses an API-format graph and proposes a manifest. Writes nothing.
    pub async fn import_workflow(
        &self,
        body: &WorkflowImportRequest,
    ) -> Result<WorkflowImportProposal, Error> {
        self.json(
            self.request(Method::POST, &["workflows", "import"])?
                .json(body),
        )
        .await
    }

    /// Writes a confirmed import as a new user workflow. Never overwrites; an
    /// identical repeat returns the same result.
    pub async fn confirm_workflow_import(
        &self,
        body: &WorkflowImportConfirmRequest,
    ) -> Result<WorkflowImportResponse, Error> {
        self.json(
            self.request(Method::POST, &["workflows", "import", "confirm"])?
                .json(body),
        )
        .await
    }

    pub async fn packs(&self) -> Result<PackListResponse, Error> {
        self.json(self.get(&["packs"])?).await
    }

    /// Checks a pack's requirements against ComfyUI. Never installs anything.
    pub async fn validate_pack(&self, id: &str) -> Result<PackValidationResponse, Error> {
        self.json(self.get(&["packs", id, "validate"])?).await
    }

    /// User and pack prompt templates, with diagnostics for unusable ones.
    pub async fn templates(&self) -> Result<TemplateListResponse, Error> {
        self.json(self.get(&["templates"])?).await
    }

    /// One template by ref: `user:<id>` or `pack:<pack_id>:<relative path>`.
    pub async fn template(&self, reference: &str) -> Result<Template, Error> {
        match reference.strip_prefix("user:") {
            Some(id) => self.json(self.get(&["templates", "user", id])?).await,
            None => match reference
                .strip_prefix("pack:")
                .and_then(|rest| rest.split_once(':'))
            {
                Some((pack_id, path)) => {
                    self.json(self.get(&["templates", "pack", pack_id, path])?)
                        .await
                }
                None => Err(Error::InvalidId(reference.into())),
            },
        }
    }

    /// Creates a user template. Never overwrites; an existing id is a 409 problem.
    pub async fn create_template(&self, body: &TemplateCreateRequest) -> Result<Template, Error> {
        self.json(self.request(Method::POST, &["templates"])?.json(body))
            .await
    }

    /// Replaces a user template, pinned to the `sha256` the caller reviewed.
    pub async fn replace_template(
        &self,
        id: &str,
        body: &TemplateReplaceRequest,
    ) -> Result<Template, Error> {
        self.json(
            self.request(Method::PUT, &["templates", "user", id])?
                .json(body),
        )
        .await
    }

    /// Deletes a user template, pinned to the `sha256` the caller reviewed.
    pub async fn delete_template(&self, id: &str, expected_sha256: &str) -> Result<(), Error> {
        let request = self
            .request(Method::DELETE, &["templates", "user", id])?
            .query(&[("expected_sha256", expected_sha256)]);
        self.send(request, false).await?;
        Ok(())
    }

    /// Renders the reviewed template version. Rendering submits nothing.
    pub async fn render_template(
        &self,
        body: &TemplateRenderRequest,
    ) -> Result<TemplateRenderResponse, Error> {
        self.json(
            self.request(Method::POST, &["templates", "render"])?
                .json(body),
        )
        .await
    }

    pub async fn workflow(&self, id: &str) -> Result<WorkflowDetails, Error> {
        self.json(self.get(&["workflows", id])?).await
    }

    pub async fn validate_workflow(
        &self,
        id: &str,
        body: &WorkflowValidationRequest,
    ) -> Result<WorkflowValidationResponse, Error> {
        self.json(
            self.request(Method::POST, &["workflows", id, "validate"])?
                .json(body),
        )
        .await
    }

    pub async fn runs(&self, query: &RunListQuery) -> Result<RunListResponse, Error> {
        self.json(self.get(&["runs"])?.query(query)).await
    }

    /// Sends one submission with the caller's idempotency key. On an uncertain
    /// outcome, inspect or reconcile the run rather than submitting a new one.
    pub async fn submit_run(
        &self,
        idempotency_key: &str,
        body: &SubmitRunRequest,
    ) -> Result<RunResponse, Error> {
        let run: RunResponse = self
            .json(
                self.request(Method::POST, &["runs"])?
                    .header("idempotency-key", idempotency_key)
                    .json(body),
            )
            .await?;
        Self::submission_result(run)
    }

    /// Creates a new run from verified provenance. Keep the same key when checking an uncertain response.
    pub async fn rerun_run(&self, id: &str, idempotency_key: &str) -> Result<RunResponse, Error> {
        let run = self
            .json(
                self.request(Method::POST, &["runs", id, "rerun"])?
                    .header("idempotency-key", idempotency_key),
            )
            .await?;
        Self::submission_result(run)
    }

    fn submission_result(run: RunResponse) -> Result<RunResponse, Error> {
        match run.status {
            RunStatus::Created | RunStatus::Submitting | RunStatus::UnknownSubmission => {
                Err(Error::UnknownSubmission(Box::new(run)))
            }
            RunStatus::Failed => Err(Error::FailedSubmission(Box::new(run))),
            _ => Ok(run),
        }
    }

    pub async fn run(&self, id: &str) -> Result<RunResponse, Error> {
        self.json(self.get(&["runs", id])?).await
    }

    pub async fn run_events(
        &self,
        id: &str,
        query: &RunEventsQuery,
    ) -> Result<RunEventsResponse, Error> {
        self.json(self.get(&["runs", id, "events"])?.query(query))
            .await
    }

    pub async fn cancel_run(&self, id: &str) -> Result<RunResponse, Error> {
        self.json(self.request(Method::POST, &["runs", id, "cancel"])?)
            .await
    }

    pub async fn reconcile_run(&self, id: &str) -> Result<RunResponse, Error> {
        self.json(self.request(Method::POST, &["runs", id, "reconcile"])?)
            .await
    }

    pub async fn reconcile_runs(&self) -> Result<ReconcileRunsResponse, Error> {
        self.json(self.request(Method::POST, &["runs", "reconcile"])?)
            .await
    }

    pub async fn reconcile_run_with_outcome(
        &self,
        id: &str,
    ) -> Result<ReconcileRunResponse, Error> {
        self.json(self.request(Method::POST, &["runs", id, "reconcile"])?)
            .await
    }

    pub async fn reconcile_runs_with_outcomes(
        &self,
    ) -> Result<ReconcileRunsWithOutcomesResponse, Error> {
        self.json(self.request(Method::POST, &["runs", "reconcile"])?)
            .await
    }

    /// Creates a character. With a key, a retry returns the character written the first time.
    pub async fn create_character(
        &self,
        idempotency_key: Option<&str>,
        body: &CreateCharacterRequest,
    ) -> Result<CreateCharacterResponse, Error> {
        let request = self.request(Method::POST, &["characters"])?.json(body);
        self.json(optional_header(request, IDEMPOTENCY_KEY, idempotency_key))
            .await
    }

    pub async fn characters(
        &self,
        query: &CharacterListQuery,
    ) -> Result<CharacterListResponse, Error> {
        self.json(self.get(&["characters"])?.query(query)).await
    }

    pub async fn character(&self, id: &str) -> Result<CharacterResponse, Error> {
        self.json(self.get(&["characters", id])?).await
    }

    pub async fn character_specs(
        &self,
        id: &str,
        query: &CharacterListQuery,
    ) -> Result<SpecVersionListResponse, Error> {
        self.json(self.get(&["characters", id, "specs"])?.query(query))
            .await
    }

    /// Appends a spec version. A stale `based_on` is a 409 problem with `latest`.
    pub async fn create_character_spec(
        &self,
        id: &str,
        idempotency_key: Option<&str>,
        body: &CreateSpecVersionRequest,
    ) -> Result<SpecVersionResponse, Error> {
        let request = self
            .request(Method::POST, &["characters", id, "specs"])?
            .json(body);
        self.json(optional_header(request, IDEMPOTENCY_KEY, idempotency_key))
            .await
    }

    pub async fn character_spec(
        &self,
        id: &str,
        version: u32,
    ) -> Result<SpecVersionResponse, Error> {
        self.json(self.get(&["characters", id, "specs", &version.to_string()])?)
            .await
    }

    pub async fn character_references(
        &self,
        id: &str,
        query: &ReferenceListQuery,
    ) -> Result<ReferenceListResponse, Error> {
        self.json(self.get(&["characters", id, "references"])?.query(query))
            .await
    }

    pub async fn add_character_reference(
        &self,
        id: &str,
        idempotency_key: Option<&str>,
        body: &AddReferenceRequest,
    ) -> Result<ReferenceResponse, Error> {
        let request = self
            .request(Method::POST, &["characters", id, "references"])?
            .json(body);
        self.json(optional_header(request, IDEMPOTENCY_KEY, idempotency_key))
            .await
    }

    /// Soft-removes a reference; the record stays.
    pub async fn remove_character_reference(
        &self,
        id: &str,
        reference_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ReferenceResponse, Error> {
        let request = self.request(
            Method::POST,
            &["characters", id, "references", reference_id, "remove"],
        )?;
        self.json(optional_header(request, IDEMPOTENCY_KEY, idempotency_key))
            .await
    }

    pub async fn character_decisions(
        &self,
        id: &str,
        query: &DecisionListQuery,
    ) -> Result<DecisionListResponse, Error> {
        self.json(self.get(&["characters", id, "decisions"])?.query(query))
            .await
    }

    pub async fn record_character_decision(
        &self,
        id: &str,
        idempotency_key: Option<&str>,
        body: &RecordDecisionRequest,
    ) -> Result<DecisionResponse, Error> {
        let request = self
            .request(Method::POST, &["characters", id, "decisions"])?
            .json(body);
        self.json(optional_header(request, IDEMPOTENCY_KEY, idempotency_key))
            .await
    }

    /// Current approved or rejected outputs, each with its output metadata.
    pub async fn character_outputs(
        &self,
        id: &str,
        query: &CharacterOutputsQuery,
    ) -> Result<CharacterOutputListResponse, Error> {
        self.json(self.get(&["characters", id, "outputs"])?.query(query))
            .await
    }

    pub async fn character_runs(
        &self,
        id: &str,
        query: &CharacterRunsQuery,
    ) -> Result<RunListResponse, Error> {
        self.json(self.get(&["characters", id, "runs"])?.query(query))
            .await
    }

    pub async fn outputs(&self, query: &OutputListQuery) -> Result<OutputListResponse, Error> {
        self.json(self.get(&["outputs"])?.query(query)).await
    }

    /// Reviews one output with the server's pinned vision role and stores a model-opinion annotation.
    pub async fn review_output(&self, id: &str) -> Result<OutputAnnotationResponse, Error> {
        self.json(self.request(Method::POST, &["outputs", id, "review"])?)
            .await
    }

    /// Lists the newest 100 model-opinion annotations for an output.
    pub async fn output_annotations(
        &self,
        id: &str,
    ) -> Result<OutputAnnotationListResponse, Error> {
        self.json(self.get(&["outputs", id, "annotations"])?).await
    }

    /// Sends the image bytes as the server's `file` multipart field.
    pub async fn upload(&self, filename: &str, image: Vec<u8>) -> Result<UploadResponse, Error> {
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(image).file_name(filename.to_owned()),
        );
        self.json(self.request(Method::POST, &["uploads"])?.multipart(form))
            .await
    }

    pub async fn upload_metadata(&self, id: &str) -> Result<UploadResponse, Error> {
        self.json(self.get(&["uploads", id])?).await
    }

    /// Offline first-run assessment of the saved configuration.
    pub async fn setup_status(&self) -> Result<SetupStatus, Error> {
        self.json(self.get(&["setup", "status"])?).await
    }

    /// Probes a fixed candidate list and the saved endpoints; allow a few seconds.
    pub async fn detect_services(&self) -> Result<SetupDetectResponse, Error> {
        self.json(self.request(Method::POST, &["setup", "detect"])?)
            .await
    }

    pub async fn settings(&self) -> Result<SettingsResponse, Error> {
        self.json(self.get(&["settings"])?).await
    }

    pub async fn save_settings(&self, patch: &SettingsPatch) -> Result<SettingsResponse, Error> {
        self.json(self.request(Method::PUT, &["settings"])?.json(patch))
            .await
    }

    pub async fn test_settings(
        &self,
        patch: &SettingsPatch,
    ) -> Result<SettingsTestResponse, Error> {
        self.json(
            self.request(Method::POST, &["settings", "test"])?
                .json(patch),
        )
        .await
    }

    fn get(&self, segments: &[&str]) -> Result<RequestBuilder, Error> {
        self.request(Method::GET, segments)
    }

    fn request(&self, method: Method, segments: &[&str]) -> Result<RequestBuilder, Error> {
        if let Some(id) = segments
            .iter()
            .find(|&&part| matches!(part, "" | "." | ".."))
        {
            return Err(Error::InvalidId((*id).into()));
        }
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .expect("an HTTP origin has path segments")
            .extend(["api", "v1"])
            .extend(segments);
        Ok(self.http.request(method, url))
    }

    async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, Error> {
        Ok(self.send(request, false).await?.json().await?)
    }

    async fn send(
        &self,
        request: RequestBuilder,
        allow_not_modified: bool,
    ) -> Result<Response, Error> {
        let request = request.build()?;
        let head = request.method() == Method::HEAD;
        let response = self.execute(request).await?;
        Self::check_status(response, allow_not_modified, head).await
    }

    async fn execute(&self, request: reqwest::Request) -> Result<Response, Error> {
        self.http.execute(request).await.map_err(|source| {
            if !source.is_connect() {
                Error::Http(source)
            } else if connection_refused(&source) {
                Error::ServerNotRunning {
                    base_url: self.base_url.clone(),
                    source,
                }
            } else {
                Error::Connect {
                    base_url: self.base_url.clone(),
                    source,
                }
            }
        })
    }

    async fn check_status(
        response: Response,
        allow_not_modified: bool,
        head: bool,
    ) -> Result<Response, Error> {
        let status = response.status();
        if status.is_success() || (allow_not_modified && status == StatusCode::NOT_MODIFIED) {
            return Ok(response);
        }
        let source = if !head && has_content_type(&response, "application/problem+json") {
            match response.json().await {
                Ok(problem) => return Err(Error::Problem(problem)),
                Err(error) => Some(error),
            }
        } else {
            None
        };
        Err(Error::HttpStatus { status, source })
    }
}

fn connection_refused(error: &reqwest::Error) -> bool {
    let mut cause = error.source();
    while let Some(error) = cause {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::ConnectionRefused)
        {
            return true;
        }
        cause = error.source();
    }
    false
}

fn has_content_type(response: &Response, expected: &str) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

fn optional_header(
    request: RequestBuilder,
    name: header::HeaderName,
    value: Option<&str>,
) -> RequestBuilder {
    match value {
        Some(value) => request.header(name, value),
        None => request,
    }
}
