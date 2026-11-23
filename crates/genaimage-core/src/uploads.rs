//! Content-addressed reference images stored independently of run records.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};

const IMAGE_FILE: &str = "image";
const METADATA_FILE: &str = "metadata.json";

#[derive(Debug, Deserialize, Serialize)]
pub struct UploadMetadata {
    pub upload_id: String,
    pub sha256: String,
    pub byte_size: u64,
    pub media_type: String,
    pub original_filename: String,
}

impl UploadMetadata {
    pub(crate) fn extension(&self) -> &str {
        match self.media_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => unreachable!("stored media type has been validated"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("The upload exceeds the configured size limit.")]
    TooLarge,
    #[error("The file is not a recognised PNG, JPEG or WebP image.")]
    UnsupportedType,
    #[error("Upload storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Upload metadata error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("The stored upload does not match its recorded metadata or SHA-256.")]
    Integrity,
    #[error("The upload store entry is a symbolic link or is not the expected file type.")]
    UnsafeEntry,
}

#[derive(Clone)]
pub struct UploadStore {
    root: PathBuf,
    limit: usize,
}

impl UploadStore {
    pub(crate) fn new(root: &Path, limit: usize) -> Self {
        Self {
            root: root.to_owned(),
            limit,
        }
    }

    pub async fn stage(&self, filename: &str) -> Result<StagedUpload, UploadError> {
        fs::create_dir_all(&self.root).await?;
        let root = self.root.clone();
        let directory = tokio::task::spawn_blocking(move || {
            tempfile::Builder::new()
                .prefix(".incoming-")
                .tempdir_in(root)
        })
        .await
        .map_err(std::io::Error::other)??;
        let file = fs::File::create(directory.path().join(IMAGE_FILE)).await?;
        Ok(StagedUpload {
            file,
            directory,
            store: self.clone(),
            filename: sanitise(filename),
            size: 0,
            hash: Sha256::new(),
            header: Vec::with_capacity(12),
        })
    }

    pub async fn metadata(&self, id: &str) -> Result<Option<UploadMetadata>, UploadError> {
        let Some(hash) = id.strip_prefix("sha256:").filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) else {
            return Ok(None);
        };
        let directory = match DigestDirectory::open(&self.root.join(hash)).await {
            Ok(directory) => directory,
            Err(UploadError::Io(error)) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        directory.metadata(id, hash).await
    }

    /// Return the exact bytes that were verified, not a path to reopen later.
    pub(crate) async fn verified_bytes(
        &self,
        metadata: &UploadMetadata,
    ) -> Result<Vec<u8>, UploadError> {
        let directory = DigestDirectory::open(&self.root.join(&metadata.sha256)).await?;
        let file = directory.open_image(metadata.byte_size).await?;
        let mut bytes = Vec::new();
        file.take(metadata.byte_size.saturating_add(1))
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 != metadata.byte_size
            || hex::encode(Sha256::digest(&bytes)) != metadata.sha256
            || detected_type(&bytes) != Some(metadata.media_type.as_str())
        {
            return Err(UploadError::Integrity);
        }
        Ok(bytes)
    }
}

struct DigestDirectory {
    #[cfg(unix)]
    fd: rustix::fd::OwnedFd,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl DigestDirectory {
    async fn open(path: &Path) -> Result<Self, UploadError> {
        check_directory(path).await?;
        #[cfg(unix)]
        {
            use rustix::fs::{open, Mode, OFlags};
            let fd = open(
                path,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(no_follow_error)?;
            Ok(Self { fd })
        }
        #[cfg(not(unix))]
        Ok(Self {
            path: path.to_owned(),
        })
    }

    async fn check_file(&self, name: &str) -> Result<(), UploadError> {
        #[cfg(unix)]
        let regular = {
            use rustix::fs::{statat, AtFlags, FileType};
            let stat =
                statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)?;
            FileType::from_raw_mode(stat.st_mode).is_file()
        };
        #[cfg(not(unix))]
        let regular = {
            check_directory(&self.path).await?;
            fs::symlink_metadata(self.path.join(name)).await?.is_file()
        };
        if !regular {
            return Err(UploadError::UnsafeEntry);
        }
        Ok(())
    }

    async fn open_file(&self, name: &str) -> Result<fs::File, UploadError> {
        self.check_file(name).await?;
        #[cfg(unix)]
        let file = {
            use rustix::fs::{openat, Mode, OFlags};
            let fd = openat(
                &self.fd,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(no_follow_error)?;
            fs::File::from_std(fd.into())
        };
        #[cfg(not(unix))]
        let file = fs::File::open(self.path.join(name)).await?;
        if !file.metadata().await?.is_file() {
            return Err(UploadError::UnsafeEntry);
        }
        Ok(file)
    }

    async fn metadata(&self, id: &str, hash: &str) -> Result<Option<UploadMetadata>, UploadError> {
        match self.check_file(IMAGE_FILE).await {
            Ok(()) => {}
            Err(UploadError::Io(error)) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut file = match self.open_file(METADATA_FILE).await {
            Ok(file) => file,
            Err(UploadError::Io(error)) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await?;
        let metadata: UploadMetadata = serde_json::from_slice(&bytes)?;
        if metadata.upload_id != id
            || metadata.sha256 != hash
            || !matches!(
                metadata.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
        {
            return Err(UploadError::Integrity);
        }
        Ok(Some(metadata))
    }

    async fn open_image(&self, size: u64) -> Result<fs::File, UploadError> {
        let file = self.open_file(IMAGE_FILE).await?;
        if file.metadata().await?.len() != size {
            return Err(UploadError::Integrity);
        }
        Ok(file)
    }

    async fn replace_image(&self, staged: &Path) -> Result<(), UploadError> {
        match self.check_file(IMAGE_FILE).await {
            Ok(()) => {}
            Err(UploadError::Io(error)) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        {
            // renameat does not follow the destination entry, even if it is swapped.
            let source = Self::open(staged).await?;
            rustix::fs::renameat(&source.fd, IMAGE_FILE, &self.fd, IMAGE_FILE)
                .map_err(std::io::Error::from)?;
        }
        #[cfg(not(unix))]
        {
            check_directory(&self.path).await?;
            fs::rename(staged.join(IMAGE_FILE), self.path.join(IMAGE_FILE)).await?;
        }
        Ok(())
    }
}

async fn check_directory(path: &Path) -> Result<(), UploadError> {
    if !fs::symlink_metadata(path).await?.is_dir() {
        return Err(UploadError::UnsafeEntry);
    }
    Ok(())
}

#[cfg(unix)]
fn no_follow_error(error: rustix::io::Errno) -> UploadError {
    match error {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => UploadError::UnsafeEntry,
        _ => std::io::Error::from(error).into(),
    }
}

async fn verify_image(
    directory: &DigestDirectory,
    metadata: &UploadMetadata,
) -> Result<(), UploadError> {
    let file = directory.open_image(metadata.byte_size).await?;
    let mut reader = file.take(metadata.byte_size.saturating_add(1));
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        size += count as u64;
        hash.update(&buffer[..count]);
    }
    if size != metadata.byte_size || hex::encode(hash.finalize()) != metadata.sha256 {
        return Err(UploadError::Integrity);
    }
    Ok(())
}

pub struct StagedUpload {
    file: fs::File,
    directory: tempfile::TempDir,
    store: UploadStore,
    filename: String,
    size: usize,
    hash: Sha256,
    header: Vec<u8>,
}

impl StagedUpload {
    pub async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), UploadError> {
        self.size = self
            .size
            .checked_add(bytes.len())
            .filter(|size| *size <= self.store.limit)
            .ok_or(UploadError::TooLarge)?;
        let take = bytes.len().min(12 - self.header.len());
        self.header.extend_from_slice(&bytes[..take]);
        if self.header.len() == 12 && detected_type(&self.header).is_none() {
            return Err(UploadError::UnsupportedType);
        }
        self.hash.update(bytes);
        self.file.write_all(bytes).await?;
        Ok(())
    }

    /// Publish metadata and the image together; concurrent duplicate uploads keep
    /// the first published entry (and its filename).
    pub async fn publish(self) -> Result<UploadMetadata, UploadError> {
        let Self {
            mut file,
            directory,
            store,
            filename,
            size,
            hash,
            header,
        } = self;
        let media_type = detected_type(&header).ok_or(UploadError::UnsupportedType)?;
        let sha256 = hex::encode(hash.finalize());
        let metadata = UploadMetadata {
            upload_id: format!("sha256:{sha256}"),
            sha256,
            byte_size: size as u64,
            media_type: media_type.into(),
            original_filename: filename,
        };
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        let mut metadata_file = fs::File::create(directory.path().join(METADATA_FILE)).await?;
        metadata_file
            .write_all(&serde_json::to_vec(&metadata)?)
            .await?;
        metadata_file.flush().await?;
        metadata_file.sync_all().await?;
        drop(metadata_file);
        let destination = store.root.join(&metadata.sha256);
        // Both candidate directories are non-empty, so rename cannot replace a winner.
        let rename_error = match fs::rename(directory.path(), &destination).await {
            Ok(()) => return Ok(metadata),
            Err(error) => error,
        };
        let destination = DigestDirectory::open(&destination).await?;
        let existing = destination
            .metadata(&metadata.upload_id, &metadata.sha256)
            .await?
            .ok_or(rename_error)?;
        if existing.byte_size != metadata.byte_size || existing.media_type != metadata.media_type {
            return Err(UploadError::Integrity);
        }
        match verify_image(&destination, &existing).await {
            Ok(()) => {}
            Err(error) => {
                if !matches!(error, UploadError::Integrity)
                    && !matches!(&error, UploadError::Io(error) if error.kind() == ErrorKind::NotFound)
                {
                    return Err(error);
                }
                // Every repair has the same digest. Keep first-upload metadata
                // and replace only the image, without exposing a partial file.
                destination.replace_image(directory.path()).await?;
            }
        }
        Ok(existing)
    }
}

fn detected_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn sanitise(filename: &str) -> String {
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or_default();
    let safe: String = basename
        .chars()
        .take(128)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim_matches('.');
    if safe.is_empty() {
        "image".into()
    } else {
        safe.into()
    }
}
