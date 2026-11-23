use std::{
    io::Read,
    path::{Component, Path},
    time::Duration,
};

use axum::{
    extract::{rejection::JsonRejection, Path as AxumPath, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use genaimage_api::{
    WorkflowDetails, WorkflowDiagnostic, WorkflowListResponse, WorkflowNodeCheck,
    WorkflowNodeCheckStatus, WorkflowParameterError, WorkflowSummary, WorkflowValidationRequest,
    WorkflowValidationResponse,
};
use genaimage_core::LoadedWorkflow;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::{AppState, Problem};

const NODE_CHECK_DEADLINE: Duration = Duration::from_secs(2);
const MAX_THUMBNAIL_BYTES: u64 = 10 * 1024 * 1024;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/workflows", get(list))
        .route("/workflows/{id}", get(details))
        .route("/workflows/{id}/validate", post(validate))
        .route("/workflows/{id}/thumbnail", get(thumbnail))
}

fn internal_error(error: impl std::fmt::Display) -> Problem {
    Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        error.to_string(),
        "Check the workflow files and server logs.",
    )
}

fn not_found(id: &str) -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        format!("Workflow '{id}' was not found."),
        "Check the workflow ID and inspect the workflow catalogue diagnostics.",
    )
}

async fn load(state: &AppState, id: String) -> Result<LoadedWorkflow, Problem> {
    let harness = state.harness.clone();
    let search_id = id.clone();
    tokio::task::spawn_blocking(move || harness.load_workflow_by_id(&search_id))
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&id))
}

async fn list(State(state): State<AppState>) -> Result<Json<WorkflowListResponse>, Problem> {
    let harness = state.harness.clone();
    let discovery = tokio::task::spawn_blocking(move || harness.discover_workflows())
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;
    Ok(Json(WorkflowListResponse {
        workflows: discovery
            .manifests
            .into_iter()
            .map(|manifest| {
                let thumbnail_url = manifest
                    .thumbnail
                    .as_ref()
                    .filter(|_| !matches!(manifest.id.as_str(), "." | ".."))
                    .map(|_| {
                        format!(
                            "/api/v1/workflows/{}/thumbnail",
                            utf8_percent_encode(&manifest.id, NON_ALPHANUMERIC)
                        )
                    });
                WorkflowSummary {
                    id: manifest.id,
                    name: manifest.title,
                    description: manifest.description,
                    tags: manifest.tags,
                    category: manifest.category,
                    thumbnail_url,
                    version: manifest.version,
                }
            })
            .collect(),
        diagnostics: discovery
            .diagnostics
            .into_iter()
            .map(|item| WorkflowDiagnostic {
                path: item.path.display().to_string(),
                error: item.error.to_string(),
            })
            .collect(),
    }))
}

async fn details(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<WorkflowDetails>, Problem> {
    let workflow = load(&state, id).await?;
    let value = serde_json::to_value(workflow.manifest).map_err(internal_error)?;
    Ok(Json(serde_json::from_value(value).map_err(internal_error)?))
}

async fn validate(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<WorkflowValidationRequest>, JsonRejection>,
) -> Result<Json<WorkflowValidationResponse>, Problem> {
    let workflow = load(&state, id).await?;
    let Json(request) = request.map_err(|error| {
        Problem::new(
            error.status(),
            error.body_text(),
            "Send a JSON object with parameters and an optional preset.",
        )
    })?;
    let errors = workflow
        .validate_bindings(request.preset.as_deref(), &request.parameters)
        .map_err(|error| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                error.to_string(),
                "Select a preset declared by this workflow.",
            )
        })?;
    let parameter_errors: Vec<WorkflowParameterError> = errors
        .into_iter()
        .map(|error| WorkflowParameterError {
            parameter: error.parameter,
            reason: error.reason,
        })
        .collect();
    let node_check = match tokio::time::timeout(
        NODE_CHECK_DEADLINE,
        state.harness.missing_workflow_nodes(&workflow.workflow),
    )
    .await
    {
        Ok(Ok(missing_classes)) => WorkflowNodeCheck {
            status: if missing_classes.is_empty() {
                WorkflowNodeCheckStatus::Available
            } else {
                WorkflowNodeCheckStatus::Missing
            },
            missing_classes,
            reason: None,
        },
        Ok(Err(error)) => WorkflowNodeCheck {
            status: WorkflowNodeCheckStatus::Unavailable,
            missing_classes: Vec::new(),
            reason: Some(error.to_string()),
        },
        Err(_) => WorkflowNodeCheck {
            status: WorkflowNodeCheckStatus::Unavailable,
            missing_classes: Vec::new(),
            reason: Some("Node check timed out.".into()),
        },
    };
    Ok(Json(WorkflowValidationResponse {
        valid: parameter_errors.is_empty()
            && node_check.status == WorkflowNodeCheckStatus::Available,
        parameter_errors,
        node_check,
    }))
}

async fn thumbnail(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, Problem> {
    let workflow = load(&state, id.clone()).await?;
    let root = state.harness.config().workflows.clone();
    let image = tokio::task::spawn_blocking(move || read_thumbnail(&workflow, &root))
        .await
        .map_err(internal_error)?;
    let (content_type, bytes) = image.ok_or_else(|| {
        Problem::new(
            StatusCode::NOT_FOUND,
            format!("Thumbnail for workflow '{id}' is unavailable."),
            "Check that the manifest declares a supported image inside its workflow directory.",
        )
    })?;
    Ok(([(header::CONTENT_TYPE, content_type)], bytes))
}

fn read_thumbnail(workflow: &LoadedWorkflow, root: &Path) -> Option<(&'static str, Vec<u8>)> {
    let relative = Path::new(workflow.manifest.thumbnail.as_deref()?);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let mime = mime_guess::from_path(relative).first_raw()?;
    let content_type = match mime {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/avif" => "image/avif",
        _ => return None,
    };
    let manifest_dir = workflow.manifest_path.parent()?;
    #[cfg(unix)]
    let file = {
        let relative_dir = manifest_dir.strip_prefix(root).ok()?;
        let directory = open_directory(root)?;
        let directory = open_beneath(directory, relative_dir, true)?;
        open_beneath(directory, relative, false)?
    };
    #[cfg(not(unix))]
    let file = open_fallback(root, manifest_dir, relative)?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_THUMBNAIL_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_THUMBNAIL_BYTES {
        return None;
    }
    Some((content_type, bytes))
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Option<std::fs::File> {
    use rustix::fs::{open, openat, Mode, OFlags};
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    if !path.is_absolute() {
        return None;
    }
    let mut directory: std::fs::File = open("/", flags, Mode::empty()).ok()?.into();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = openat(&directory, name, flags, Mode::empty()).ok()?.into();
            }
            _ => return None,
        }
    }
    Some(directory)
}

#[cfg(unix)]
fn open_beneath(
    mut directory: std::fs::File,
    relative: &Path,
    final_directory: bool,
) -> Option<std::fs::File> {
    use rustix::fs::{openat, Mode, OFlags};
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if component == Component::CurDir {
                continue;
            }
            return None;
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if components.peek().is_some() || final_directory {
            flags |= OFlags::DIRECTORY;
        }
        directory = openat(&directory, name, flags, Mode::empty()).ok()?.into();
    }
    Some(directory)
}

// Other platforms lack the Unix descriptor-relative containment guarantee.
#[cfg(any(not(unix), test))]
fn open_fallback(root: &Path, manifest_dir: &Path, relative: &Path) -> Option<std::fs::File> {
    let root_path = root.canonicalize().ok()?;
    let directory = manifest_dir.canonicalize().ok()?;
    if !directory.starts_with(&root_path) {
        return None;
    }
    let candidate = manifest_dir.join(relative);
    let target = candidate.canonicalize().ok()?;
    if !target.starts_with(&directory) || !std::fs::metadata(&target).ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(&target).ok()?;
    if !file.metadata().ok()?.is_file()
        || root.canonicalize().ok()? != root_path
        || manifest_dir.canonicalize().ok()? != directory
        || candidate.canonicalize().ok()? != target
    {
        return None;
    }
    Some(file)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{open_beneath, open_directory, read_thumbnail};
    use std::{io::Read, path::Path};

    #[test]
    fn open_remains_confined_after_directory_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workflow");
        let original = temp.path().join("original");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("image.png"), b"inside").unwrap();
        std::fs::write(outside.join("image.png"), b"outside").unwrap();
        let directory = open_directory(&root).unwrap();
        std::fs::rename(&root, &original).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        let mut image = open_beneath(directory, Path::new("image.png"), false).unwrap();
        let mut contents = Vec::new();
        image.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"inside");
    }
    #[test]
    fn open_refuses_file_and_directory_symlink_swaps() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workflow");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("image.png"), b"outside").unwrap();
        let image = root.join("image.png");
        std::fs::write(&image, b"inside").unwrap();
        let directory = open_directory(&root).unwrap();
        assert!(image.canonicalize().unwrap().starts_with(&root));
        std::fs::remove_file(&image).unwrap();
        std::os::unix::fs::symlink(outside.join("image.png"), &image).unwrap();
        assert!(open_beneath(directory, Path::new("image.png"), false).is_none());

        let subdirectory = root.join("subdirectory");
        std::fs::create_dir(&subdirectory).unwrap();
        let directory = open_directory(&root).unwrap();
        std::fs::remove_dir(&subdirectory).unwrap();
        std::os::unix::fs::symlink(&outside, &subdirectory).unwrap();
        assert!(open_beneath(directory, Path::new("subdirectory/image.png"), false).is_none());
    }
    #[test]
    fn rejects_manifest_directory_swapped_after_loading() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workflows");
        let workflow = root.join("workflow");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(
            workflow.join("api.json"),
            br#"{ "1": {"class_type":"SaveImage", "inputs":{}} }"#,
        )
        .unwrap();
        std::fs::write(workflow.join("workflow.genaimage.json"), br#"{ "schema_version":2, "id":"test", "version":"1", "workflow":"api.json", "thumbnail":"image.png" }"#).unwrap();
        std::fs::write(outside.join("image.png"), b"outside").unwrap();
        let loaded =
            genaimage_core::LoadedWorkflow::load(&workflow.join("workflow.genaimage.json"))
                .unwrap();
        std::fs::rename(&workflow, root.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &workflow).unwrap();
        assert!(read_thumbnail(&loaded, &root).is_none());
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::open_fallback;
    use std::{io::Read, path::Path};

    #[test]
    fn portable_fallback_accepts_regular_files_and_rejects_escaping_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workflows");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("image.png"), b"inside").unwrap();
        std::fs::write(outside.join("image.png"), b"outside").unwrap();
        let mut file = open_fallback(&root, &root, Path::new("image.png")).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"inside");
        assert!(open_fallback(&root, &outside, Path::new("image.png")).is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("image.png"), root.join("link.png")).unwrap();
            assert!(open_fallback(&root, &root, Path::new("link.png")).is_none());
        }
        assert!(open_fallback(&root, &root, Path::new("missing.png")).is_none());
    }
}
