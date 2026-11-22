use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use md5_digest::{Digest, Md5};
use sources::{ImageSize, SourceId};

use crate::selection::Candidate;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const CACHE_LAYOUT: &str = "v1";

#[derive(Clone, Debug)]
pub struct ArtworkCacheEntry {
    pub key: String,
    pub path: PathBuf,
}

impl ArtworkCacheEntry {
    pub(crate) fn family(&self) -> &str {
        self.key
            .split_once('/')
            .unwrap()
            .1
            .rsplit_once('/')
            .unwrap()
            .0
    }
}

/// Walks the fixed cache layout without collecting the artwork library in memory.
pub struct ArtworkCacheEntries {
    root: PathBuf,
    directories: Vec<fs::ReadDir>,
}

impl Iterator for ArtworkCacheEntries {
    type Item = io::Result<ArtworkCacheEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let directory = self.directories.last_mut()?;
            let entry = match directory.next() {
                Some(Ok(entry)) => entry,
                Some(Err(error)) => return Some(Err(error)),
                None => {
                    self.directories.pop();
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => return Some(Err(error)),
            };
            let path = entry.path();
            if file_type.is_dir() {
                let image_directory = path
                    .strip_prefix(&self.root)
                    .ok()
                    .and_then(|path| path.components().next())
                    .is_some_and(|part| {
                        part.as_os_str() == "originals" || part.as_os_str() == "displays"
                    });
                if image_directory && self.directories.len() < 5 {
                    match fs::read_dir(path) {
                        Ok(directory) => self.directories.push(directory),
                        Err(error) => return Some(Err(error)),
                    }
                }
            } else if file_type.is_file()
                && let Some(key) = cache_key(&self.root, &path)
            {
                match entry.metadata() {
                    Ok(metadata) if metadata.len() > 0 => {
                        return Some(Ok(ArtworkCacheEntry { key, path }));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Some(Err(error)),
                }
            }
        }
    }
}

pub(crate) fn current_layout(root: &Path) -> io::Result<PathBuf> {
    let current = root.join(CACHE_LAYOUT);
    fs::create_dir_all(&current)?;
    Ok(current)
}

#[derive(Clone, Debug)]
pub(crate) struct FilesystemCache {
    root: PathBuf,
}

impl FilesystemCache {
    /// Opens the cache, which keeps what it holds until the person clears it.
    pub(crate) fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        remove_dir_if_present(&root.join("ready"))?;
        Ok(Self { root })
    }

    pub(crate) fn entries(&self) -> io::Result<ArtworkCacheEntries> {
        Ok(ArtworkCacheEntries {
            root: self.root.clone(),
            directories: vec![fs::read_dir(&self.root)?],
        })
    }

    pub(crate) fn file(&self, key: &str) -> io::Result<Option<PathBuf>> {
        let path = self.path_for_key(key)?;
        match fs::metadata(&path) {
            Ok(metadata) => Ok((metadata.is_file() && metadata.len() > 0).then_some(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn path_for_key(&self, key: &str) -> io::Result<PathBuf> {
        if valid_cache_key(key) {
            Ok(key
                .split('/')
                .fold(self.root.clone(), |path, part| path.join(part)))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid artwork cache key",
            ))
        }
    }

    pub(crate) fn remove_entries(&self, entries: &[ArtworkCacheEntry]) -> io::Result<()> {
        entries
            .iter()
            .try_for_each(|entry| remove_file_if_present(&entry.path))
    }

    pub(crate) fn clear_missing(&self) -> io::Result<()> {
        remove_dir_if_present(&self.root.join("missing"))
    }

    pub(crate) fn ready_entry(&self, candidate: &Candidate) -> Option<CacheEntry> {
        self.candidate_directory("originals", candidate)
            .read_dir()
            .ok()?
            .filter_map(Result::ok)
            .find_map(|entry| {
                let path = entry.path();
                cache_key(&self.root, &path)?;
                let metadata = entry.metadata().ok()?;
                if metadata.is_file() && metadata.len() > 0 {
                    Some(CacheEntry { path })
                } else {
                    let _ = fs::remove_file(&path);
                    None
                }
            })
    }

    pub(crate) fn write_ready(
        &self,
        candidate: &Candidate,
        size: ImageSize,
        bytes: &[u8],
    ) -> io::Result<PathBuf> {
        let path = match size {
            ImageSize::Original => {
                let extension = crate::decode::original_extension(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                self.candidate_directory("originals", candidate)
                    .join(format!("original.{extension}"))
            }
            ImageSize::Thumbnail(size) => self
                .candidate_directory("displays", candidate)
                .join(format!("{size}.image")),
        };
        atomic_write(&path, bytes)?;
        let _ = fs::remove_file(self.missing_path(candidate));
        Ok(path)
    }

    pub(crate) fn display_path(&self, candidate: &Candidate, size: u32) -> Option<PathBuf> {
        self.display_file(&candidate.key, size)
            .ok()?
            .map(|entry| entry.path)
    }

    fn display_file(&self, family: &str, size: u32) -> io::Result<Option<ArtworkCacheEntry>> {
        let key = format!("displays/{family}/{size}.image");
        if let Some(path) = self.file(&key)? {
            return Ok(Some(ArtworkCacheEntry { key, path }));
        }
        let directory = match fs::read_dir(self.root.join("displays").join(family)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(directory
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let available = entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".image")?
                    .parse::<u32>()
                    .ok()?;
                if available < size || file_len(&path) == 0 {
                    return None;
                }
                let key = cache_key(&self.root, &path)?;
                Some((available, ArtworkCacheEntry { key, path }))
            })
            .min_by_key(|(size, _)| *size)
            .map(|(_, entry)| entry))
    }

    pub(crate) fn best_display(&self, candidate: &Candidate) -> Option<PathBuf> {
        self.candidate_directory("displays", candidate)
            .read_dir()
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let size = entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".image")?
                    .parse::<u32>()
                    .ok()?;
                let path = entry.path();
                (size > 0 && file_len(&path) > 0).then_some((size, path))
            })
            .max_by_key(|(size, _)| *size)
            .map(|(_, path)| path)
    }

    pub(crate) fn remove_ready(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }

    pub(crate) fn is_missing(&self, candidate: &Candidate) -> bool {
        self.missing_path(candidate).is_file()
    }

    pub(crate) fn mark_missing(&self, candidate: &Candidate) -> io::Result<()> {
        atomic_write(&self.missing_path(candidate), &b"missing\n"[..])
    }

    pub(crate) fn retry_external(&self) -> io::Result<()> {
        remove_dir_if_present(&self.root.join("missing/external"))
    }

    /// Forgets which of a source's pictures its server said it does not have.
    pub(crate) fn retry_source(&self, source_id: &SourceId) -> io::Result<()> {
        remove_dir_if_present(
            &self
                .root
                .join("missing/native")
                .join(digest(source_id.as_str())),
        )
    }

    pub(crate) fn invalidate_source(&self, source_id: &SourceId) -> io::Result<()> {
        let source = digest(source_id.as_str());
        remove_dir_if_present(&self.root.join("missing/native").join(&source))?;
        remove_dir_if_present(&self.root.join("originals/native").join(&source))?;
        remove_dir_if_present(&self.root.join("displays/native").join(source))
    }

    pub(crate) fn invalidate_image(&self, candidate: &Candidate) -> io::Result<()> {
        ["missing", "originals", "displays"]
            .into_iter()
            .try_for_each(|state| {
                remove_dir_if_present(&self.candidate_directory(state, candidate))
            })
    }

    fn missing_path(&self, candidate: &Candidate) -> PathBuf {
        self.candidate_directory("missing", candidate)
            .join("original.missing")
    }

    fn candidate_directory(&self, state: &str, candidate: &Candidate) -> PathBuf {
        self.root
            .join(state)
            .join(Self::candidate_family(candidate))
    }

    pub(crate) fn candidate_family(candidate: &Candidate) -> String {
        candidate.key.clone()
    }
}

/// The size of the file at `path`; nothing there is no size.
fn file_len(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .filter(fs::Metadata::is_file)
        .map_or(0, |metadata| metadata.len())
}

pub(crate) fn valid_cache_key(key: &str) -> bool {
    let parts = key.split('/').collect::<Vec<_>>();
    let (state, identity, name) = match parts.as_slice() {
        [state, "native", source, identity, name] if is_digest(source) => {
            (*state, *identity, *name)
        }
        [state, "external" | "playlists", identity, name] => (*state, *identity, *name),
        _ => return false,
    };
    is_digest(identity)
        && match state {
            "displays" => name
                .strip_suffix(".image")
                .is_some_and(|size| size.parse::<u32>().is_ok_and(|size| size > 0)),
            "originals" => name.strip_prefix("original.").is_some_and(|extension| {
                matches!(
                    extension,
                    "jpg" | "png" | "gif" | "webp" | "tiff" | "bmp" | "jxl"
                )
            }),
            _ => false,
        }
}

fn is_digest(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn cache_key(root: &Path, path: &Path) -> Option<String> {
    let key = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    valid_cache_key(&key).then_some(key)
}

#[derive(Clone, Debug)]
pub(crate) struct CacheEntry {
    pub(crate) path: PathBuf,
}

fn digest(value: &str) -> String {
    format!(
        "{:032x}",
        u128::from_be_bytes(Md5::digest(value.as_bytes()).into())
    )
}

fn atomic_write(path: &Path, mut bytes: impl Read) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artwork cache path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".artwork-{}-{}.tmp",
        std::process::id(),
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    io::copy(&mut bytes, &mut fs::File::create(&temporary)?)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) if path.is_file() => {
            let _ = fs::remove_file(&temporary);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library::ArtworkImage;

    fn native() -> Candidate {
        Candidate::new(
            ArtworkImage::Native {
                source_id: SourceId::new("source"),
                item_id: "album".into(),
                tag: Some("tag".into()),
            },
            None,
        )
    }

    #[test]
    fn cached_original_is_returned_for_its_binding() {
        let directory = tempfile::tempdir().expect("temporary artwork cache");
        let cache = FilesystemCache::new(directory.path().to_path_buf()).expect("open cache");
        let bytes = b"\x89PNG\r\n\x1a\n";
        cache.mark_missing(&native()).unwrap();
        let path = cache
            .write_ready(&native(), ImageSize::Original, bytes)
            .expect("cache original");
        assert_eq!(cache.ready_entry(&native()).unwrap().path, path);
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(!cache.is_missing(&native()));
        let display = cache
            .write_ready(&native(), ImageSize::Thumbnail(540), bytes)
            .unwrap();
        let smaller = cache
            .write_ready(&native(), ImageSize::Thumbnail(256), bytes)
            .unwrap();
        assert_eq!(cache.display_path(&native(), 540), Some(display.clone()));
        assert_eq!(cache.display_path(&native(), 512), Some(display.clone()));
        assert_eq!(cache.display_path(&native(), 96), Some(smaller.clone()));
        assert!(cache.display_path(&native(), 768).is_none());
    }

    #[test]
    fn source_invalidation_removes_only_that_native_binding_family() {
        let directory = tempfile::tempdir().expect("temporary artwork cache");
        let cache = FilesystemCache::new(directory.path().to_path_buf()).expect("open cache");
        let source = SourceId::new("source");
        let other = Candidate::new(
            ArtworkImage::Native {
                source_id: SourceId::new("other"),
                item_id: "album".into(),
                tag: Some("tag".into()),
            },
            None,
        );
        cache
            .write_ready(&native(), ImageSize::Original, b"\x89PNG\r\n\x1a\n")
            .expect("cache source cover");
        cache
            .write_ready(&other, ImageSize::Original, b"\x89PNG\r\n\x1a\n")
            .expect("cache other cover");
        cache
            .write_ready(&native(), ImageSize::Thumbnail(512), b"\x89PNG\r\n\x1a\n")
            .unwrap();
        cache
            .write_ready(&other, ImageSize::Thumbnail(512), b"\x89PNG\r\n\x1a\n")
            .unwrap();

        cache.invalidate_source(&source).expect("invalidate source");
        assert!(cache.ready_entry(&native()).is_none());
        assert!(cache.ready_entry(&other).is_some());
        assert!(cache.display_path(&native(), 512).is_none());
        assert!(cache.display_path(&other, 512).is_some());
        cache.invalidate_image(&other).expect("invalidate image");
        assert!(cache.ready_entry(&other).is_none());
        assert!(cache.display_path(&other, 512).is_none());
    }
}

fn remove_dir_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
