use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{safe_file, HarnessError, Result};

pub const SYSTEM_PROMPT_TEMPLATE: &str = "system";

pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are the local GenAImage assistant.

Help the user describe and refine image-generation choices. Be explicit about models, workflows, prompts, images, seeds and other settings. Never silently substitute any choice, claim that a run was submitted, or treat a technically successful generation as creative approval. Treat your output as an editable proposal. The user must explicitly confirm any workflow submission or tool action that requires confirmation.

Use the read-only tools to see what the user could see: list_workflows, inspect_workflow, list_checkpoints, list_runs, list_outputs, list_characters, get_character, get_character_spec and list_annotations. A preset marked as a layout in preset_info sets the canvas size or arrangement; presets apply only when a run names them. A checkpoint profile is never inferred from a file name: name a profile only if inspect_workflow or list_checkpoints lists it for that exact checkpoint, and give its revision. Image inputs, including character references, are always chosen by the user, never by you. Annotations are model opinions, not approval. get_character returns only a summary of the spec; read the content with get_character_spec and follow next_cursor until complete is true. Build a proposal from a character spec only after reading it to complete: true, and always state which spec version you used. If profile_pack_problems is present, say that profiles may have failed to load; an empty profile list is then not proof that none exists. If a tool reports that a service is unreachable, that a list was truncated or that a page was shortened, say so and request the next page; never present a partial view as complete or as an empty result.
"#;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptTemplate {
    pub name: String,
    pub content: String,
    pub sha256: String,
}

#[derive(Clone)]
pub struct PromptTemplates {
    data_dir: PathBuf,
    builtins: Arc<BTreeMap<String, String>>,
}

impl PromptTemplates {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let mut templates = Self {
            data_dir: data_dir.into(),
            builtins: Arc::new(BTreeMap::new()),
        };
        templates
            .register_builtin(SYSTEM_PROMPT_TEMPLATE, DEFAULT_SYSTEM_PROMPT)
            .expect("the built-in system template name is valid");
        templates
    }

    pub fn register_builtin(&mut self, name: &str, content: &str) -> Result<()> {
        validate_name(name)?;
        let mut builtins = (*self.builtins).clone();
        if builtins
            .insert(name.to_owned(), content.to_owned())
            .is_some()
        {
            return Err(HarnessError::Configuration(format!(
                "assistant prompt template '{name}' is already registered"
            )));
        }
        self.builtins = Arc::new(builtins);
        Ok(())
    }

    pub async fn load(&self, name: &str) -> Result<PromptTemplate> {
        validate_name(name)?;
        let name = name.to_owned();
        let builtin = self.builtins.get(&name).cloned().ok_or_else(|| {
            HarnessError::Configuration(format!(
                "assistant prompt template '{name}' has no built-in default"
            ))
        })?;
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || load_blocking(&data_dir, &name, builtin.as_bytes()))
            .await
            .map_err(|error| {
                HarnessError::Configuration(format!(
                    "assistant prompt template task failed: {error}"
                ))
            })?
    }
}

fn load_blocking(data_dir: &Path, name: &str, builtin: &[u8]) -> Result<PromptTemplate> {
    fs::create_dir_all(data_dir)?;
    let root = data_dir.canonicalize()?;
    let assistant_dir = root.join("assistant");
    ensure_real_directory(&root, &assistant_dir)?;
    let prompts_dir = assistant_dir.join("prompts");
    ensure_real_directory(&root, &prompts_dir)?;
    if !prompts_dir.canonicalize()?.starts_with(&root) {
        return Err(HarnessError::Configuration(format!(
            "assistant prompt directory '{}' escapes the data directory",
            prompts_dir.display()
        )));
    }

    let filename = format!("{name}.md");
    let path = prompts_dir.join(&filename);
    if safe_file::open_regular_beneath(&root, &prompts_dir, Path::new(&filename)).is_none() {
        let temporary = prompts_dir.join(format!(".{name}-{}.tmp", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(builtin)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error.into());
            }
        }
        let _ = fs::remove_file(temporary);
    }

    let mut file = safe_file::open_regular_beneath(&root, &prompts_dir, Path::new(&filename))
        .ok_or_else(|| {
            HarnessError::Configuration(format!(
                "assistant prompt template '{}' is not a regular file beneath the data directory",
                path.display()
            ))
        })?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let sha256 = hex::encode(Sha256::digest(content.as_bytes()));
    Ok(PromptTemplate {
        name: name.to_owned(),
        content,
        sha256,
    })
}

fn ensure_real_directory(root: &Path, path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(HarnessError::Configuration(format!(
                "assistant prompt path '{}' is not a regular directory",
                path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.file_type().is_dir() {
                return Err(HarnessError::Configuration(format!(
                    "assistant prompt path '{}' is not a regular directory",
                    path.display()
                )));
            }
        }
        Err(error) => return Err(error.into()),
    }
    if path.canonicalize()?.starts_with(root) {
        Ok(())
    } else {
        Err(HarnessError::Configuration(format!(
            "assistant prompt path '{}' escapes the data directory",
            path.display()
        )))
    }
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        });
    if valid {
        Ok(())
    } else {
        Err(HarnessError::Configuration(
            "assistant prompt template names must contain 1-64 lower-case letters, digits, '-' or '_'"
                .into(),
        ))
    }
}
