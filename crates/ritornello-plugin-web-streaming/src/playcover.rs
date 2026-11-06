//! The cover of what plays: the site's own thumbnail, the one a download
//! takes, shown by the player.
//!
//! **Fetched by the plugin, never by the core.** The address comes from a
//! probe, that is from remote data of any site; the plugin fetches it with
//! the very function a download uses ([`crate::download::fetch_jpg`] behind
//! `Limits::fetch_cover`: https, a size cap, timeouts, and the network guard
//! that refuses the local network on every address and redirect). The core is
//! then handed a **path** (`CoverRef::Path`), which it reads without any
//! network. Handing it the address instead would have the core fetch it with
//! a client that judges an address's shape, not where its name resolves:
//! the same thumbnail would cross two different guards depending on whether
//! it is downloaded or played.
//!
//! **Where the address comes from.** A playlist's videos carry theirs in the
//! cached list (the probe that filled it). A single video's preset keeps
//! none, so its page is probed once, in the background, the first time it
//! plays; after that the picture is on disk and nothing is looked up again.
//!
//! **On disk**, `covers/<sha256 of the identity>.jpg` in the plugin's data
//! directory: one file per video, whatever preset or list it plays from,
//! written atomically, at most [`KEPT_COVERS`] of them (the oldest go first).

use std::future::Future;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::download::CoverFetch;
use crate::entries::write_atomically;

/// How many pictures the cache keeps: a few long playlists' worth, a few
/// megabytes.
pub const KEPT_COVERS: usize = 500;

pub struct CoverCache {
    dir: PathBuf,
}

impl CoverCache {
    pub fn new(data: &Path) -> Self {
        Self { dir: data.join("covers") }
    }

    /// Where the picture of the video known as `identity` lives. Hashed: an
    /// identity is remote data (another site's `extractor:id`, or its link),
    /// never a file name.
    pub fn path_for(&self, identity: &str) -> PathBuf {
        let hex: String = Sha256::digest(identity.as_bytes()).iter().take(16).map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{hex}.jpg"))
    }

    /// The picture of `identity`, already on disk, or fetched now: from
    /// `known` (the address the list's probe gave), else from what `lookup`
    /// finds (a probe of the video's page), through `fetch`. `None` when there
    /// is none to have: a cover is a nicety, never an error.
    pub async fn fill<L, F>(&self, identity: &str, known: Option<String>, lookup: L, fetch: &CoverFetch) -> Option<PathBuf>
    where
        L: FnOnce() -> F,
        F: Future<Output = Option<String>>,
    {
        let path = self.path_for(identity);
        if path.is_file() {
            return Some(path);
        }
        let url = match known {
            Some(url) => url,
            None => lookup().await?,
        };
        let bytes = fetch(url).await?;
        let (dir, target) = (self.dir.clone(), path.clone());
        let written = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            std::fs::create_dir_all(&dir)?;
            write_atomically(&target, &bytes)?;
            prune(&dir, KEPT_COVERS);
            Ok(())
        })
        .await;
        match written {
            Ok(Ok(())) => Some(path),
            Ok(Err(e)) => {
                tracing::warn!("cover of {identity} not kept: {e:#}");
                None
            }
            Err(e) => {
                tracing::warn!("cover of {identity} not kept: {e}");
                None
            }
        }
    }
}

/// Keeps the `keep` most recent `.jpg` of `dir`, by modification time.
fn prune(dir: &Path, keep: usize) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut pictures: Vec<(std::time::SystemTime, PathBuf)> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    if pictures.len() <= keep {
        return;
    }
    pictures.sort_by_key(|p| std::cmp::Reverse(p.0));
    for (_, p) in pictures.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];

    fn fetching(calls: Arc<Mutex<Vec<String>>>, bytes: Option<&'static [u8]>) -> CoverFetch {
        Arc::new(move |url| {
            calls.lock().unwrap().push(url);
            Box::pin(async move { bytes.map(<[u8]>::to_vec) })
        })
    }

    #[tokio::test]
    async fn the_known_address_is_fetched_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CoverCache::new(dir.path());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fetch = fetching(calls.clone(), Some(JPEG));
        async fn never() -> Option<String> {
            panic!("no lookup when the address is known")
        }
        let path = cache.fill("video:a", Some("https://i.example/a.jpg".into()), never, &fetch).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), JPEG);
        assert!(path.starts_with(dir.path().join("covers")));
        // Played again: read from disk, nothing fetched.
        let again = cache.fill("video:a", Some("https://i.example/a.jpg".into()), never, &fetch).await.unwrap();
        assert_eq!(again, path);
        assert_eq!(*calls.lock().unwrap(), ["https://i.example/a.jpg"]);
    }

    #[tokio::test]
    async fn without_an_address_the_lookup_finds_it_and_without_one_there_is_no_cover() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CoverCache::new(dir.path());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fetch = fetching(calls.clone(), Some(JPEG));
        let found = cache.fill("video:b", None, || async { Some("https://i.example/b.jpg".to_string()) }, &fetch).await;
        assert!(found.is_some());
        assert_eq!(*calls.lock().unwrap(), ["https://i.example/b.jpg"]);
        assert_eq!(cache.fill("video:c", None, || async { None }, &fetch).await, None);
        // A fetch that fails (refused by the guard, not a jpeg...) leaves nothing.
        let refused = fetching(calls.clone(), None);
        assert_eq!(cache.fill("video:d", Some("https://i.example/d.jpg".into()), || async { None }, &refused).await, None);
        assert!(!cache.path_for("video:d").exists());
    }

    #[test]
    fn an_identity_never_becomes_a_path_of_its_own() {
        let cache = CoverCache::new(Path::new("/data"));
        let p = cache.path_for("../../etc/passwd");
        assert_eq!(p.parent().unwrap(), Path::new("/data/covers"));
        assert_ne!(cache.path_for("a"), cache.path_for("b"));
    }

    #[test]
    fn prune_keeps_the_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in ["old.jpg", "mid.jpg", "new.jpg"].iter().enumerate() {
            let p = dir.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        prune(dir.path(), 2);
        let mut left: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["mid.jpg", "new.jpg", "notes.txt"]);
    }
}
