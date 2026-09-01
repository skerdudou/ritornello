//! The cover of what is playing: fetch it, keep it, serve it.
//!
//! It is **the device** that goes and fetches the image, never the browser.
//! Three reasons: the page must not load any external resource — a principle
//! already established for the admin pages; the image becomes available to a
//! future graphical display; and a cover embedded in a file, which only the
//! device can read, would have no URL to hand to the browser.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ritornello_proto::CoverRef;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::RwLock;
use tokio_util::io::ReaderStream;

/// Cap for an image coming from the network. Rules out the bare `front` of the
/// Cover Art Archive, measured at 2,670,705 bytes where `front-500` returns
/// 75,249.
const NETWORK_CAP: usize = 2 * 1024 * 1024;

/// Prefix of the local URL published in `Track::cover_href`.
///
/// Shared between `metadata::Metadata::state`, which **builds** it, and
/// `main::display_relay`, which **re-reads** it to recover the cache key: two
/// literals could have drifted apart silently, and the consequence would have
/// been a display that never receives a cover again, with no error anywhere.
pub const HREF_PREFIX: &str = "/api/cover/";

/// Prefix of the temporary files produced by embedded-cover extraction,
/// dropped in `std::env::temp_dir()` by `player::mpv::embedded_cover`.
///
/// Shared between this module (purge at startup, bounded eviction) and
/// `mpv.rs` (naming): both must recognize exactly the same files, on pain of
/// either never purging them, or — worse — purging a file that is not ours.
pub const TEMP_PREFIX: &str = "ritornello-cover-";

/// True if `path` is a temporary extraction file created by this process.
///
/// **Never** true for a `folder.jpg` declared by a Source: that one lives on
/// the user's share, and the core must never delete it of its own accord.
/// `CoverPayload::File` carries both forms (see its doc); this is where the
/// distinction is made before acting on the disk.
fn is_cover_temp(path: &std::path::Path) -> bool {
    path.parent() == Some(std::env::temp_dir().as_path())
        && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(TEMP_PREFIX))
}

/// Sweeps the temporary files left by a previous run. Called once at startup,
/// before anything can create new ones.
///
/// **Two reasons, one of them about correctness.** Since `embedded_cover`
/// names its files after their content and only writes when the name is free,
/// a file left by a run **killed mid-write** would be truncated while carrying
/// the name of a complete image: the conditional write would adopt it, and a
/// display would receive a cut-off image. This sweep is what makes that case
/// impossible, and that is why it must run **before** anything can create a
/// temporary file.
///
/// The second reason is accumulation, and it is worth spelling out because one
/// could believe the system takes care of it: nothing else deletes these files
/// between two startups, and a `systemctl restart` does **not** clear
/// `std::env::temp_dir()` — on a Pi it is often a `tmpfs`, which only a real
/// reboot resets, and what piles up there eats RAM, not just disk. Relying on
/// `/tmp` would therefore have leaked exactly the most frequent case, the
/// service restart.
///
/// With no risk of purging something useful: the cache never survives a
/// restart (`CoverCache` is rebuilt at every launch), so nothing still lying
/// around here can be referenced by anything.
pub fn purge_temp_files() {
    purge_temp_files_in(&std::env::temp_dir());
}

/// Testable core of `purge_temp_files`, parameterized by the directory to
/// sweep.
///
/// `std::env::temp_dir()` is **shared** by the whole system, and by the other
/// tests of this same binary, which write real `ritornello-cover-*` files
/// there to exercise the extraction itself (see `player::mpv::tests`): running
/// a real sweep there from a test would put it in competition with them. Split
/// out so a test can point to a directory of its own, fully isolated.
fn purge_temp_files_in(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_str().is_some_and(|n| n.starts_with(TEMP_PREFIX))
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            tracing::debug!("purging leftover cover file {}: {e}", entry.path().display());
        }
    }
}

/// A cover frame under construction, shared between the caller building it
/// and those waiting for it.
///
/// The outer `Option` is the one of `line` — "nothing to push", for the same
/// reasons as everywhere in this module; the inner `Arc<str>` is the already
/// serialized line of text. The cell sits behind an `Arc` so the waiters can
/// hold it after releasing the table's lock.
type FrameInFlight = Arc<tokio::sync::OnceCell<Option<Arc<str>>>>;

/// What the core keeps of a cover.
///
/// Two natures, and it is deliberate: a **local** cover does not enter memory.
/// A three-megabyte `folder.jpg` is commonplace on a NAS, and loading it into
/// RAM on a Pi for an image the browser will cache on its side would be a
/// waste.
#[derive(Debug, Clone)]
pub enum CoverPayload {
    /// From the network: the bytes are in memory.
    Bytes(Vec<u8>, &'static str),
    /// Local: only the path is kept, the route re-reads the file.
    File(PathBuf),
}

/// Fingerprint of the source, published in the local URL.
///
/// `DefaultHasher` and not `sha2`: a collision would display the wrong cover
/// and nothing else, which does not justify a cryptographic dependency.
/// Computable **before** the download, which makes it possible to deduplicate
/// two requests for the same image.
pub fn key(r: &CoverRef) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match r {
        CoverRef::Url { url } => {
            0u8.hash(&mut h);
            url.hash(&mut h);
        }
        CoverRef::Path { path } => {
            1u8.hash(&mut h);
            path.hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

/// Fingerprint of an image's **content**, to name a temporary file.
///
/// Same hasher as `key`, and the same trade-off: a collision would display the
/// wrong cover and nothing else. What changes is what gets hashed — the bytes
/// of the image, not the path they come from. Two tracks of the same album
/// carrying the same embedded cover thus land on a single file, hence a single
/// `href`, hence nothing to push again nor to decode again: the embedded case
/// thereby joins the local `folder.jpg`, which was already free. Without that,
/// a fifteen-track album made a cache that only holds as many entries as the
/// setting allows (`CoverSettings::entries`) churn for nothing, extraction,
/// write and eviction included.
pub fn content_key(bytes: &[u8]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// What the core makes of a cover before pushing it onto a socket.
///
/// Absent (`CoverSettings::rendition` at `None`) when the user has unchecked
/// re-encoding: the original bytes leave as they are. An `Option` rather than
/// a boolean inside, and this is not cosmetic — the four settings only exist
/// where they mean something, so that code reading `max_edge_px` cannot forget
/// to first check that the rendition is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rendition {
    /// Longest edge of the thumbnail, in pixels. The aspect ratio is kept.
    pub max_edge_px: u32,
    /// JPEG quality, 1 to 100. Ignored for an image with an alpha channel,
    /// re-encoded as lossless PNG.
    pub jpeg_quality: u8,
    /// Cap on the produced thumbnail, in bytes. A safety net: beyond it,
    /// nothing is pushed.
    pub output_cap: usize,
    /// Cap on the pixels to decode. Compared against the dimensions read from
    /// the header **before any allocation**, and carried into `image::Limits`
    /// for the case of a header that would lie about its own dimensions.
    pub pixel_cap: u64,
}

/// The two stages of cover processing, not to be confused.
///
/// `source_max` bounds what the core agrees to **read**, whatever happens
/// next: it is the only guard that remains when the rendition is disabled, and
/// the cheapest of all, since it is judged on the file size without reading a
/// single byte of its content.
///
/// `rendition` only describes what the core **produces**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverSettings {
    /// How many covers the cache keeps. See
    /// `state::Settings::cover_cache_entries`.
    pub entries: usize,
    /// Cap on the source cover, in bytes.
    pub source_max: usize,
    /// `None` = push the source as it is.
    pub rendition: Option<Rendition>,
}

impl Default for CoverSettings {
    /// The product's defaults, not neutral defaults: a `CoverCache::new()`
    /// behaves like a device fresh out of the factory, including in tests that
    /// do not mention settings. Derived from `state::Settings::default()` so
    /// that there is only one place where these values are written.
    fn default() -> Self {
        Self::from(&crate::state::Settings::default())
    }
}

impl From<&crate::state::Settings> for CoverSettings {
    fn from(s: &crate::state::Settings) -> Self {
        Self {
            entries: s.cover_cache_entries as usize,
            source_max: (s.cover_source_max_mio as usize) * 1024 * 1024,
            rendition: s.cover_rendition.then(|| Rendition {
                max_edge_px: s.cover_max_edge_px,
                jpeg_quality: s.cover_jpeg_quality,
                output_cap: (s.cover_max_bytes_ko as usize) * 1024,
                pixel_cap: (s.cover_max_pixels_mpx as u64) * 1_000_000,
            }),
        }
    }
}

#[derive(Default)]
pub struct CoverCache {
    entries: RwLock<VecDeque<(String, CoverPayload)>>,
    /// Live settings, re-read at every publication.
    ///
    /// A `std::sync` lock and not tokio's, unlike `entries` just above: the
    /// critical section is the copy of a thirty-byte `Copy` structure, never
    /// an IO. This keeps `Core::set_settings` synchronous — making it `async`
    /// for this field would have contaminated its signature and all its test
    /// callers. The value is **copied out of the lock** before any `await`: no
    /// guard crosses a suspension point.
    settings: std::sync::RwLock<CoverSettings>,
    /// The frame builds in progress, one entry per key.
    ///
    /// **A rendezvous, not a cache — the distinction is everything.**
    /// Memorizing a frame would be wrong for the reason the `rendition` doc
    /// gives: the key hashes the *path*, not the content, so a kept thumbnail
    /// would become wrong as soon as the user replaces the image under that
    /// path. An entry here does not outlive its construction: the last caller
    /// out removes it, and the next caller starts over from a fresh read of
    /// the file.
    ///
    /// What this saves: two subscribed displays receiving the same state frame
    /// ask for the same cover at the same instant, and used to decode then
    /// re-encode the same image twice. On a Pi 2, that is one core busy for
    /// several hundred milliseconds, in duplicate.
    ///
    /// `tokio::sync::OnceCell::get_or_init` **is** the rendezvous: the first
    /// arrival executes, the followers wait for its result. The cell sits
    /// behind an `Arc` so the followers can hold it after releasing the
    /// table's lock — the lock never covers the work, only the registration.
    in_flight: tokio::sync::Mutex<HashMap<String, FrameInFlight>>,
    /// How many frame builds were **actually** executed.
    ///
    /// Under `cfg(test)`, and that is the right compromise. The rendezvous can
    /// only be proven by a count of executions: `Arc::ptr_eq` on the returned
    /// frames would show that an `Arc` is shared, which is already true
    /// without any rendezvous — each caller receives its own `Arc` over its
    /// own string, and nothing in the equality of the contents says how many
    /// times the image was decoded. Yet *that* is what we are saving.
    ///
    /// Nothing in service needs this number, so it does not enter the shipped
    /// binary: on a Pi 2, one more atomic counter is not a cost, but a field
    /// nobody reads is a debt.
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
    /// The thumbnails already built for the HTTP route, **cache key and ETag
    /// combined**.
    ///
    /// A cache, this time, and not a rendezvous like `in_flight` — the
    /// difference lies entirely in what serves as the key. `line` could not
    /// memorize anything because its key hashes the *path*: the user replaces
    /// the `folder.jpg` under that path and nothing invalidates the entry.
    /// Here the key additionally carries the ETag, that is, the file's
    /// modification date and size (see `file_etag`) — replacing the file
    /// therefore changes the key, and the old thumbnail is never served again.
    /// It then evicts itself on its own, like the rest.
    ///
    /// Without this, every load of the home page would re-decode and re-encode
    /// the image on a Pi 2, when the thumbnail is precisely what we build so
    /// the browser *does not* have to download three megabytes. The browser
    /// revalidates (`no-cache`), so the common case is a 304 with nothing
    /// built; this cache covers the first load of each new browser, and the
    /// multiple tabs of a single device.
    thumbnails: RwLock<VecDeque<Thumbnail>>,
    /// How many thumbnails were **actually** decoded and re-encoded.
    ///
    /// Under `cfg(test)`, the same trade-off as `builds` just above and for
    /// the same reason: the only proof that a cache saves work is a count of
    /// executions. Comparing two responses says nothing — two successive
    /// builds return the same bytes.
    #[cfg(test)]
    thumbnails_built: std::sync::atomic::AtomicUsize,
}

/// A retained thumbnail: its identity (cache key **plus** the source's ETag),
/// its MIME type, and its bytes.
///
/// A named type rather than a triple: the first field is the only one that
/// could be confused with another `String`, and it carries precisely the
/// property that makes this cache safe — see the `thumbnails` field.
struct Thumbnail {
    identity: String,
    mime: &'static str,
    bytes: Arc<Vec<u8>>,
}

/// Number of HTTP thumbnails retained. The same count as the cover cache
/// entries setting: beyond the current cover and a few previous ones, nobody
/// asks again.
const THUMBNAILS: usize = 4;

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The thumbnail already built for this identity, if there is one.
    async fn cached_thumbnail(&self, identity: &str) -> Option<(&'static str, Arc<Vec<u8>>)> {
        self.thumbnails
            .read()
            .await
            .iter()
            .find(|v| v.identity == identity)
            .map(|v| (v.mime, v.bytes.clone()))
    }

    /// Retains a thumbnail under its identity (key + ETag), evicting the
    /// oldest beyond `THUMBNAILS`.
    async fn remember_thumbnail(&self, identity: String, mime: &'static str, bytes: Arc<Vec<u8>>) {
        let mut v = self.thumbnails.write().await;
        v.retain(|e| e.identity != identity);
        v.push_back(Thumbnail { identity, mime, bytes });
        while v.len() > THUMBNAILS {
            v.pop_front();
        }
    }

    /// Builds — or retrieves — the thumbnail of `key`, under the identity
    /// `identity` (the cache key **plus** the source's ETag, see the
    /// `thumbnails` field).
    ///
    /// `None` means "no thumbnail to serve" without distinguishing the cases:
    /// re-encoding disabled by the user, unreadable image, dimensions beyond
    /// the cap. The caller then falls back to the original, which is the
    /// answer it would have given without this route.
    async fn thumbnail(&self, key: &str, identity: &str) -> Option<(&'static str, Arc<Vec<u8>>)> {
        if let Some(found) = self.cached_thumbnail(identity).await {
            return Some(found);
        }
        // A single read of the settings for the two stages, like `line`: two
        // reads could straddle a change and produce a thumbnail under rules
        // that never coexisted.
        let settings = self.settings();
        let wanted_rendition = settings.rendition?;
        let (mime, bytes) = self.bytes(key, settings.source_max).await?;
        #[cfg(test)]
        self.thumbnails_built.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (mime, bytes) = rendition(mime, bytes, wanted_rendition).await?;
        let bytes = Arc::new(bytes);
        self.remember_thumbnail(identity.to_string(), mime, bytes.clone()).await;
        Some((mime, bytes))
    }

    /// How many times a frame was built since the cache was created.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many thumbnails were built since the cache was created.
    #[cfg(test)]
    pub(crate) fn thumbnails_built(&self) -> usize {
        self.thumbnails_built.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Publishes new settings. Taken into account at the next publication:
    /// nothing is memorized, so there is nothing to invalidate.
    pub fn set_cover_settings(&self, r: CoverSettings) {
        // A poisoned lock would mean a holder panicked while holding thirty
        // `Copy` bytes — impossible without a defect elsewhere. Overwrite
        // rather than propagate: lost settings would silently degrade the next
        // publication, whereas the poisoning, for its part, shows up in the
        // log of the original panic.
        match self.settings.write() {
            Ok(mut g) => *g = r,
            Err(e) => *e.into_inner() = r,
        }
    }

    /// Copy of the current settings, lock released immediately.
    fn settings(&self) -> CoverSettings {
        match self.settings.read() {
            Ok(g) => *g,
            Err(e) => *e.into_inner(),
        }
    }

    pub async fn insert(&self, key: String, p: CoverPayload) {
        let mut e = self.entries.write().await;
        e.retain(|(k, _)| k != &key);
        e.push_back((key, p));
        // Re-read at every insertion: lowering the setting must reclaim the
        // memory at the next track, not at the next restart.
        let cap = self.settings().entries.max(1);
        while e.len() > cap {
            let Some((_, evicted)) = e.pop_front() else { break };
            // Bounds the accumulation **during** the process's lifetime, not
            // only at startup (see `purge_temp_files`): a session that runs
            // for months and walks a large library must not leave one file per
            // distinct track never replayed. Never touches a Source's
            // `folder.jpg`, which is not ours.
            if let CoverPayload::File(path) = &evicted
                && is_cover_temp(path)
                && let Err(err) = tokio::fs::remove_file(path).await
            {
                tracing::debug!("purging evicted cover file {}: {err}", path.display());
            }
        }
    }

    pub async fn contains(&self, key: &str) -> bool {
        self.entries.read().await.iter().any(|(k, _)| k == key)
    }

    async fn read(&self, key: &str) -> Option<CoverPayload> {
        self.entries.read().await.iter().find(|(k, _)| k == key).map(|(_, p)| p.clone())
    }

    /// Materializes the bytes of a cover: `(mime, bytes)`.
    ///
    /// **Precisely what the HTTP route avoids doing.** That one, for a local
    /// file, opens, checks the header and *streams* without ever holding the
    /// whole image. Pushing onto a socket leaves no such choice, hence this
    /// method — and hence the cap, which did not exist on the local side (see
    /// `COVER_MAX_BYTES` and the doc of `fetch`).
    ///
    /// `None` covers indistinctly: unknown key, file vanished or unreadable,
    /// share not answering, content that is no longer an image, and **size
    /// beyond the cap**. The caller has nothing to distinguish among them: in
    /// every case the display has no image, just as it has none when the fetch
    /// fails.
    /// The cap is **passed by the caller** rather than re-read here, so that
    /// `line` reads the settings only once: two reads could straddle a change,
    /// and produce a thumbnail under rules that never coexisted.
    async fn bytes(&self, key: &str, cap: usize) -> Option<(&'static str, Vec<u8>)> {
        // The lock is released **before** any IO. A local cover commonly lives
        // on a sleeping share: holding the read lock during `FILE_TIMEOUT`
        // would block the cache's insertions, hence the detached task of
        // `Core::start_cover_fetch`, for one image.
        //
        // The `Bytes` branch answers under the lock rather than going through
        // `read`: that one clones the whole `CoverPayload`, which would make
        // two copies of the bytes instead of one.
        let path = {
            let e = self.entries.read().await;
            match e.iter().find(|(k, _)| k == key).map(|(_, p)| p) {
                None => return None,
                // Already in memory, and already bounded by construction:
                // these bytes come from an HTTP body that `download` cut at
                // `NETWORK_CAP`.
                //
                // The configurable cap is checked anyway: it can be lowered
                // **below** `NETWORK_CAP`, and then the construction-time
                // bound no longer says anything. Without this check, the
                // setting would only apply to local files — true today by the
                // mere coincidence of the two values, and false as soon as one
                // of them is touched.
                Some(CoverPayload::Bytes(v, mime)) => {
                    if v.len() > cap {
                        tracing::warn!(
                            "network cover not pushed: {} bytes over the {cap}-byte limit",
                            v.len()
                        );
                        return None;
                    }
                    return Some((*mime, v.clone()));
                }
                Some(CoverPayload::File(c)) => c.clone(),
            }
        };
        read_file_bounded(&path, cap).await
    }

    /// Builds the `DisplayFrame::Cover` protocol line for `key`/`href`: the
    /// complete JSON, base64 included, terminated by a newline, ready to be
    /// written as is onto a socket.
    ///
    /// **Built at every call, never memorized, and that is the property that
    /// matters.** An encoded line kept from one call to the next was tried
    /// here, then removed: the cache key hashes the *path*, not the content,
    /// so a kept line became wrong as soon as the user replaced the image
    /// under that path. And the gesture leading there takes three clicks —
    /// disable the display from the admin page, replace the `folder.jpg`,
    /// re-enable it: the reconnected relay starts over with its deduplication
    /// guard at zero (`main::display_relay`, `CoverTracking`), asks for the
    /// current cover again, and used to receive the line from before. Nothing
    /// invalidated it because nothing *could* invalidate it: replacing a file
    /// on a share goes through no code of ours. A visibly wrong image is the
    /// worst defect of this device, far above a memory spike.
    ///
    /// **Sharing remains desirable, but structural rather than memorized.**
    /// The intended saving — paying once per *publication* for the
    /// materialization of the bytes and their base64, up to
    /// `COVER_MAX_BYTES`, rather than once per subscribed relay — is obtained
    /// by building the line **at publication time** and handing the same
    /// `Arc` to each relay. That is a full-blown rework: the construction
    /// reads a file, so it cannot settle on the core's main loop. And there
    /// was nothing to gain from anticipating it with a memo, because in
    /// service it had **no** second caller to serve: `wants_covers` is false
    /// by default, a single plugin overrides it, and `display_relay` only
    /// calls this function once per `cover_href` change. The MPD plugin does
    /// not come back through here either to serve its 8 KiB slices — it keeps
    /// its own copy of the received frame.
    ///
    /// **Never an `Arc` inside a serialized type**: what travels behind the
    /// returned `Arc` is the line of text already produced by `serde_json`,
    /// not a `ritornello_proto::Cover` value — that type remains an ordinary
    /// wire type, with no sharing to express. The `Arc` serves
    /// `DisplayClient::send_cover_line`, which writes these bytes as they are
    /// rather than copying and re-encoding.
    ///
    /// `None` covers the same cases as `bytes`: nothing to push.
    pub async fn line(&self, key: &str, href: &str) -> Option<Arc<str>> {
        // Registration at the rendezvous. The table's lock only covers the
        // registration itself — never the construction, which reads a file and
        // occupies a core. Holding it during the work would serialize
        // *different* keys, which is the opposite of the goal.
        let cell = {
            let mut in_flight = self.in_flight.lock().await;
            in_flight.entry(key.to_string()).or_insert_with(FrameInFlight::default).clone()
        };

        // `href` does not need to be compared between callers: `key` is
        // derived from it (`display_relay` extracts it from `href` via
        // `strip_prefix(HREF_PREFIX)`), so two callers with the same key carry
        // the same string. A follower does receive the frame of the first
        // arrival, and it describes the same image under the same name.
        let result = cell
            .get_or_init(|| async {
                #[cfg(test)]
                self.builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // A single read of the settings for the two stages: see
                // `bytes`. Two reads could straddle a change, and produce a
                // thumbnail under rules that never coexisted.
                let settings = self.settings();
                let (mime, bytes) = self.bytes(key, settings.source_max).await?;
                // The rendition applies **here and not in `bytes`**, so on the
                // push path only. The HTTP route `cover_get`, for its part,
                // streams the local file without ever holding it whole:
                // forcing a re-encode on it would make it lose exactly the
                // property that makes it cheap, for an image the browser
                // resizes and caches on its side.
                let (mime, bytes) = match settings.rendition {
                    None => (mime, bytes),
                    Some(r) => rendition(mime, bytes, r).await?,
                };
                let cover = ritornello_proto::Cover {
                    href: href.to_string(),
                    mime: mime.to_string(),
                    bytes,
                };
                let mut line =
                    serde_json::to_string(&ritornello_proto::DisplayFrame::Cover(cover)).ok()?;
                line.push('\n');
                Some(Arc::from(line))
            })
            .await
            .clone();

        // **The removal is what keeps the rendezvous from becoming a cache.**
        // A `OnceCell` keeps its value forever; left in the table, it would
        // serve the same thumbnail to a caller showing up one hour later, when
        // the file may have changed under its path.
        //
        // All callers attempt the removal, not just the first arrival: if that
        // one is abandoned midway (its task cancelled), a follower takes over
        // the initialization, and nobody else would be there to clean up.
        //
        // The identity is checked before removing: between the end of the work
        // and this lock, a more recent caller may have registered a **fresh**
        // cell under the same key. Removing it would make that caller lose its
        // rendezvous — not a correctness defect, but exactly the saving we are
        // installing here.
        {
            let mut in_flight = self.in_flight.lock().await;
            if in_flight.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
                in_flight.remove(key);
            }
        }
        result
    }
}

/// Re-encodes a cover into a thumbnail, or returns the original bytes when
/// there is nothing to gain.
///
/// Four steps, in this order, and the order **is** the protection:
///
/// 1. **The dimensions are read from the header**, without decoding. A few
///    dozen bytes suffice, and nothing is allocated at the image's size.
/// 2. **The bomb guard** compares the pixel count against the cap. It is the
///    only bound that truly protects: the file size says *nothing* about the
///    decoding cost — a 200 KiB PNG can announce 30000 × 30000 pixels, that
///    is a 3.6 GiB buffer, and `source_max` lets it through without blinking.
/// 3. **The pass-through**: an image already small in pixels *and* in bytes
///    leaves as it is, without decoding or re-encoding. A 300 × 300 cover
///    pulled from a file has nothing to gain from a round trip that would
///    degrade it.
/// 4. **The decoding and encoding**, on a blocking thread.
///
/// Swapping 2 and 1 would be absurd; swapping 3 and 2 would be dangerous —
/// a 30000 × 30000 image weighing 200 KiB would pass the pass-through on its
/// weight when it is precisely the bomb we are trying to refuse. The
/// pass-through therefore tests **both** criteria, and comes after the guard.
///
/// **Nothing is memorized**, and this is consistent with `line`: the cache
/// key hashes the path, not the content, so a kept thumbnail would become
/// wrong as soon as the user replaces the image under that path. The price is
/// one decode per publication, and `line` is only called once per cover
/// change and per subscribed relay.
///
/// `None` = nothing to push, as everywhere in this module: unreadable image,
/// dimensions beyond the cap, or produced thumbnail beyond the safety net.
async fn rendition(
    mime: &'static str,
    bytes: Vec<u8>,
    r: Rendition,
) -> Option<(&'static str, Vec<u8>)> {
    let (width, height) = dimensions(&bytes)?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > r.pixel_cap {
        tracing::warn!(
            "cover not pushed: {width}x{height} is {pixels} pixels, over the {} allowed \
             (decoding it would need about {} MiB)",
            r.pixel_cap,
            pixels * 4 / (1024 * 1024)
        );
        return None;
    }
    if width.max(height) <= r.max_edge_px && bytes.len() <= r.output_cap {
        tracing::debug!("cover already small ({width}x{height}, {} bytes), pushed as it is", bytes.len());
        return Some((mime, bytes));
    }

    // `spawn_blocking`: decoding then re-encoding a multi-megapixel image
    // occupies a core for hundreds of milliseconds on a Pi 2. Doing it on a
    // scheduler thread would freeze the core's loop — hence the position
    // clock, the remote-control commands and the HTTP requests — for the
    // duration of one cover.
    //
    // This task is **not cancellable**: dropping the future here does not stop
    // it, it will run to completion and its result will be thrown away. That
    // is acceptable precisely thanks to the guard of step 2, which bounds what
    // it can cost before launching it.
    let alloc_cap = (r.pixel_cap as usize).saturating_mul(4);
    let work = tokio::task::spawn_blocking(move || encode(bytes, r, alloc_cap)).await;
    let (mime, output) = match work {
        Ok(Some(v)) => v,
        Ok(None) => return None,
        Err(e) => {
            // A decoder panic on an input coming from the network: refused
            // like the rest, but logged at `warn` — it is a defect of the
            // library or an input that broke it, not a use case.
            tracing::warn!("cover rendition panicked: {e}");
            return None;
        }
    };
    if output.len() > r.output_cap {
        tracing::warn!(
            "cover not pushed: rendered to {} bytes, over the {}-byte net",
            output.len(),
            r.output_cap
        );
        return None;
    }
    tracing::debug!(
        "cover rendered: {} bytes in, {} bytes out ({mime})",
        pixels * 4,
        output.len()
    );
    Some((mime, output))
}

/// Dimensions announced by the header, without decoding the image.
///
/// Split out to be testable on its own: it is the value the bomb guard
/// depends on, and a guard that misreads its dimensions guards nothing.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    match reader.into_dimensions() {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::debug!("cover header unreadable: {e}");
            None
        }
    }
}

/// The decoding and encoding themselves. **Blocking**: called under
/// `spawn_blocking`.
fn encode(bytes: Vec<u8>, r: Rendition, alloc_cap: usize) -> Option<(&'static str, Vec<u8>)> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    // Belt after the braces: the guard in `rendition` has already refused
    // oversized dimensions, but it believes the header. `Limits` bounds the
    // decoder's actual allocation, so it covers the case of a header that
    // would lie about its own dimensions — the file crafted on purpose, not
    // the clumsy one.
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(alloc_cap as u64);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!("cover undecodable: {e}");
            return None;
        }
    };

    // `thumbnail` and not `resize`: at comparable sampling quality for a
    // strong reduction (every source pixel contributes to a target pixel), it
    // is markedly cheaper — and on a Pi 2 that is the deciding factor. The
    // aspect ratio is kept, the image fits in the requested square.
    let thumbnail = image.thumbnail(r.max_edge_px, r.max_edge_px);

    let mut output = Vec::new();
    // PNG as soon as there is an alpha channel, lossless. Flattening the
    // transparency would require choosing a background color — a visual
    // stance the device has no business taking on somebody else's cover.
    if thumbnail.color().has_alpha() {
        if let Err(e) = thumbnail.write_to(&mut std::io::Cursor::new(&mut output), image::ImageFormat::Png) {
            tracing::warn!("cover PNG encoding failed: {e}");
            return None;
        }
        return Some(("image/png", output));
    }
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, r.jpeg_quality);
    // `to_rgb8`: the JPEG encoder refuses a buffer with an alpha channel, and
    // a grayscale or paletted image has to be converted anyway.
    if let Err(e) = encoder.encode_image(&thumbnail.to_rgb8()) {
        tracing::warn!("cover JPEG encoding failed: {e}");
        return None;
    }
    Some(("image/jpeg", output))
}

/// What the bounded read of a cover file returns, before the image type is
/// validated.
enum BoundedRead {
    Bytes(Vec<u8>),
    /// The file's size, **known through `metadata`, before any read of the
    /// bytes themselves**: see the doc of `read_file_bounded`.
    TooLarge(u64),
}

/// Reads a cover file in order to push it, bounded and validated.
///
/// **The header validation is done on the returned bytes themselves**, not on
/// a separate first read. The HTTP route, for its part, cannot: it must check
/// then stream, so it takes care to keep the *same descriptor* between the two
/// reads — otherwise a contributor could replace the share's content between
/// the check and the serving. Here the checked content **is** the returned
/// content, a single descriptor and a single read: the window does not exist
/// at all, rather than being closed. The guarantee is therefore not weakened
/// but strengthened.
///
/// **The size is checked before any read of the bytes**, via `metadata`, and
/// that is deliberate: a file size requires no knowledge of the format — no
/// header to interpret, no decoder, indifferent to a JPEG, a PNG, a WebP or
/// whatever comes next. An outsized file on the NAS (the 150 MB PNG that
/// `cover_get` cites as a real case) is thus refused without a single byte of
/// its content being read, rather than being discovered after a read bounded
/// at `COVER_MAX_BYTES + 1` bytes — a cost that only makes sense if the file
/// passes the bound. `take` before `read_to_end` stays in place afterwards, as
/// a safety net: if the file grows *between* the `metadata` and the read, the
/// reopened TOCTOU window never lets more than `COVER_MAX_BYTES + 1` bytes be
/// read.
///
/// Two time bounds under the same timeout, and a size one before anything:
///
/// * `metadata` then, if the size passes, at most `COVER_MAX_BYTES + 1` bytes
///   are read (the TOCTOU net above).
/// * `FILE_TIMEOUT`, as everywhere this module touches a file: the share may
///   be asleep, and the wait must be bounded by us rather than by the kernel.
async fn read_file_bounded(
    path: &std::path::Path,
    cap: usize,
) -> Option<(&'static str, Vec<u8>)> {
    let read_attempt = tokio::time::timeout(FILE_TIMEOUT, async {
        let file = tokio::fs::File::open(path).await?;
        let size = file.metadata().await?.len();
        if size > cap as u64 {
            return Ok::<_, std::io::Error>(BoundedRead::TooLarge(size));
        }
        let mut bytes = Vec::new();
        // `take` **before** `read_to_end`: `read_to_end` alone would read the
        // whole file, and the size check would come after the very allocation
        // it is supposed to avoid. Only acts here on the TOCTOU window (see
        // the doc above): the common case has already been settled by
        // `metadata`.
        file.take(cap as u64 + 1).read_to_end(&mut bytes).await?;
        Ok(BoundedRead::Bytes(bytes))
    })
    .await;
    let bytes = match read_attempt {
        Ok(Ok(BoundedRead::Bytes(v))) => v,
        Ok(Ok(BoundedRead::TooLarge(size))) => {
            // The exact size of the offense, known without having read any of
            // its content — which is what the read bounded at `+ 1` byte
            // could never log: it would never see anything but `cap + 1`,
            // whatever the actual size.
            tracing::warn!(
                "cover file {} not read: {size} bytes over the {cap}-byte limit",
                path.display()
            );
            return None;
        }
        Ok(Err(e)) => {
            tracing::debug!("cover file unreadable: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
            return None;
        }
    };
    if bytes.len() > cap {
        tracing::warn!(
            "cover file {} not pushed: grew past {cap} bytes while being read",
            path.display()
        );
        return None;
    }
    let mime = image_type(&bytes)?;
    Some((mime, bytes))
}

/// Header bytes of a recognized image. Checked before serving a local file:
/// without this, a badly written contributor could get any file of the system
/// served on a public HTTP route.
fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Number of redirect hops tolerated, `reqwest`'s default value.
///
/// Restated explicitly: replacing the default policy with a custom one also
/// loses its cap, and an endless redirect chain is a denial of service at the
/// cost of one round trip.
const MAX_HOPS: usize = 10;

/// Shared HTTP client: building it at every call would redo the `rustls`
/// configuration and the loading of the root store each time, which
/// `reqwest`'s documentation asks precisely to avoid. The configuration is
/// frozen (no proxy, no user input): a build failure would be a defect of the
/// environment, not a per-request outage, hence the `expect`.
///
/// **Redirects are followed, but every hop is revalidated.** The design
/// requires following them (Radio France answers a cross-host 301, measured),
/// and `reqwest`'s default policy followed them without checking anything:
/// `allowed_target` only applied to the starting URL, so the image host — a
/// third party the design precisely does not trust, since OUI FM's `coverUrl`
/// is written by someone else — only had to answer
/// `302 http://192.168.1.1/…` to make the device issue a GET on its local
/// network, scheme change included. One hop of indirection cancelled the
/// entire safeguard.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!(
                "ritornello/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/skerdudou/ritornello)"
            ))
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::custom(|hop| {
                if hop.previous().len() >= MAX_HOPS {
                    return hop.stop();
                }
                if allowed_target(hop.url().as_str()) {
                    hop.follow()
                } else {
                    tracing::debug!("cover redirect refused: target not allowed");
                    hop.stop()
                }
            }))
            .build()
            .expect("frozen HTTP configuration, must never fail")
    })
}

/// Is the target acceptable for an outgoing request?
///
/// The check lives here, and not in `ritornello-proto`, for two reasons: this
/// is where the request leaves from, and replaying URL parsing rules by hand
/// is a losing race — a trailing dot (`192.168.1.1.`) or a hexadecimal label
/// (`0x7f.0.0.1`) is enough to make a literal address pass for a hostname in
/// front of `ritornello-proto`'s string splitting. `Url::domain()` relies on
/// the WHATWG parsing already done by `reqwest` (re-exported, so no extra
/// dependency): it classifies the host as IPv4/IPv6 **after** normalization,
/// whatever its original spelling, and only returns `Some` for a real domain
/// name.
///
/// `ritornello-proto` guards the form (https, extension); this module guards
/// the target: it is the one issuing the request, and it is the SSE of a
/// third-party source (OUI FM, for example) that can supply the URL.
///
/// Applied to **every** target reached, not only the first: `fetch` filters
/// the starting URL, the redirect policy of `client()` filters all subsequent
/// hops.
fn allowed_target(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    if u.scheme() != "https" {
        return false;
    }
    match u.domain() {
        // `None` covers both the absence of a host and a literal IP address
        // (v4 or v6): `domain()` only returns `Some` for a domain name, never
        // for a `HostInternal::Ipv4`/`Ipv6`.
        Some(d) => d.contains('.'),
        None => false,
    }
}

/// Performs the request and applies the three network safeguards: the
/// `Content-Type`, the cap applied while reading chunk by chunk, and the magic
/// bytes of the received body. Split from `fetch` to stay testable against a
/// local HTTP server (`127.0.0.1`) without ever going through
/// `allowed_target`, which would refuse precisely that address.
async fn download(url: &str) -> Option<CoverPayload> {
    let mut response = client().get(url).send().await.ok()?;
    if !response.status().is_success() {
        tracing::debug!("cover fetch returned {}", response.status());
        return None;
    }
    let mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !mime.starts_with("image/") {
        tracing::debug!("cover fetch refused: content-type {mime:?}");
        return None;
    }
    // Cap applied **while reading chunk by chunk**: checking the announced
    // `Content-Length` protects from nothing, it is declarative.
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > NETWORK_CAP {
            tracing::debug!("cover fetch refused: over {NETWORK_CAP} bytes");
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let mime = image_type(&bytes)?;
    Some(CoverPayload::Bytes(bytes, mime))
}

/// Timeout granted to an access to the image file itself — opening,
/// `metadata`, first bytes.
///
/// **The same as the one for embedded extraction** (`health::TIMEOUT`), and
/// for the same reason: these two paths touch files that commonly live on a
/// sleeping SMB share, and this project has already lived through the outage
/// that an IO that never completes causes. No event loop is held here — the
/// fetch is detached, the HTTP route is one task per request — so the audio
/// risks nothing; what is bounded is the wait itself, rather than letting it
/// last as long as the kernel pleases.
///
/// A time bound and not `Health`: the circuit breaker takes a **blocking
/// closure** (`spawn_blocking`), whereas these two paths are already
/// asynchronous and, in the case of `cover_get`, must return a
/// `tokio::fs::File` to stream. Fitting them in would require going back to
/// `std::fs` then converting again, and wiring the circuit breaker all the
/// way into the HTTP `AppState` — a rework for a property the bound already
/// gives. What `Health` would bring in addition, and that we therefore do not
/// have here, is the memory of the mute mount: one thread of the blocking
/// pool remains lost per attempt, exactly what `health.rs` documents as
/// unavoidable once the kernel is gone.
const FILE_TIMEOUT: std::time::Duration = crate::health::TIMEOUT;

/// Goes and fetches the cover. `None` = failure, and the failure is
/// **silent**: the device simply displays no image.
pub async fn fetch(r: &CoverRef) -> Option<CoverPayload> {
    match r {
        CoverRef::Path { path } => {
            let path = PathBuf::from(path);
            let to_read = path.clone();
            // Opening **and** first read under the same bound: it is the
            // opening that blocks on a sleeping share, but a share that
            // answers the `open` and no longer the `read` is the case of a
            // disconnection in progress — both must be covered.
            let recognized = tokio::time::timeout(FILE_TIMEOUT, async move {
                let mut file = tokio::fs::File::open(&to_read).await.ok()?;
                let mut head = [0u8; 12];
                let n = file.read(&mut head).await.ok()?;
                image_type(&head[..n])
            })
            .await;
            match recognized {
                Ok(Some(_)) => {}
                Ok(None) => return None,
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
                    return None;
                }
            }
            // The cap does not apply to local files: it protects against a
            // third party on the network, and a file on the NAS is trusted.
            // Its header bytes have been checked, that is what matters. The
            // route will re-read the file at serving time: between the two,
            // the share is no longer under the device's control (see
            // `cover_get`).
            Some(CoverPayload::File(path))
        }
        CoverRef::Url { url } => {
            if !allowed_target(url) {
                tracing::debug!("cover fetch refused: target not allowed");
                return None;
            }
            download(url).await
        }
    }
}

/// ETag of a local file: unlike the cache key — which hashes the **source**
/// (the path), not the content — this file remains modifiable afterwards on
/// its share. The ETag must therefore follow the content, not just the path,
/// otherwise a conditional request would validate forever an image the user
/// has in fact replaced.
fn file_etag(modified: Option<std::time::SystemTime>, size: u64) -> String {
    let nanos = modified
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("\"{nanos:x}-{size:x}\"")
}

/// What the route knows how to serve.
///
/// **Two sizes and not one, because the page has two uses for it.** The card's
/// square is 224 px on a phone: loading the three-megabyte `folder.jpg` of a
/// NAS into it is pure waste, especially over Wi-Fi. But the same image
/// enlarged on click (see `PlayerCard.vue`) deserves, for its part, all its
/// pixels. The size is therefore **requested by the caller** rather than
/// guessed here.
///
/// Full size by default, and that is deliberate: the `cover_href` published in
/// the state designates the image as it is, without transformation, for any
/// present or future consumer of the protocol. The thumbnail is a service
/// rendered to whoever asks for it, not a change of what the bare URL means.
/// The query string of `cover_get`.
///
/// **A free-form string and not a serialized enumeration**, and this is a
/// fix: an `enum` made the extractor `Query` refuse the whole request — a
/// `?size=nawak` returned a 400, hence the ♫ fallback on the page, for a mere
/// typo in a URL. An unknown value must mean the default, like an absent
/// value: the size is a service rendered to whoever asks for it, never a
/// condition of service.
#[derive(Debug, Default, serde::Deserialize)]
pub struct CoverParams {
    #[serde(default)]
    size: Option<String>,
}

/// The word that requests the reduction, as the page writes it in its URL.
const THUMBNAIL_SIZE: &str = "thumbnail";

/// `GET /api/cover/{key}[?size=thumbnail]`. The key is a fingerprint of the
/// **source**, so its immutability says nothing about the content: a network
/// cover is indeed frozen under its key (it comes from a body already fully
/// checked), but a local file remains modifiable on its share afterwards.
pub async fn cover_get(
    State(state): State<crate::status::AppState>,
    Path(key): Path<String>,
    axum::extract::Query(params): axum::extract::Query<CoverParams>,
    headers: HeaderMap,
) -> Response {
    let thumbnail_requested = params.size.as_deref() == Some(THUMBNAIL_SIZE);
    let Some(p) = state.covers.read(&key).await else {
        // **A `warn`, and it was missing.** This key was published in
        // `cover_href` by the core itself: no longer knowing how to serve it
        // is a broken promise, not an ordinary case. The cache only keeps
        // `CoverSettings::entries` entries, so the suspect is eviction — this
        // is the line that will say so, where the screen only showed a ♫ with
        // no explanation and the owner reported "no warn at all".
        tracing::warn!("cover {key} requested but no longer in the cache (evicted?)");
        return (StatusCode::NOT_FOUND, "inconnue").into_response();
    };
    match p {
        CoverPayload::Bytes(bytes, mime) => {
            // A network cover is frozen under its key: its ETag has nothing to
            // carry beyond the key and the requested size, and its thumbnail
            // is as immutable as it is.
            if thumbnail_requested {
                let etag = format!("\"{key}-v\"");
                if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok())
                    == Some(etag.as_str())
                {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                if let Some((mime, small)) =
                    state.covers.thumbnail(&key, &format!("{key}:v")).await
                {
                    return (
                        [
                            (header::CONTENT_TYPE, mime.to_string()),
                            (
                                header::CACHE_CONTROL,
                                "public, max-age=31536000, immutable".to_string(),
                            ),
                            (header::ETAG, etag),
                        ],
                        small.as_slice().to_vec(),
                    )
                        .into_response();
                }
                // No thumbnail (re-encoding disabled, unreadable image,
                // dimensions beyond the cap): the original, which is the
                // answer we would have given without this parameter. Better an
                // oversized image than no image.
            }
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
                    (header::ETAG, format!("\"{key}\"")),
                ],
                bytes,
            )
                .into_response()
        }
        CoverPayload::File(path) => {
            // Opening and `metadata` under a time bound: this file commonly
            // lives on a network share, and this route is reachable by any
            // browser on the LAN. Without a bound, a sleeping share held the
            // request for as long as the kernel wanted — the very incident
            // `health.rs` exists to bound. The expiry is handled like the
            // unreadability that already existed: a 404, which the UI renders
            // through its ♫ fallback.
            //
            // Bounded **in two stages**, the header just below: keeping the
            // 304 answer ahead of any read of the body is what makes a
            // conditional request genuinely cheap.
            let opening = tokio::time::timeout(FILE_TIMEOUT, async {
                let file = tokio::fs::File::open(&path).await?;
                let meta = file.metadata().await?;
                Ok::<_, std::io::Error>((file, meta))
            })
            .await;
            let (mut file, meta) = match opening {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    // `warn` and not `debug`: the core published this
                    // `cover_href`, so failing to serve it is a defect visible
                    // on screen and must be visible in the log. At `debug` it
                    // was visible nowhere.
                    tracing::warn!("cover {key} unreadable: {e}");
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
            };
            // The thumbnail's ETag is not the original's: it is the same
            // source content but not the same served bytes, and two different
            // responses under a single validator would make the browser serve
            // one for the other.
            let source_etag = file_etag(meta.modified().ok(), meta.len());
            let etag = if thumbnail_requested {
                format!("\"v-{}\"", source_etag.trim_matches('"'))
            } else {
                source_etag.clone()
            };
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            // **After the 304 and not before**: a conditional request must
            // build nothing, and that is what makes the thumbnail cheap in
            // steady state. The memorized identity carries the source's ETag,
            // so a `folder.jpg` replaced on the share changes key and its old
            // thumbnail is never served again.
            if thumbnail_requested {
                let identity = format!("{key}:{source_etag}");
                if let Some((mime, small)) = state.covers.thumbnail(&key, &identity).await {
                    return (
                        [
                            (header::CONTENT_TYPE, mime.to_string()),
                            (header::CACHE_CONTROL, "no-cache".to_string()),
                            (header::ETAG, etag),
                        ],
                        small.as_slice().to_vec(),
                    )
                        .into_response();
                }
                // Nothing to shrink: we fall back to streaming the original,
                // below, with the thumbnail's ETag — the content served under
                // this URL stays consistent with its validator, which is all
                // the cache requires.
            }
            // Revalidation of the header bytes at serving time, and not only
            // at discovery time (`fetch`): between the two, the share is not
            // under the device's control, and a contributor who replaced the
            // content must not get just anything served under this public
            // route. Same file descriptor for the check and for the stream
            // served next: the content cannot change between the two reads.
            //
            // Second bound, on the read this time: a share that answers the
            // `open` and no longer the first `read` is the case of a
            // disconnection in progress, and nothing would rule it out
            // otherwise.
            let header_read = tokio::time::timeout(FILE_TIMEOUT, async {
                let mut head = [0u8; 12];
                let n = file.read(&mut head).await?;
                file.seek(std::io::SeekFrom::Start(0)).await?;
                Ok::<_, std::io::Error>((head, n))
            })
            .await;
            let (head, n) = match header_read {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    // `warn` and not `debug`: the core published this
                    // `cover_href`, so failing to serve it is a defect visible
                    // on screen and must be visible in the log. At `debug` it
                    // was visible nowhere.
                    tracing::warn!("cover {key} unreadable: {e}");
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
            };
            let Some(mime) = image_type(&head[..n]) else {
                tracing::warn!(
                    "cover {key} is no longer an image: {}",
                    path.display()
                );
                return (StatusCode::NOT_FOUND, "illisible").into_response();
            };
            // Streamed, not a single `Vec`: this route is reachable without
            // authentication from the LAN, and a local file has by design no
            // size cap. A 150 MB PNG on the share, or a few concurrent
            // requests on a file of a few megabytes, must not exhaust a Pi's
            // memory.
            let body = Body::from_stream(ReaderStream::new(file));
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                    (header::ETAG, etag),
                ],
                body,
            )
                .into_response()
        }
    }
}

/// Image fixtures shared by this module's tests and those of `main`.
///
/// Here and not in each `mod tests`: two copies of an image generator would
/// drift apart, and a test that believes it produces a decodable image when it
/// no longer does is a silent false positive.
#[cfg(test)]
pub(crate) mod fixtures {
    /// A **genuinely decodable** JPEG of `width × height`.
    ///
    /// Needed as soon as a test goes through `CoverCache::line`: the
    /// rendition, enabled by default, decodes the image, and a header followed
    /// by padding is refused — rightly so, it is a truncated file.
    ///
    /// A gradient and not a flat fill: a flat fill compresses down to a few
    /// hundred bytes whatever its size, which would make "the thumbnail was
    /// produced" and "the output cap was never approached" indistinguishable.
    pub fn jpeg_decodable(width: u32, height: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(width, height);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        }
        let mut output = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 90)
            .encode_image(&img)
            .expect("fixture encoding");
        output
    }

    /// A decodable PNG **with an alpha channel**, for the lossless path.
    pub fn png_alpha(width: u32, height: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(width, height);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x % 256) as u8, (y % 256) as u8, 0, ((x + y) % 256) as u8]);
        }
        let mut output = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut output), image::ImageFormat::Png)
            .expect("fixture encoding");
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::CoverRef;

    #[test]
    fn the_key_is_stable_and_distinguishes_two_sources() {
        let a = CoverRef::Url { url: "https://x.org/a.jpg".into() };
        let b = CoverRef::Url { url: "https://x.org/b.jpg".into() };
        assert_eq!(key(&a), key(&a), "the key must be stable: it is published in a URL");
        assert_ne!(key(&a), key(&b));
        // A different form for the same string must not collide.
        assert_ne!(key(&a), key(&CoverRef::Path { path: "/https://x.org/a.jpg".into() }));
        // Hexadecimal, so no surprises inside a URL path.
        assert!(key(&a).chars().all(|c| c.is_ascii_hexdigit()), "{}", key(&a));
    }

    /// The body served by the real HTTP route for this key and this size.
    ///
    /// Through `status::router` and a real request, like
    /// `the_http_route_serves_what_the_core_just_deposited` (in
    /// `core::track_metadata`): the extractor chain (including the `Query`
    /// that reads `size`) is part of what is being exercised, and calling the
    /// handler as a function would short-circuit it.
    async fn served_body(cache: &Arc<CoverCache>, key: &str, query: &str) -> (u16, Vec<u8>) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let app = crate::status::router(crate::status::AppState {
            covers: cache.clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get(format!("/api/cover/{key}{query}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn the_route_serves_a_thumbnail_when_asked_and_the_original_otherwise() {
        // **The owner's request**: the 224 px square of the home page must no
        // longer download a NAS's whole `folder.jpg`. The bare URL, for its
        // part, does not change meaning — it is the one the enlarged view
        // loads, and it must return all the pixels.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let large = fixtures::jpeg_decodable(1500, 1500);
        std::fs::write(&path, &large).unwrap();
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".to_string(), CoverPayload::File(path)).await;

        let (status, full) = served_body(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(full.len(), large.len(), "the bare URL serves the file as it is");

        let (status, thumbnail) = served_body(&cache, "k", "?size=thumbnail").await;
        assert_eq!(status, 200);
        assert!(
            thumbnail.len() < full.len(),
            "the thumbnail must weigh less ({} against {})",
            thumbnail.len(),
            full.len()
        );
        let (w, h) = dimensions(&thumbnail).expect("the thumbnail must remain a readable image");
        let edge = crate::state::Settings::default().cover_max_edge_px;
        assert!(w <= edge && h <= edge, "thumbnail {w}x{h}, cap {edge}");
    }

    #[tokio::test]
    async fn an_unknown_size_falls_back_to_the_original_rather_than_an_error() {
        // A malformed URL must not make the cover unfindable: the page's
        // square would show the ♫ fallback for a typo.
        let cache = Arc::new(CoverCache::new());
        let bytes = fixtures::jpeg_decodable(40, 40);
        cache.insert("k".to_string(), CoverPayload::Bytes(bytes.clone(), "image/jpeg")).await;
        let (status, body) = served_body(&cache, "k", "?size=nawak").await;
        assert_eq!(status, 200);
        assert_eq!(body, bytes);
    }

    #[tokio::test]
    async fn a_file_thumbnail_is_only_built_once() {
        // Decoding then re-encoding costs hundreds of milliseconds on a Pi 2:
        // two browsers opening the page must not pay for it twice. The
        // memorized identity carries the source's ETag, so nothing stale can
        // be served (see the `thumbnails` field).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(1200, 1200)).unwrap();
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".to_string(), CoverPayload::File(path.clone())).await;

        let (_, first) = served_body(&cache, "k", "?size=thumbnail").await;
        let (status, second) = served_body(&cache, "k", "?size=thumbnail").await;
        assert_eq!(status, 200);
        assert_eq!(first, second);
        // **The count is the only proof**: comparing the two responses says
        // nothing, two successive builds return the same bytes. Same reason as
        // the rendezvous's `builds` counter.
        assert_eq!(cache.thumbnails_built(), 1, "the second request must be free");
    }

    #[tokio::test]
    async fn the_cache_is_bounded_by_the_setting_and_forgets_the_oldest() {
        // **The bound is now a setting** (`cover_cache_entries`, 20 by
        // default) and not a constant: the test therefore sets it itself,
        // which proves at the same time that it is indeed read at every
        // insertion.
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { entries: 4, ..CoverSettings::default() });
        for i in 0..6 {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![i as u8], "image/jpeg")).await;
        }
        assert!(!cache.contains("k0").await);
        assert!(!cache.contains("k1").await);
        assert!(cache.contains("k5").await);
    }

    #[tokio::test]
    async fn lowering_the_setting_reclaims_memory_at_the_next_insertion() {
        // The setting is re-read **at every insertion**: lowering it must not
        // wait for a restart to give the memory back, otherwise setting it is
        // useless while the device is playing.
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { entries: 10, ..CoverSettings::default() });
        for i in 0..10 {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![i as u8], "image/jpeg")).await;
        }
        assert!(cache.contains("k0").await, "precondition: all ten fit");

        cache.set_cover_settings(CoverSettings { entries: 3, ..CoverSettings::default() });
        cache.insert("fresh".into(), CoverPayload::Bytes(vec![99], "image/jpeg")).await;

        assert!(cache.contains("fresh").await);
        assert!(cache.contains("k9").await, "the most recent ones stay");
        assert!(!cache.contains("k0").await, "the oldest ones leave right away");
        assert!(!cache.contains("k7").await);
    }

    /// Out-of-bounds eviction must reclaim the space of the temporary
    /// extraction files it pushes out of the cache — otherwise nothing else
    /// ever deletes them during the process's lifetime — but must **never**
    /// touch a `folder.jpg` declared by a Source, which lives on its own
    /// share.
    #[tokio::test]
    async fn eviction_deletes_our_own_temp_file_but_never_a_source_folder_jpg() {
        // Unique name guaranteed by `tempfile`, in the real system temporary
        // directory: that is where, and only where, `is_cover_temp`
        // recognizes a file as ours. A random name avoids any collision with
        // the files that other tests of this same binary write there in
        // parallel (see `player::mpv::tests`).
        let our_file = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .suffix(".jpg")
            .tempfile_in(std::env::temp_dir())
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        // A Source's `folder.jpg` lives elsewhere, never in the system
        // temporary directory: simulated here in a directory of its own.
        let source_dir = tempfile::tempdir().unwrap();
        let folder_jpg = source_dir.path().join("folder.jpg");
        std::fs::write(&folder_jpg, b"x").unwrap();

        let cache = CoverCache::new();
        // Bound set explicitly: the default value is twenty entries, which
        // this test does not want to have to fill.
        cache.set_cover_settings(CoverSettings { entries: 4, ..CoverSettings::default() });
        cache.insert("to-keep".into(), CoverPayload::File(folder_jpg.clone())).await;
        cache.insert("ours".into(), CoverPayload::File(our_file.clone())).await;
        // Enough insertions to exceed the bound and evict the first two.
        for i in 0..4u8 {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![i], "image/jpeg")).await;
        }

        assert!(!our_file.exists(), "one of our own temp files, once evicted, must be deleted from disk");
        assert!(folder_jpg.exists(), "a Source's folder.jpg must never be deleted on our own initiative");
    }

    #[test]
    fn purge_deletes_our_own_files_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join(format!("{TEMP_PREFIX}abcd1234.jpg"));
        let not_ours = dir.path().join("folder.jpg");
        std::fs::write(&ours, b"x").unwrap();
        std::fs::write(&not_ours, b"y").unwrap();

        purge_temp_files_in(dir.path());

        assert!(!ours.exists(), "a file of ours, left over from a previous run, must disappear");
        assert!(not_ours.exists(), "a file that is not ours must never be touched");
    }

    #[tokio::test]
    async fn a_local_file_that_is_not_an_image_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("folder.jpg");
        std::fs::write(&fake, b"this is not an image").unwrap();
        let r = CoverRef::Path { path: fake.to_string_lossy().into_owned() };
        assert!(
            fetch(&r).await.is_none(),
            "the header bytes must be checked: without this, a badly written contributor \
             would get any file of the system served on a public HTTP route"
        );

        let real = dir.path().join("cover.jpg");
        // Minimal JPEG header: SOI + APP0 marker.
        std::fs::write(&real, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = CoverRef::Path { path: real.to_string_lossy().into_owned() };
        match fetch(&r).await {
            Some(CoverPayload::File(p)) => assert_eq!(p, real),
            other => panic!("a local image must stay a path, not bytes: {other:?}"),
        }
    }

    // -- `bytes`: the materialization for the display protocol -------------

    /// The source cap of the default settings.
    ///
    /// The `bytes` tests below are about the cap, not the rendition: passing
    /// it explicitly makes the bound being exercised visible in the test,
    /// where it used to be hidden in a module constant. Taking it from the
    /// **default** settings rather than from `COVER_MAX_BYTES` directly is
    /// deliberate: it is the value a device fresh out of the factory actually
    /// applies.
    fn cap() -> usize {
        CoverSettings::default().source_max
    }

    /// Minimal JPEG header, followed by `padding` arbitrary bytes.
    ///
    /// **Undecodable on purpose**: these bytes validate the header that
    /// `image_type` inspects, and nothing more. That suits everything about
    /// sizes and caps, and it does **not** suit anything about the rendition —
    /// see the real-image fixtures, further down.
    fn jpeg(padding: usize) -> Vec<u8> {
        let mut v = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        v.resize(6 + padding, 0x42);
        v
    }

    #[tokio::test]
    async fn bytes_returns_a_network_cover_bytes_with_its_mime() {
        let cache = CoverCache::new();
        let image = jpeg(10);
        cache.insert("k".into(), CoverPayload::Bytes(image.clone(), "image/png")).await;
        assert_eq!(cache.bytes("k", cap()).await, Some(("image/png", image)));
        assert_eq!(cache.bytes("unknown", cap()).await, None);
    }

    #[tokio::test]
    async fn bytes_reads_a_local_file_the_route_would_have_streamed() {
        // The difference with `cover_get`: here the bytes are materialized,
        // because pushing onto a socket offers no other choice.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let image = jpeg(1000);
        std::fs::write(&path, &image).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path)).await;
        assert_eq!(cache.bytes("k", cap()).await, Some(("image/jpeg", image)));
    }

    #[tokio::test]
    async fn bytes_revalidates_the_header_on_the_bytes_it_returns() {
        // `fetch` validated the header at discovery time, but between the two
        // the share is not under the device's control. Like the HTTP route,
        // this read therefore does not trust the discovery — and it goes
        // further: the checked content **is** the returned content, a single
        // read on a single descriptor, so there is no window at all between
        // the check and the use.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, jpeg(10)).unwrap();
        let r = CoverRef::Path { path: path.to_string_lossy().into_owned() };
        let Some(p) = fetch(&r).await else { panic!("a local image must be accepted") };
        let cache = CoverCache::new();
        cache.insert("k".into(), p).await;

        // Someone replaces the share's content after the discovery.
        std::fs::write(&path, b"this is no longer an image").unwrap();
        assert_eq!(
            cache.bytes("k", cap()).await,
            None,
            "the returned bytes must be the ones that were validated, never assumed content"
        );
    }

    #[tokio::test]
    async fn bytes_refuses_a_local_file_over_the_cap_and_accepts_exactly_the_cap() {
        // The transport cap, exercised on its exact bound. Local files have by
        // design **no** size limit (see `fetch`): so it is here, and nowhere
        // else, that the bound exists. A refusal, not an allocation of the
        // file's size — the read stops at `COVER_MAX_BYTES + 1` bytes,
        // whatever the actual size.
        let cap = cap();
        let dir = tempfile::tempdir().unwrap();

        let exact = dir.path().join("exact.jpg");
        std::fs::write(&exact, jpeg(cap - 6)).unwrap();
        let cache = CoverCache::new();
        cache.insert("exact".into(), CoverPayload::File(exact)).await;
        match cache.bytes("exact", cap).await {
            Some((mime, o)) => {
                assert_eq!(mime, "image/jpeg");
                assert_eq!(o.len(), cap, "exactly the cap must pass, not be refused");
            }
            None => panic!("an image of exactly COVER_MAX_BYTES must pass"),
        }

        let over = dir.path().join("over.jpg");
        std::fs::write(&over, jpeg(cap - 5)).unwrap();
        cache.insert("over".into(), CoverPayload::File(over)).await;
        assert_eq!(
            cache.bytes("over", cap).await,
            None,
            "a single byte over the cap must be enough to refuse"
        );
    }

    #[tokio::test]
    async fn bytes_returns_none_on_a_vanished_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(dir.path().join("absent.jpg"))).await;
        assert_eq!(cache.bytes("k", cap()).await, None);
    }

    /// Proves that the refusal comes from `metadata`, called **before** any
    /// read of the bytes — not from the read bounded at
    /// `COVER_MAX_BYTES + 1` bytes that remains as a net further down in
    /// `read_file_bounded`. A test that merely checked the `None` would not
    /// distinguish the two: the bounded read refuses just as well. The proof
    /// lies in the log: it must name the **actual** size of the file, far
    /// beyond `COVER_MAX_BYTES + 1` — a number the bounded read could never
    /// return, since it never reads more than that bound.
    #[tokio::test]
    async fn the_cap_is_checked_on_the_file_size_before_any_read() {
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Buffer;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("too-big.png");
        // Well beyond COVER_MAX_BYTES + 1: a read bounded at that limit could
        // never log such a number. Sparse file (`set_len`): no actual write
        // of the bytes, `metadata` alone must suffice.
        let real_size = ritornello_proto::COVER_MAX_BYTES as u64 + 50_000_000;
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(real_size).unwrap();
        drop(file);

        let buffer = Buffer::default();
        // `#[tokio::test]` is single-threaded by default: the per-thread
        // subscriber set here therefore remains valid across the `.await`
        // that follows.
        let subscriber = tracing_subscriber::fmt().with_writer(buffer.clone()).with_ansi(false).finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let result = read_file_bounded(&path, cap()).await;
        drop(guard);

        assert!(result.is_none(), "a file well beyond the cap must be refused");
        let log_output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(
            log_output.contains(&real_size.to_string()),
            "the log must name the file's actual size, known through metadata before any read: {log_output}"
        );
    }

    // -- `line`: the cover frame, re-read at every call --------------------

    #[tokio::test]
    async fn line_rereads_the_file_so_an_image_replaced_under_the_same_path_is_served_fresh() {
        // **The most serious defect of the review pass, at the cache's
        // granularity.** The key hashes the path, not the content: nothing in
        // the cache can see that a `folder.jpg` was replaced on the share. As
        // long as `line` re-reads the file at every call, that is not a
        // problem; an encoded line kept in memory, for its part, served the
        // previous image forever.
        //
        // The real scenario takes three clicks: disable the display from the
        // admin page, replace the image, re-enable it. The reconnected relay
        // asks for the current cover again — hence `line` with the **same
        // key** — and nobody has inserted anything in between. That is why
        // this test does not call `insert` a second time: an invalidation
        // placed in `insert` would not cover that path.
        //
        // Two **decodable** images, and small ones: under 640 px and under
        // the output cap, the default rendition lets them through as they are
        // (see `rendition`, step 3). The served bytes are therefore those of
        // the file, which keeps this test the sharpest possible assertion
        // about freshness — and documents the pass-through along the way.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(48, 48)).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path.clone())).await;

        let before = cache.line("k", "/api/cover/k").await.expect("a local image must produce a line");
        // The user replaces the cover on the share. Different dimensions, so
        // that the inequality does not hinge on the padding alone.
        std::fs::write(&path, fixtures::jpeg_decodable(64, 64)).unwrap();
        let after = cache.line("k", "/api/cover/k").await.expect("the second call must succeed too");

        assert_ne!(
            &*before, &*after,
            "after replacing the file under the same key, the served line must be the new one"
        );
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&after).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(
                    c.bytes,
                    fixtures::jpeg_decodable(64, 64),
                    "the served bytes must be those of the current file"
                );
            }
            other => panic!("a cover frame was expected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn line_changes_when_the_key_changes_and_stays_a_valid_cover_frame() {
        // The counterpart of the test above: two distinct keys designate two
        // distinct images, and each must return its own.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        std::fs::write(&a, fixtures::jpeg_decodable(48, 48)).unwrap();
        std::fs::write(&b, fixtures::jpeg_decodable(64, 64)).unwrap();
        let cache = CoverCache::new();
        cache.insert("a".into(), CoverPayload::File(a)).await;
        cache.insert("b".into(), CoverPayload::File(b)).await;

        let line_a = cache.line("a", "/api/cover/a").await.unwrap();
        let line_b = cache.line("b", "/api/cover/b").await.unwrap();
        assert_ne!(&*line_a, &*line_b, "two different keys must produce different lines");

        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&line_a).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(c.href, "/api/cover/a");
                assert_eq!(c.mime, "image/jpeg");
            }
            other => panic!("a cover frame was expected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_route_returns_404_on_an_unknown_key() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app
            .oneshot(Request::get("/api/cover/nonexistent").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -- allowed_target: the SSRF safeguard, with no network at all ---------

    #[test]
    fn allowed_target_rejects_a_literal_ip_with_trailing_dot() {
        // The `!l.is_empty()` of `ritornello-proto`'s parser fails on the
        // trailing empty label, which makes this form *pass* over there.
        // `Url::domain()` relies on the host actually resolved by the
        // browser/reqwest, which normalizes it to an IPv4.
        assert!(!allowed_target("https://192.168.1.1./a.jpg"));
    }

    #[test]
    fn allowed_target_rejects_a_literal_ip_in_hexadecimal() {
        assert!(!allowed_target("https://0x7f.0.0.1/a.jpg"));
    }

    #[test]
    fn allowed_target_rejects_localhost_for_lack_of_a_dot() {
        assert!(!allowed_target("https://localhost/a.jpg"));
    }

    #[test]
    fn allowed_target_rejects_a_literal_ipv6_address() {
        assert!(!allowed_target("https://[::1]/a.jpg"));
    }

    #[test]
    fn allowed_target_accepts_a_real_https_hostname() {
        assert!(allowed_target("https://coverartarchive.org/x/front-500"));
    }

    // -- download: the three network safeguards, against a real server -----
    //
    // `download` and not `fetch`: `allowed_target` refuses precisely
    // `127.0.0.1`, so going through `fetch` would prevent these tests from
    // reaching the code they want to exercise.

    /// Serializes a body as `Transfer-Encoding: chunked`.
    fn chunked_body(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend_from_slice(format!("{:x}\r\n", c.len()).as_bytes());
            out.extend_from_slice(c);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    fn http_response(headers: &str, body: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("HTTP/1.1 200 OK\r\n{headers}\r\n").as_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Serves `response` to the first connection received on `127.0.0.1`, on
    /// a port chosen by the OS. `close` closes the connection right after
    /// writing it; otherwise the connection stays open, never sending
    /// anything more — which lets a test prove that a caller did not try to
    /// read beyond what was served.
    async fn serve(response: Vec<u8>, close: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored).await;
                let _ = socket.write_all(&response).await;
                if close {
                    let _ = socket.shutdown().await;
                } else {
                    // Never closes: if the caller reads the body regardless,
                    // it will stay blocked until the test's timeout.
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            }
        });
        format!("http://127.0.0.1:{port}/cover.jpg")
    }

    #[tokio::test]
    async fn the_network_cap_cuts_a_chunked_stream_before_the_end() {
        // No `Content-Length` (`chunked` response): nothing to rely on except
        // the size actually received, chunk after chunk.
        let mut first = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        first.resize(900_000, 0);
        let second = vec![0u8; 900_000];
        let third = vec![0u8; 900_000]; // ~2.7 MB total, beyond the 2 MB cap
        let body = chunked_body(&[first, second, third]);
        let response =
            http_response("Content-Type: image/jpeg\r\nTransfer-Encoding: chunked\r\n", body);
        let url = serve(response, true).await;
        assert!(
            download(&url).await.is_none(),
            "the cap must cut the stream chunk by chunk, without waiting for the end"
        );
    }

    #[tokio::test]
    async fn a_refused_content_type_never_reads_the_body() {
        // The server never sends the body it announces: if `download` read it
        // despite the refused content-type, this wait would stay blocked until
        // the timeout below.
        let response = http_response("Content-Type: text/html\r\nContent-Length: 1000000\r\n", Vec::new());
        let url = serve(response, false).await;
        match tokio::time::timeout(std::time::Duration::from_secs(2), download(&url)).await {
            Ok(None) => {}
            Ok(Some(p)) => panic!("content-type refused but a cover was produced: {p:?}"),
            Err(_) => panic!(
                "timeout: the body was read (or its wait begun) despite the refused content-type"
            ),
        }
    }

    /// Serves a redirect towards `target`, then answers nothing more: if the
    /// client followed the hop, it would try to reach `target` — which the
    /// test's assertion observes through the failure of the whole request.
    fn redirect_response(target: &str) -> Vec<u8> {
        format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n").into_bytes()
    }

    #[tokio::test]
    async fn a_redirect_to_a_literal_ip_is_refused() {
        // The SSRF safeguard only applied to the starting URL: the image
        // host — a third party, since OUI FM's `coverUrl` is written by
        // someone else — only had to answer a `302` towards a LAN address to
        // make the device issue a GET on it, scheme change included. One hop
        // of indirection cancelled the whole check.
        //
        // `192.0.2.1` (RFC 5737 documentation block) rather than the
        // development network's gateway: if the policy let it through, this
        // test must fail without having reached anything real.
        for target in [
            "http://192.0.2.1/a.jpg",
            "https://192.0.2.1/a.jpg",
            // Same host form as a legitimate target, but the scheme falls
            // back to cleartext: refused too. Domain in `.invalid`, which
            // never resolves — no test here touches the network.
            "http://coverartarchive.invalid/a.jpg",
        ] {
            let url = serve(redirect_response(target), true).await;
            // The timeout is not a pacing assumption but a failure detector,
            // as in the content-type test above: if the hop were followed,
            // the wait for an unreachable host would last until the client's
            // ten-second timeout and return `None` — hence a green test for
            // the wrong reason.
            match tokio::time::timeout(std::time::Duration::from_secs(2), download(&url)).await {
                Ok(None) => {}
                Ok(Some(p)) => panic!("redirect followed towards {target:?}: {p:?}"),
                Err(_) => panic!("the hop towards {target:?} was attempted: the policy must refuse it"),
            }
        }
    }

    #[tokio::test]
    async fn a_body_that_is_not_an_image_is_refused_despite_the_content_type() {
        let body = b"this is not an image".to_vec();
        let response = http_response(
            &format!("Content-Type: image/png\r\nContent-Length: {}\r\n", body.len()),
            body,
        );
        let url = serve(response, true).await;
        assert!(
            download(&url).await.is_none(),
            "the content declares `image/png` but the received bytes are not: must be refused"
        );
    }
    // -- The rendition: what the core builds before pushing -----------------

    /// A `Rendition` whose every field is named by the test using it: the
    /// product defaults (640 px, 512 KiB, 16 Mpx) would make most cases
    /// unreachable without fabricating huge images.
    fn test_rendition(max_edge_px: u32, output_cap: usize, pixel_cap: u64) -> Rendition {
        Rendition { max_edge_px, jpeg_quality: 85, output_cap, pixel_cap }
    }

    #[tokio::test]
    async fn eight_displays_asking_for_the_same_cover_build_it_only_once() {
        // The rendezvous. Two subscribed displays receive the **same** state
        // frame, so they ask for the same cover at the same instant, and used
        // to decode then re-encode the same image twice — several hundred
        // milliseconds of core in duplicate on a Pi 2.
        //
        // The proof is a **count of executions**, and there is no other:
        // comparing the returned frames would say nothing, two successive
        // builds of the same image producing identical bytes.
        let cache = Arc::new(CoverCache::new());
        // A rendition with real work to do: 600 × 400 exceeds the maximum
        // edge, so the image is decoded and re-encoded for real. Without
        // that, the pass-through would return the source as is and the first
        // arrival would finish without ever suspending — no follower would
        // have time to show up, and the test would pass without proving
        // anything.
        cache.set_cover_settings(CoverSettings {
            entries: 20,
            source_max: 8 * 1024 * 1024,
            rendition: Some(test_rendition(64, 512 * 1024, 16_000_000)),
        });
        cache
            .insert("k".into(), CoverPayload::Bytes(fixtures::jpeg_decodable(600, 400), "image/jpeg"))
            .await;

        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let c = cache.clone();
                tokio::spawn(async move { c.line("k", "/api/cover/k").await })
            })
            .collect();
        let mut frames = Vec::new();
        for t in tasks {
            frames.push(t.await.expect("no task must panic"));
        }

        assert!(frames.iter().all(|t| t.is_some()), "all eight must receive a frame");
        let first = frames[0].as_deref().unwrap();
        assert!(
            frames.iter().all(|t| t.as_deref() == Some(first)),
            "all eight must receive the same frame"
        );
        assert_eq!(
            cache.builds(),
            1,
            "a single build for eight concurrent requests of the same key"
        );
    }

    #[tokio::test]
    async fn the_rendezvous_keeps_nothing_once_the_frame_is_built() {
        // The rendezvous is **not** a cache, and that is what makes it
        // acceptable: the key hashes the *path*, not the content, so a kept
        // frame would become wrong as soon as the user replaces the image
        // under that path. A `OnceCell` keeping its value forever, everything
        // hinges on the removal of the entry.
        let cache = Arc::new(CoverCache::new());
        cache.set_cover_settings(CoverSettings {
            entries: 20,
            source_max: 8 * 1024 * 1024,
            rendition: Some(test_rendition(64, 512 * 1024, 16_000_000)),
        });
        cache
            .insert("k".into(), CoverPayload::Bytes(fixtures::jpeg_decodable(600, 400), "image/jpeg"))
            .await;

        assert!(cache.line("k", "/api/cover/k").await.is_some());
        assert!(
            cache.in_flight.lock().await.is_empty(),
            "the table of in-progress builds must be empty afterwards"
        );

        // And the second request **rebuilds**, instead of being served by a
        // cell left in place.
        assert!(cache.line("k", "/api/cover/k").await.is_some());
        assert_eq!(
            cache.builds(),
            2,
            "two requests separated in time must produce two builds"
        );
    }

    #[tokio::test]
    async fn an_already_small_image_leaves_as_is_without_reencoding() {
        // The pass-through. The **binary** identity is the assertion that
        // matters: a decode/encode round trip would produce different bytes
        // even at equal dimensions, so the equality proves that none took
        // place.
        let source = fixtures::jpeg_decodable(64, 64);
        let output = rendition("image/jpeg", source.clone(), test_rendition(640, 512 * 1024, 16_000_000))
            .await
            .expect("a small image must pass");
        assert_eq!(output, ("image/jpeg", source));
    }

    #[tokio::test]
    async fn an_oversized_image_is_shrunk_keeping_its_aspect_ratio() {
        // 300 × 150, reduced to an edge of 100: the 2:1 ratio must survive.
        // Verified by **decoding the output**, not by taking the code's word
        // for it.
        let source = fixtures::jpeg_decodable(300, 150);
        let (mime, output) = rendition("image/jpeg", source.clone(), test_rendition(100, 512 * 1024, 16_000_000))
            .await
            .expect("a large image must be shrunk, not refused");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(dimensions(&output), Some((100, 50)), "the 2:1 ratio must be kept");
        assert!(
            output.len() < source.len(),
            "a 100x50 thumbnail must weigh less than its 300x150 original: {} against {}",
            output.len(),
            source.len()
        );
    }

    #[tokio::test]
    async fn an_image_with_alpha_channel_is_reencoded_as_lossless_png() {
        // Flattening the transparency would require choosing a background
        // color, a visual stance the device has no business taking. The mime
        // changes, so the pushed frame declares it — a display receiving
        // `image/jpeg` with PNG bytes would show a broken square.
        let source = fixtures::png_alpha(300, 300);
        let (mime, output) = rendition("image/png", source, test_rendition(100, 512 * 1024, 16_000_000))
            .await
            .expect("an alpha png must be rendered");
        assert_eq!(mime, "image/png", "the mime must follow the format actually produced");
        assert_eq!(dimensions(&output), Some((100, 100)));
    }

    /// **The bomb guard, and its order.**
    ///
    /// The image of this test would clear the pass-through without
    /// difficulty: 100 px per edge under the 640 allowed, two kilobytes under
    /// the output cap. Only the pixel guard refuses it. The test therefore
    /// fails if the guard disappears **and** if it is moved after the
    /// pass-through — it is this second case that matters, because a bomb is
    /// precisely an image tiny in bytes and outsized in pixels.
    #[tokio::test]
    async fn the_pixel_cap_refuses_before_any_decoding_and_before_the_pass_through() {
        let source = fixtures::jpeg_decodable(100, 100);
        assert!(
            source.len() < 512 * 1024,
            "the fixture must fit under the output cap, otherwise the test does not prove the order"
        );
        assert_eq!(
            rendition("image/jpeg", source, test_rendition(640, 512 * 1024, 1_000)).await,
            None,
            "10000 pixels beyond a cap of 1000 must be refused"
        );
    }

    #[tokio::test]
    async fn a_thumbnail_over_the_output_net_is_not_pushed() {
        // The safety net, exercised on a deliberately tiny cap: a 200 × 200
        // thumbnail of a gradient does not fit in 200 bytes.
        let source = fixtures::jpeg_decodable(400, 400);
        assert_eq!(
            rendition("image/jpeg", source, test_rendition(200, 200, 16_000_000)).await,
            None,
            "a thumbnail beyond the net must not be pushed"
        );
    }

    #[tokio::test]
    async fn the_switch_unchecked_pushes_the_source_without_decoding_it() {
        // Two properties in one, and the fixture is the trick: these bytes
        // have a valid JPEG header but **undecodable** content. If they come
        // out of `line` intact, it means the decoder was not called at all —
        // not merely that its result was ignored.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let source = jpeg(1000);
        std::fs::write(&path, &source).unwrap();
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { entries: 20, source_max: cap(), rendition: None });
        cache.insert("k".into(), CoverPayload::File(path)).await;

        let line = cache.line("k", "/api/cover/k").await.expect("the source must leave as it is");
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&line).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(c.bytes, source, "the pushed bytes must be those of the source");
                assert_eq!(c.mime, "image/jpeg");
            }
            other => panic!("a cover frame was expected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_switch_checked_discards_an_image_whose_bytes_do_not_decode() {
        // The counterpart of the test above, and a deliberate **behavior
        // change**: `image_type` only reads the magic bytes, so a truncated
        // file passed that validation and left for the displays, each of
        // which showed a broken square in its own way. The rendition settles
        // it once for all, at the center.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, jpeg(1000)).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path)).await;
        assert!(
            cache.line("k", "/api/cover/k").await.is_none(),
            "a file whose header lies about its content must not be pushed"
        );
    }

    #[tokio::test]
    async fn the_product_settings_reencode_a_large_cover() {
        // The complete production path, with the defaults and without
        // parameterizing them: a 1000 × 1000 cover must arrive at 640 px.
        // Without this test, all the others could pass with settings no
        // device applies.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let source = fixtures::jpeg_decodable(1000, 1000);
        std::fs::write(&path, &source).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path)).await;

        let line = cache.line("k", "/api/cover/k").await.expect("a cover must be pushed");
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&line).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(
                    dimensions(&c.bytes),
                    Some((640, 640)),
                    "the default settings must bring the long edge down to 640 px"
                );
                assert!(
                    c.bytes.len() < source.len(),
                    "the thumbnail must weigh less than the source: {} against {}",
                    c.bytes.len(),
                    source.len()
                );
            }
            other => panic!("a cover frame was expected: {other:?}"),
        }
    }

    #[test]
    fn settings_translate_the_switch_into_an_absent_rendition() {
        // The `Settings -> CoverSettings` conversion, which is the only place
        // where the switch becomes a structure. `None` rather than a boolean
        // carried alongside: it is what makes it impossible to read
        // `max_edge_px` without having first checked that the rendition is
        // enabled.
        let mut s = crate::state::Settings::default();
        assert!(CoverSettings::from(&s).rendition.is_some(), "the product default re-encodes");

        s.cover_rendition = false;
        assert!(CoverSettings::from(&s).rendition.is_none());
        assert_eq!(
            CoverSettings::from(&s).source_max,
            20 * 1024 * 1024,
            "the source cap survives the switch: that is its reason for being"
        );
    }
}
