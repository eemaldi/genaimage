use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

use genaimage_api::{
    client::{Client, OutputRequest},
    OutputListQuery,
};
use serde::Serialize;
use serde_json::Value;
use tempfile::NamedTempFile;
use tokio::{fs, io::AsyncWriteExt};

use crate::output::{Error, Output};

#[derive(Serialize)]
struct Download {
    id: String,
    path: PathBuf,
}

#[derive(Serialize)]
struct Downloads {
    run_id: String,
    outputs: Vec<Download>,
}

pub(crate) async fn download(
    client: &Client,
    output: Output,
    id: &str,
    dir: &Path,
) -> Result<(), Error> {
    client.run(id).await?;
    fs::create_dir_all(dir)
        .await
        .map_err(|error| file_error(dir, error))?;
    let mut files = Downloads {
        run_id: id.to_owned(),
        outputs: Vec::new(),
    };
    let mut query = OutputListQuery {
        run: Some(id.to_owned()),
        ..Default::default()
    };
    loop {
        let page = client.outputs(&query).await?;
        for item in page.outputs {
            let extension = Path::new(&item.source_filename)
                .extension()
                .and_then(|ext| ext.to_str())
                .filter(|ext| {
                    !ext.is_empty()
                        && ext.len() <= 10
                        && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                .unwrap_or("bin");
            let path = dir.join(format!("output-{:03}.{extension}", files.outputs.len() + 1));
            let mut response = client.output(&item.id, &OutputRequest::default()).await?;
            let temp = NamedTempFile::new_in(dir).map_err(|error| file_error(&path, error))?;
            let mut file =
                fs::File::from_std(temp.reopen().map_err(|error| file_error(&path, error))?);
            while let Some(bytes) = response
                .chunk()
                .await
                .map_err(genaimage_api::client::Error::from)?
            {
                file.write_all(&bytes)
                    .await
                    .map_err(|error| file_error(&path, error))?;
            }
            file.flush()
                .await
                .map_err(|error| file_error(&path, error))?;
            drop(file);
            publish(temp, &path)?;
            files.outputs.push(Download { id: item.id, path });
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        query.cursor = Some(cursor);
    }
    output.write(&files, |out| {
        if files.outputs.is_empty() {
            writeln!(out, "No completed outputs for '{id}'.")?;
        }
        for file in &files.outputs {
            writeln!(out, "{}", file.path.display())?;
        }
        Ok(())
    })
}

pub(crate) async fn manifest(
    client: &Client,
    output: Output,
    id: &str,
    destination: Option<&Path>,
) -> Result<(), Error> {
    let mut value: Value = client
        .run_manifest(id, None)
        .await?
        .json()
        .await
        .map_err(genaimage_api::client::Error::from)?;
    // The archived format includes filesystem locations; retain provenance, not those locations.
    if let Some(workflow) = value.get_mut("workflow").and_then(Value::as_object_mut) {
        workflow.remove("manifest_path");
    }
    for (field, path_key) in [("outputs", "local_path"), ("source_inputs", "source_path")] {
        if let Some(items) = value.get_mut(field).and_then(Value::as_array_mut) {
            for item in items.iter_mut().filter_map(Value::as_object_mut) {
                item.remove(path_key);
            }
        }
    }
    if let Some(path) = destination {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temp = NamedTempFile::new_in(parent).map_err(|error| file_error(path, error))?;
        let rendered = serde_json::to_string_pretty(&value).map_err(Error::Output)?;
        writeln!(temp, "{rendered}").map_err(|error| file_error(path, error))?;
        publish(temp, path)?;
        output.write(&serde_json::json!({"run_id": id, "path": path}), |out| {
            writeln!(out, "Saved manifest to '{}'.", path.display())
        })
    } else {
        output.write(&value, |out| {
            serde_json::to_writer_pretty(&mut *out, &value).map_err(io::Error::other)?;
            writeln!(out)
        })
    }
}

fn publish(file: NamedTempFile, path: &Path) -> Result<(), Error> {
    file.persist_noclobber(path)
        .map_err(|error| file_error(path, error.error))?;
    Ok(())
}

fn file_error(path: &Path, error: io::Error) -> Error {
    let detail = if error.kind() == io::ErrorKind::AlreadyExists {
        "already exists; choose a different destination".to_owned()
    } else {
        error.to_string()
    };
    Error::File(format!("Cannot save '{}': {detail}.", path.display()))
}
