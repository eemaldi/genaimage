use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use chrono::{DateTime, Utc};
use image::{ImageDecoder, ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};

use crate::{
    artifacts::{RunManifest, RUN_MANIFEST_VERSION},
    persistence::OutputRecord,
    safe_file::open_regular_beneath,
    Harness, HarnessError, Result, RunStatus,
};

const CACHE_VERSION: &str = "webp-v1";
const MAX_EDGE: u32 = 8192;
const MAX_PIXELS: u64 = 16_000_000;
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_THUMBNAIL_BYTES: u64 = 8 * 1024 * 1024;
const LOCK_STRIPES: usize = 32;
static CACHE_LOCKS: LazyLock<[Mutex<()>; LOCK_STRIPES]> =
    LazyLock::new(|| std::array::from_fn(|_| Mutex::new(())));
static DECODERS: Semaphore = Semaphore::const_new(2);

#[derive(Default)]
pub struct OutputListQuery {
    pub run: Option<String>,
    pub workflow: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub tag: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

pub struct IndexedOutput {
    pub id: String,
    pub workflow_id: String,
    pub tags: Vec<String>,
    pub output: OutputRecord,
}

pub struct OutputPage {
    pub outputs: Vec<IndexedOutput>,
    pub next_cursor: Option<String>,
}

pub struct MediaFile {
    pub file: File,
    pub sha256: String,
    pub len: u64,
}

pub struct StoredRunManifest {
    pub media: MediaFile,
    pub integrity_verified: bool,
}

#[derive(Clone, Copy)]
pub struct ThumbnailSize(u32);

impl ThumbnailSize {
    pub fn new(size: u32) -> Result<Self> {
        if !matches!(size, 128 | 256 | 512 | 1024) {
            return Err(HarnessError::InvalidMediaRequest(
                "Thumbnail size must be 128, 256, 512 or 1024.",
            ));
        }
        Ok(Self(size))
    }
}

impl Harness {
    pub async fn open_output(&self, id: &str) -> Result<(IndexedOutput, MediaFile)> {
        let output = self.repository().as_async().output(id).await?;
        let root = self.config().runs.clone();
        let data = self.config().data_dir.clone();
        blocking(move || {
            let file = verified_output(&output.output, &root, &data)?;
            Ok((output, file))
        })
        .await
    }

    pub async fn open_run_manifest(&self, id: &str) -> Result<StoredRunManifest> {
        let repository = self.repository().as_async();
        let run = repository.get(id).await?;
        if run.status != RunStatus::Completed {
            return Err(HarnessError::ManifestNotFound);
        }
        let path = PathBuf::from(run.manifest_path.ok_or(HarnessError::ManifestNotFound)?);
        let expected = repository.manifest_checksum(run.id.clone()).await?;
        let root = self.config().runs.clone();
        let data = self.config().data_dir.clone();
        blocking(move || {
            let file = open_stored(&root, &path)?;
            let mut manifest = snapshot(file, &data, None)?;
            let integrity_verified = expected.is_some();
            if let Some(expected) = expected {
                if expected != manifest.sha256 {
                    return Err(HarnessError::MediaIntegrity);
                }
            } else {
                let stored: RunManifest =
                    serde_json::from_reader(BufReader::new(&mut manifest.file))
                        .map_err(|_| HarnessError::MediaIntegrity)?;
                if stored.schema_version != RUN_MANIFEST_VERSION
                    || stored.run_id != run.id
                    || stored.prompt_id != run.prompt_id
                    || stored.workflow.id != run.workflow_id
                    || stored.workflow.version != run.workflow_version
                    || stored.workflow.sha256 != run.workflow_sha256
                    || !is_sha256(&stored.workflow.manifest_sha256)
                    || !stored
                        .outputs
                        .iter()
                        .all(|output| output.run_id == run.id && is_sha256(&output.sha256))
                    || !stored
                        .source_inputs
                        .iter()
                        .all(|input| is_sha256(&input.sha256))
                {
                    return Err(HarnessError::MediaIntegrity);
                }
                manifest.file.rewind()?;
            }
            Ok(StoredRunManifest {
                media: manifest,
                integrity_verified,
            })
        })
        .await
    }

    pub async fn open_thumbnail(&self, id: &str, size: ThumbnailSize) -> Result<MediaFile> {
        let output = self.repository().as_async().output(id).await?.output;
        let root = self.config().runs.clone();
        let data = self.config().data_dir.clone();
        let cache = ThumbnailCache::new(&data, &output.sha256, size)?;
        cache
            .get_or_generate(move |target| {
                if output.byte_size > MAX_SOURCE_BYTES {
                    return Err(thumbnail_limit());
                }
                encode_thumbnail(verified_output(&output, &root, &data)?.file, size, target)
            })
            .await
    }
}

pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(std::io::Error::other)?
}

pub(crate) fn open_stored(root: &Path, path: &Path) -> Result<File> {
    path.strip_prefix(root)
        .ok()
        .and_then(|path| open_regular_beneath(root, root, path))
        .ok_or(HarnessError::MediaIntegrity)
}

fn verified_output(output: &OutputRecord, root: &Path, data: &Path) -> Result<MediaFile> {
    let source = open_stored(root, Path::new(&output.local_path))?;
    if source.metadata()?.len() != output.byte_size {
        return Err(HarnessError::MediaIntegrity);
    }
    let snapshot = snapshot(source, data, Some(output.byte_size))?;
    if snapshot.sha256 != output.sha256 || snapshot.len != output.byte_size {
        return Err(HarnessError::MediaIntegrity);
    }
    Ok(snapshot)
}

// Snapshot the verified bytes into a private, unnamed file. An open source
// descriptor alone would not protect streaming against later in-place edits.
pub(crate) fn snapshot(mut source: File, data: &Path, limit: Option<u64>) -> Result<MediaFile> {
    fs::create_dir_all(data)?;
    let mut file = tempfile::tempfile_in(data)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut len = 0;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        len += count as u64;
        if limit.is_some_and(|limit| len > limit) {
            return Err(HarnessError::MediaIntegrity);
        }
        hash.update(&buffer[..count]);
        file.write_all(&buffer[..count])?;
    }
    file.rewind()?;
    Ok(MediaFile {
        file,
        sha256: hex::encode(hash.finalize()),
        len,
    })
}

fn is_sha256(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

struct ThumbnailCache {
    root: PathBuf,
    name: String,
    stripe: usize,
}

impl ThumbnailCache {
    fn new(data: &Path, checksum: &str, size: ThumbnailSize) -> Result<Self> {
        let mut digest = [0; 32];
        hex::decode_to_slice(checksum, &mut digest).map_err(|_| HarnessError::MediaIntegrity)?;
        Ok(Self {
            root: data.join("thumbnails").join(CACHE_VERSION),
            name: format!("{}-{}.webp", hex::encode(digest), size.0),
            stripe: (usize::from(digest[0]) + (size.0 / 128) as usize) % LOCK_STRIPES,
        })
    }

    async fn get_or_generate(
        self,
        generate: impl FnOnce(&mut File) -> Result<()> + Send + 'static,
    ) -> Result<MediaFile> {
        // Move both guards into the blocking job so cancelling a request cannot
        // let a second encoder race the first.
        let stripe = CACHE_LOCKS[self.stripe].lock().await;
        let permit = DECODERS
            .acquire()
            .await
            .expect("decoder semaphore is never closed");
        blocking(move || {
            let (_stripe, _permit) = (stripe, permit);
            self.get_or_generate_blocking(generate)
        })
        .await
    }

    fn verified_hit(&self, checksum_name: &Path) -> Result<Option<MediaFile>> {
        let open = |name| open_regular_beneath(&self.root, &self.root, name);
        let Some(checksum) = open(checksum_name) else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        checksum.take(65).read_to_end(&mut bytes)?;
        let Ok(expected) = std::str::from_utf8(&bytes) else {
            return Ok(None);
        };
        if !is_sha256(expected) {
            return Ok(None);
        }
        let Some(file) = open(Path::new(&self.name)) else {
            return Ok(None);
        };
        match snapshot(file, &self.root, Some(MAX_THUMBNAIL_BYTES)) {
            Ok(media) if media.sha256 == expected => Ok(Some(media)),
            Ok(_) | Err(HarnessError::MediaIntegrity) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn get_or_generate_blocking(
        &self,
        generate: impl FnOnce(&mut File) -> Result<()>,
    ) -> Result<MediaFile> {
        let checksum_name = PathBuf::from(format!("{}.sha256", self.name));
        if let Some(media) = self.verified_hit(&checksum_name)? {
            return Ok(media);
        }
        fs::create_dir_all(&self.root)?;
        // Also coordinate with other processes sharing the workspace data directory.
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(format!("lock-{}", self.stripe)))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        if let Some(media) = self.verified_hit(&checksum_name)? {
            return Ok(media);
        }
        let mut thumbnail = tempfile::NamedTempFile::new_in(&self.root)?;
        generate(thumbnail.as_file_mut())?;
        let mut encoded = thumbnail.reopen()?;
        encoded.rewind()?;
        let media = snapshot(encoded, &self.root, Some(MAX_THUMBNAIL_BYTES))?;
        let mut checksum = tempfile::NamedTempFile::new_in(&self.root)?;
        checksum.write_all(media.sha256.as_bytes())?;
        // Any partial publication is a cache miss: readers only accept a matching pair.
        thumbnail
            .persist(self.root.join(&self.name))
            .map_err(|error| error.error)?;
        checksum
            .persist(self.root.join(checksum_name))
            .map_err(|error| error.error)?;
        Ok(media)
    }
}

fn thumbnail_limit() -> HarnessError {
    HarnessError::ThumbnailDecode(image::ImageError::Limits(
        image::error::LimitError::from_kind(image::error::LimitErrorKind::InsufficientMemory),
    ))
}

fn encode_thumbnail(source: File, size: ThumbnailSize, target: &mut File) -> Result<()> {
    let mut reader = ImageReader::new(BufReader::new(source)).with_guessed_format()?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        return Err(HarnessError::InvalidMediaRequest(
            "Only PNG, JPEG and WebP outputs support thumbnails.",
        ));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits.clone());
    let check = |error| HarnessError::ThumbnailDecode(error);
    let mut decoder = reader.into_decoder().map_err(check)?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(thumbnail_limit());
    }
    decoder.set_limits(limits).map_err(check)?;
    let image = image::DynamicImage::ImageRgba8(
        image::DynamicImage::from_decoder(decoder)
            .map_err(check)?
            .into_rgba8(),
    );
    let thumbnail = if width.max(height) > size.0 {
        image.thumbnail(size.0, size.0)
    } else {
        image
    };
    thumbnail
        .write_to(target, ImageFormat::WebP)
        .map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;

    #[tokio::test]
    async fn concurrent_cache_misses_encode_once_per_size_and_reopen_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let checksum = "a".repeat(64);
        let calls = Arc::new(AtomicUsize::new(0));
        for size in [128, 256] {
            let size = ThumbnailSize::new(size).unwrap();
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..8 {
                let calls = Arc::clone(&calls);
                let cache = ThumbnailCache::new(dir.path(), &checksum, size).unwrap();
                tasks.spawn(cache.get_or_generate(move |file| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    file.write_all(b"encoded thumbnail")?;
                    Ok(())
                }));
            }
            while let Some(file) = tasks.join_next().await {
                assert_eq!(file.unwrap().unwrap().len, 17);
            }
            ThumbnailCache::new(dir.path(), &checksum, size)
                .unwrap()
                .get_or_generate(|_| panic!("disk cache did not survive reopening"))
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn thumbnail_rejects_an_oversized_source_dimension() {
        let mut source = tempfile::tempfile().unwrap();
        image::DynamicImage::new_rgba8(MAX_EDGE + 1, 1)
            .write_to(&mut source, ImageFormat::Png)
            .unwrap();
        source.rewind().unwrap();
        let mut target = tempfile::tempfile().unwrap();
        assert!(matches!(
            encode_thumbnail(source, ThumbnailSize::new(128).unwrap(), &mut target),
            Err(HarnessError::ThumbnailDecode(image::ImageError::Limits(_)))
        ));
        assert_eq!(target.metadata().unwrap().len(), 0);
    }

    #[test]
    fn original_snapshot_is_unchanged_after_in_place_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output");
        fs::write(&path, b"original").unwrap();
        let mut verified = snapshot(File::open(&path).unwrap(), dir.path(), Some(8)).unwrap();
        fs::write(&path, b"modified").unwrap();
        let mut bytes = Vec::new();
        verified.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
    }
}
