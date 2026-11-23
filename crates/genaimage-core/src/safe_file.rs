use std::{
    fs::File,
    path::{Component, Path},
};

/// Open a regular file below a configured root without following child symlinks on Unix.
#[doc(hidden)]
pub fn open_regular_beneath(root: &Path, directory: &Path, relative: &Path) -> Option<File> {
    let relative_dir = directory.strip_prefix(root).ok()?;
    let safe = |path: &Path| {
        path.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    };
    if !safe(relative_dir) || !safe(relative) {
        return None;
    }
    #[cfg(unix)]
    let file = open_beneath(
        open_directory(&root.canonicalize().ok()?)?,
        &relative_dir.join(relative),
    )?;
    #[cfg(not(unix))]
    let file = open_fallback(root, directory, relative)?;
    file.metadata().ok()?.is_file().then_some(file)
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
fn open_beneath(mut directory: std::fs::File, relative: &Path) -> Option<std::fs::File> {
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
        if components.peek().is_some() {
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
    use super::{open_beneath, open_directory};
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
        let mut image = open_beneath(directory, Path::new("image.png")).unwrap();
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
        assert!(open_beneath(directory, Path::new("image.png")).is_none());

        let subdirectory = root.join("subdirectory");
        std::fs::create_dir(&subdirectory).unwrap();
        let directory = open_directory(&root).unwrap();
        std::fs::remove_dir(&subdirectory).unwrap();
        std::os::unix::fs::symlink(&outside, &subdirectory).unwrap();
        assert!(open_beneath(directory, Path::new("subdirectory/image.png")).is_none());
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
