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

/// Prefix of the local URL published in `Track::cover_href`.
///
/// Shared between `metadata::Metadata::state`, which **builds** it, and
/// `main::display_relay`, which **re-reads** it to recover the cache key: two
/// literals could have drifted apart silently, and the consequence would have
/// been a display that never receives a cover again, with no error anywhere.
pub const HREF_PREFIX: &str = "/api/cover/";

/// A cover frame under construction, shared between the caller building it
/// and those waiting for it.
///
/// The outer `Option` is the one of `line` — "nothing to push", for the same
/// reasons as everywhere in this module; the inner `Arc<str>` is the already
/// serialized line of text. The cell sits behind an `Arc` so the waiters can
/// hold it after releasing the table's lock.
type FrameInFlight = Arc<tokio::sync::OnceCell<Option<Arc<str>>>>;

/// A rendition under construction, shared between the caller producing it and
/// those waiting for it. The same rendezvous as `FrameInFlight`, one stage
/// lower: what is shared here is the re-encoded image, not the protocol line
/// wrapped around it.
///
/// **The `SourceStamp` travels with the bytes, and it is not decoration.** A
/// waiter registers at this rendezvous *after* it has stat'ed the file, but
/// the cell it joins was filled by a read that started *before* that stat —
/// so the picture it collects can describe the file as it was **older** than
/// its own stamp. Labelling those bytes with the waiter's newer validator
/// freezes the wrong image in that browser: the response is `no-cache`, the
/// browser revalidates with `If-None-Match`, the stat still yields the same
/// newer stamp, and the `304` keeps handing back the stale bytes forever.
/// Carrying the stamp of the read that actually produced the bytes lets
/// `cover_get` label its `200` with it; the next revalidation then mismatches
/// and the correct image is served. That is the direction `rendition_for`'s
/// own comment declares harmless — bytes *fresher* than their label — and it
/// only holds because the label is now the read's, not the stat's.
type RenditionInFlight =
    Arc<tokio::sync::OnceCell<Option<(&'static str, Arc<Vec<u8>>, SourceStamp)>>>;

/// A full-size embedded extraction under way, shared between the caller
/// running it and those waiting for it. The third instance of the same
/// rendezvous, one stage lower than `RenditionInFlight`: what is shared here
/// is the raw picture pulled out of the audio container, before any
/// re-encoding — see `CoverCache::embedded_in_flight` for why this one exists
/// at all.
///
/// **`axum::body::Bytes`, not `Arc<Vec<u8>>` like `RenditionInFlight`.** A
/// rendition's bytes are always consumed by cloning them out into an owned
/// `Vec` — `line` must, to embed them in a `ritornello_proto::Cover` — so
/// `Arc<Vec<u8>>` merely gets the *waiting* right; the eventual clone-out
/// still costs one full copy per waiter. `cover_get`'s bare-URL branch has no
/// such requirement: `Bytes` is what `axum::body::Body` is built from
/// directly, refcounted like an `Arc` internally, so N waiters resolving
/// together clone a *handle*, not the picture — one allocation serves every
/// response.
///
/// **The `SourceStamp` travels here too**, for the reason `RenditionInFlight`
/// states above and which bites hardest on this path: `cover_get` stats the
/// audio file, derives its `ETag` from that stat, and only then joins this
/// rendezvous — where it may collect a picture some earlier caller pulled out
/// of the container *before* the file was retagged. `read_embedded_bounded`
/// already returns the stamp of the read it performed and this cell used to
/// discard it; relaying it is what lets the response be labelled with the
/// picture's own stamp.
type EmbeddedInFlight =
    Arc<tokio::sync::OnceCell<Option<(&'static str, axum::body::Bytes, SourceStamp)>>>;

/// A full-size download under way, shared between the caller performing it
/// and those waiting for it. The fourth instance of this module's rendezvous,
/// and the one with the most to lose: `EmbeddedInFlight` spares a parse of a
/// container on the owner's own disk, this one spares a request to a third
/// party's server.
///
/// **`axum::body::Bytes` for the same reason as `EmbeddedInFlight`**: it is
/// what a response body is built from directly, so N browsers enlarging the
/// same cover together share one allocation all the way to their sockets
/// instead of each cloning out its own copy of a 2.5 MiB picture — which is
/// half of what this rendezvous exists to avoid, the other half being the N
/// requests themselves.
///
/// **No `SourceStamp` travels here**, unlike the two rendezvous above, and
/// its absence is not an oversight: both halves of a `CoverPayload::Pair`
/// come from a network body checked in full, frozen under a key that hashes
/// the URL it came from. There is no stat anywhere on this path, hence no
/// skew between a caller's stamp and the read's to express — every caller's
/// validator is the key itself.
type FullInFlight = Arc<tokio::sync::OnceCell<Option<(&'static str, axum::body::Bytes)>>>;

/// A shared buffer in the shape `axum::body::Bytes::from_owner` asks for.
///
/// `Arc<Vec<u8>>` implements `AsRef<Vec<u8>>` and not `AsRef<[u8]>`, so it
/// cannot be handed over directly; this wrapper is the whole of the
/// adaptation, and it is what keeps a memoised full size from being copied
/// on its way into a response body — see `CoverPayload::Pair`'s `fetched`.
struct SharedBody(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBody {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A response body over `bytes`, sharing its allocation rather than copying
/// it. See `SharedBody`.
fn shared_body(bytes: Arc<Vec<u8>>) -> axum::body::Bytes {
    axum::body::Bytes::from_owner(SharedBody(bytes))
}

/// What proves a source has not changed under its key.
///
/// **Owned by this module, and that is a correction.** The HTTP route used to
/// build a quoted `ETag` string and inject it into the cache's identity, so a
/// protocol's validator leaked down into the core's memory. The dependency now
/// runs the other way: the core stamps its sources, and `cover_get` *derives*
/// its header from the stamp.
///
/// `Frozen` is not a degenerate case. A network cover is an HTTP body read
/// whole, held in memory under a key derived from its URL: nothing can change
/// it in place, so there is nothing to stamp. Only a file on a share can be
/// replaced under its path without any code of ours running.
///
/// **What the file stamp is worth.** Modification date and size, the pair HTTP
/// has validated caches with for thirty years, and the pair `cover_get`
/// already trusted for its `304`. Two writes landing inside one clock tick
/// *and* producing the same byte count would collide — on Windows the system
/// clock advances about every 15 ms — where re-reading every time could not.
/// That is a real weakening, and a bounded one: the defect the `line` doc
/// describes was a cache that nothing could *ever* invalidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceStamp {
    /// A body already read whole, immutable under its key.
    Frozen,
    /// A file on a share, which anyone can replace behind our back.
    File { modified_nanos: u128, size: u64 },
}

impl SourceStamp {
    /// The stamp of a file, read from the metadata of the descriptor that is
    /// about to be — or has just been — read. Same descriptor, so no window
    /// between the stamp and the bytes it describes — for a caller that reads
    /// through one open. `read_embedded_bounded` does not: it takes a
    /// separate `std::fs::metadata` and then a separate `Probe::open`, so a
    /// replacement landing in that gap is possible there. The consequence is
    /// benign rather than a correctness hole: a picture written into that
    /// window is filed under a stamp that describes the file *before* the
    /// replacement, an identity no later stat of the replaced file will ever
    /// produce again — so the entry is orphaned in the cache, never wrongly
    /// served to a caller validating against the file as it now is.
    fn of_file(meta: &std::fs::Metadata) -> Self {
        let modified_nanos = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self::File { modified_nanos, size: meta.len() }
    }

    /// What this stamp contributes to a cache identity.
    fn tag(&self) -> String {
        match self {
            Self::Frozen => "frozen".to_string(),
            Self::File { modified_nanos, size } => format!("{modified_nanos:x}-{size:x}"),
        }
    }
}

/// What the core keeps of a cover.
///
/// Bytes or a path, and it is deliberate: a **local** cover does not enter
/// memory. A three-megabyte `folder.jpg` is commonplace on a NAS, and loading
/// it into RAM on a Pi for an image the browser will cache on its side would
/// be a waste.
///
/// `Pair` is the one variant that is both at once — bytes for the thumbnail,
/// a reference for the full size — and that is exactly what it exists to
/// express.
#[derive(Debug, Clone)]
pub enum CoverPayload {
    /// From the network: the bytes are in memory.
    Bytes(Vec<u8>, &'static str),
    /// Local: only the path is kept, the route re-reads the file.
    File(PathBuf),
    /// Embedded in the audio file: only the audio file's path is kept, and
    /// the picture is extracted again on demand. Symmetrical with `File` — a
    /// cover that lives on a share costs a path, whether it sits beside the
    /// track or inside it.
    ///
    /// Deposited by `player::mpv::embedded_cover` through
    /// `Core::extraction_arrived` and `cover::fetch`, exactly as `File` is
    /// deposited through `Core::set_source_cover` and `fetch` — a probe, never
    /// a copy: nothing is written to disk to produce this variant.
    Embedded(PathBuf),
    /// A cover whose contributor supplied a ready-made thumbnail alongside it.
    ///
    /// The thumbnail's bytes are held — they are what the player's square
    /// shows — and the full-size image is kept as a **reference only**, not
    /// downloaded, until someone enlarges the cover. Most covers never are.
    ///
    /// Why the reference lives in the payload and not in a table beside it:
    /// the invariant then has nowhere to break. A side table can outlive the
    /// eviction of its entry and hand back an orphan reference; a variant
    /// carries both halves or neither.
    ///
    /// `fetched` is that full-size image **once somebody has enlarged it**,
    /// with its MIME type: the memo `CoverCache::remember_full` writes back
    /// after a download, so that a second reader of the same cover costs
    /// nothing. It lives in the variant for the reason the reference does,
    /// and the reason is worth more here: being held in memory it is charged
    /// to the budget (`payload_cost`), and being part of its entry it is
    /// **evicted with it** — no separate lifetime to manage, and no fourth
    /// concern in `evict_to_budget`, which the byte budget already bounds.
    ///
    /// **Shared behind an `Arc`, and that is not a detail of taste.**
    /// `CoverCache::read` clones the whole payload on **every** request for
    /// this key — the thumbnail of the player's square included, and a `304`
    /// included, both of which reach that clone before they ever look at the
    /// size they were asked for. With owned bytes in here, one enlargement
    /// would make every later request on that key copy up to `source_max`
    /// (20 MiB by default) on a route nobody has to authenticate against.
    /// An `Arc` makes that clone a refcount bump, exactly as the retained
    /// renditions are shared (`Rendered::bytes`), and
    /// `axum::body::Bytes::from_owner` then builds a response body over the
    /// very same allocation rather than a copy of it.
    Pair {
        thumb: Vec<u8>,
        thumb_mime: &'static str,
        full: CoverRef,
        fetched: Option<(Arc<Vec<u8>>, &'static str)>,
    },
}

/// What `p` charges against `CoverSettings::budget`.
///
/// Only bytes actually held cost anything, and that mirrors `CoverPayload`'s
/// own doc: `File` and `Embedded` both keep a path and nothing else, the same
/// path a `folder.jpg` on a NAS would cost whether it sat beside the track
/// or inside it. `evict_to_budget` charges a retained `Rendered` the same
/// way, directly against its `bytes.len()` — there is no payload to match
/// on there, only ever bytes.
///
/// **A `Pair` is charged its thumbnail, plus its full size once that has
/// actually been downloaded.** For as long as nobody enlarges the cover, the
/// other half is a `CoverRef` nobody has fetched — a string, on the order of
/// the path a `File` costs — and charging the full-size image then would bill
/// the budget for megabytes the process does not hold. After an enlargement
/// the opposite is true: `fetched` holds those megabytes, and leaving them
/// uncharged would stop the budget from describing what the appliance
/// actually holds, which is the one thing it exists to do.
fn payload_cost(p: &CoverPayload) -> usize {
    match p {
        CoverPayload::Bytes(v, _) => v.len(),
        CoverPayload::Pair { thumb, fetched, .. } => {
            thumb.len() + fetched.as_ref().map_or(0, |(b, _)| b.len())
        }
        CoverPayload::File(_) | CoverPayload::Embedded(_) => 0,
    }
}

/// A cover the core has found, whichever door it came through, before any
/// fetch is attempted.
///
/// **`Ref` wraps the wire type verbatim, `Embedded` does not exist on the
/// wire at all.** A Source or a `metadata` plugin can only ever announce a
/// `ritornello_proto::CoverRef` — the protocol has exactly two variants,
/// `Url` and `Path`, and this task changes neither. `Embedded` is what the
/// core itself produces when it probes the played file's own tags
/// (`player::mpv::embedded_cover`): there is nothing to name on the wire for
/// it, since no Source declares it and no protocol message carries it. This
/// type is therefore internal to the core, one layer above `CoverRef`, not a
/// third protocol variant in disguise.
///
/// `content` on `Embedded` is deliberate, not `audio` alone: see `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverSource {
    /// Declared by a Source or a `metadata` plugin, the wire form unchanged.
    Ref(CoverRef),
    /// Found embedded in the currently playing audio file, by
    /// `player::mpv::embedded_cover`. `audio` is what a later read reopens to
    /// extract the picture again (`read_embedded_bounded`); `content` is the
    /// fingerprint of the picture's own bytes, computed once at probe time so
    /// that `key`, below, need not reopen the file to deduplicate.
    Embedded { audio: PathBuf, content: String },
}

/// Fingerprint of the source, published in the local URL.
///
/// `DefaultHasher` and not `sha2`: a collision would display the wrong cover
/// and nothing else, which does not justify a cryptographic dependency.
/// Computable **before** the download, which makes it possible to deduplicate
/// two requests for the same image.
///
/// **`Embedded` hashes `content`, never `audio`.** This is what makes the
/// deduplication of an album survive the move away from a temp file: two
/// tracks of the same album carry two different `audio` paths but the very
/// same picture bytes, hence the same `content`, hence the same key, hence a
/// single cache entry and a single `href` — exactly the property a
/// path-keyed hash would have destroyed, one entry (and one fetch, one
/// decode) per track instead of per album.
pub fn key(s: &CoverSource) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match s {
        CoverSource::Ref(CoverRef::Url { url }) => {
            0u8.hash(&mut h);
            url.hash(&mut h);
        }
        CoverSource::Ref(CoverRef::Path { path }) => {
            1u8.hash(&mut h);
            path.hash(&mut h);
        }
        CoverSource::Embedded { content, .. } => {
            2u8.hash(&mut h);
            content.hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

/// Fingerprint of an image's **content**, carried as `CoverSource::Embedded`'s
/// `content` field so that `key` can deduplicate an album without reopening
/// the file.
///
/// Same hasher as `key`, and the same trade-off: a collision would display the
/// wrong cover and nothing else. What changes is what gets hashed — the bytes
/// of the image, not the path they come from. Two tracks of the same album
/// carrying the same embedded cover thus land on a single `CoverSource`, hence
/// a single cache key, hence a single `href`, hence nothing to fetch again nor
/// to decode again: the embedded case thereby joins the local `folder.jpg`,
/// which was already free. Without that, a fifteen-track album made a cache
/// bounded by memory (`CoverSettings::budget`) churn for nothing, extraction
/// and eviction included.
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
    /// Weight under which the original is pushed untouched, in bytes.
    ///
    /// Compared to the **incoming** image only. What the encoder produces is
    /// judged by `net` instead, and the two must never be the same number
    /// again: as one number it was raised to avoid dropping covers, which let
    /// heavy originals through, and lowered to tighten thumbnails, which made
    /// drops likelier.
    pub passthrough_max: usize,
    /// Cap on the pixels to decode. Compared against the dimensions read from
    /// the header **before any allocation**, and carried into `image::Limits`
    /// for the case of a header that would lie about its own dimensions.
    pub pixel_cap: u64,
}

impl Rendition {
    /// What these rules contribute to a cache identity.
    ///
    /// **This is what makes a settings change take effect**, and its absence
    /// was a defect: nothing invalidated the memorized renditions, so lowering
    /// `max_edge_px` in the admin page kept serving the old size until the
    /// entry was evicted. Carrying the rules in the identity settles it
    /// without a line of invalidation logic — other rules, other identity, and
    /// the stale entry falls out on its own.
    fn tag(&self) -> String {
        let Self { max_edge_px, jpeg_quality, passthrough_max, pixel_cap } = self;
        format!("{max_edge_px}-{jpeg_quality}-{passthrough_max}-{pixel_cap}")
    }

    /// **The rule: is this image already small enough to be left alone?**
    /// Within the longest edge *and* under the pass-through threshold.
    ///
    /// One method and not two spellings of one predicate, and that is the
    /// whole reason it exists. It has two callers with nothing else in
    /// common — `rendition`, deciding whether to run the encoder at all, and
    /// `cover_get`'s `Pair` arm, deciding whether a **supplied** thumbnail is
    /// acceptable as it stands. Written out on both sides, the day someone
    /// turned a `<=` into a `<`, or measured the long edge differently, a
    /// contributor's thumbnail would be judged by a rule no other cover is
    /// judged by. The convergence of those two questions onto one answer is
    /// the finding this worksite rests on; a shared method is what makes it
    /// true rather than merely intended.
    ///
    /// `len` and the dimensions are passed in rather than read from the
    /// bytes: `rendition` has already decoded the header for its pixel cap,
    /// and asking for them again would be a second read of the same header.
    pub fn leaves_alone(&self, len: usize, (width, height): (u32, u32)) -> bool {
        width.max(height) <= self.max_edge_px && len <= self.passthrough_max
    }

    /// The safety net on what the encoder **produced**, in bytes.
    ///
    /// Derived, not configured: **two** bytes per pixel of the thumbnail,
    /// floored at 256 KiB.
    ///
    /// **Two and not one, and the difference between the two figures is the
    /// difference between a median and a maximum.** The density this comment
    /// used to quote — 0.30 byte per pixel at 640 px, q90 — is the bench's
    /// *median* over a real library (78 covers, of which 41 were large enough
    /// to be re-encoded at that edge). Its **maximum** on that same sample is
    /// 246 KiB, i.e. 0.61 byte per pixel: one byte per pixel left a factor of
    /// 1.6 over the heaviest cover measured, not the factor of three claimed
    /// here. And q90 is not the ceiling either — `jpeg_quality` is
    /// validated up to 100, where a JPEG commonly weighs two to three times
    /// its q90 weight. One byte per pixel was therefore reachable by raising
    /// a setting the admin page offers, and reaching it drops the cover: a
    /// regression against the adjustable ceiling this net replaced. Two bytes
    /// per pixel put the heaviest cover measured at q90 at a factor of about
    /// 3.3, which is the headroom the old comment believed it had.
    ///
    /// It still exists only to stop the absurd, and it is still not a
    /// setting.
    ///
    /// **Deliberately computed from the edge alone**, and not from the page's
    /// weight model: that model lives in `web/app` because it exists to
    /// explain, and a net depending on it would force a second copy here, with
    /// two versions to drift apart.
    pub fn net(&self) -> usize {
        let square = (self.max_edge_px as usize).saturating_mul(self.max_edge_px as usize);
        square.saturating_mul(2).max(256 * 1024)
    }
}

/// The identity under which a rendition is memorized: the source's key, what
/// proves that source has not changed, and the rules it was produced under.
/// All three are needed — drop any one and the cache can serve bytes that
/// answer a question nobody asked.
fn rendition_identity(key: &str, stamp: &SourceStamp, rules: &Rendition) -> String {
    format!("{key}:{}:{}", stamp.tag(), rules.tag())
}

/// Whether a retained rendition's identity (see `rendition_identity`) was
/// produced under the rules the cache is configured with **right now**.
///
/// Nothing re-renders a retained thumbnail when the settings change — see
/// `set_cover_settings` — so an identity's rules tag drifting from the live
/// one is exactly what marks it as pure waste for `evict_to_budget`'s first
/// step: it answers a question today's settings no longer ask, and nobody
/// will ever look it up again under this identity. A disabled rendition
/// (`current` at `None`) matches nothing at all: no rule is producing
/// anything right now, so every retained rendition is waste, not merely
/// stale.
///
/// Splitting on the **last** `:` is safe because none of the three parts of
/// an identity ever contains one: the key and `SourceStamp::tag` are both
/// hexadecimal, and `Rendition::tag` is digits and `-`.
fn rendition_is_current(identity: &str, current: Option<Rendition>) -> bool {
    match (identity.rsplit_once(':'), current) {
        (Some((_, tag)), Some(rules)) => tag == rules.tag(),
        _ => false,
    }
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
    /// Memory budget for the cache, in bytes. See
    /// `state::Settings::cover_cache_budget_mio`.
    ///
    /// Enforced by `CoverCache::evict_to_budget`, called after every
    /// `insert` and `remember_rendition`, against the combined cost of
    /// `entries` and `renditions` (see `payload_cost`).
    pub budget: usize,
    /// Cap on a cover **downloaded from the internet**, in bytes. See
    /// `state::Settings::cover_download_max_mio`. Passed to `download`
    /// rather than read from a constant — the only way to make the cut
    /// testable at more than one value.
    pub download_max: usize,
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
            budget: (s.cover_cache_budget_mio as usize) * 1024 * 1024,
            download_max: (s.cover_download_max_mio as usize) * 1024 * 1024,
            source_max: (s.cover_source_max_mio as usize) * 1024 * 1024,
            rendition: s.cover_rendition.then(|| Rendition {
                max_edge_px: s.cover_max_edge_px,
                jpeg_quality: s.cover_jpeg_quality,
                passthrough_max: (s.cover_passthrough_max_ko as usize) * 1024,
                pixel_cap: (s.cover_max_pixels_mpx as u64) * 1_000_000,
            }),
        }
    }
}

/// What the cache holds **right now**, for the configuration page's detail
/// panel.
///
/// The counterpart of the estimate shown on that page, and not a replacement
/// for it: the estimate answers "what would this setting do", this answers
/// "what is happening". Keeping them apart is deliberate — an estimate that
/// moved because the cache warmed up would blur the very effect the user is
/// trying to see.
///
/// Bytes and counts only, no keys and no paths: the panel is a diagnostic, not
/// a listing, and the keys name what someone is listening to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CacheSnapshot {
    /// Bytes charged against the budget: costly sources plus every retained
    /// rendition. The same arithmetic `evict_to_budget` uses.
    pub used_bytes: usize,
    /// The budget those bytes are measured against.
    pub budget_bytes: usize,
    /// Entries held, whatever they cost.
    pub entries: usize,
    /// Of those, how many cost nothing but a path — a local file or a picture
    /// embedded in the audio file (see `payload_cost`). This is the line that
    /// makes `MAX_ENTRIES` legible: it exists because these cost zero.
    pub entries_free: usize,
    /// Retained thumbnails, and what they weigh together. The page divides one
    /// by the other to show the real average, which is the ground truth behind
    /// the weight it predicts.
    pub renditions: usize,
    pub renditions_bytes: usize,
    /// Of those, how many were produced under rules the cache no longer uses
    /// (see `rendition_is_current`). They are pure waste, and the first thing
    /// eviction reclaims.
    pub renditions_stale: usize,
    /// The belt on the entry count, `MAX_ENTRIES`. Shown **here** and nowhere
    /// else: this is the one place it can be presented as what it is, a bound
    /// on a count, without being mistaken for a memory bound.
    pub max_entries: usize,
}

impl CoverCache {
    /// Reads both tables once and counts. No allocation, no clone of a
    /// payload: a walk of a few hundred entries at worst.
    pub async fn snapshot(&self) -> CacheSnapshot {
        let settings = self.settings();
        let entries = self.entries.read().await;
        let renditions = self.renditions.read().await;
        let costly: usize = entries.iter().map(|(_, p)| payload_cost(p)).sum();
        let renditions_bytes: usize = renditions.iter().map(|r| r.bytes.len()).sum();
        CacheSnapshot {
            used_bytes: costly + renditions_bytes,
            budget_bytes: settings.budget,
            entries: entries.len(),
            entries_free: entries.iter().filter(|(_, p)| payload_cost(p) == 0).count(),
            renditions: renditions.len(),
            renditions_bytes,
            renditions_stale: renditions
                .iter()
                .filter(|r| !rendition_is_current(&r.identity, settings.rendition))
                .count(),
            max_entries: MAX_ENTRIES,
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
    /// The renditions already produced, **shared by both consumers**.
    ///
    /// A cache, this time, and not a rendezvous like `in_flight` — the
    /// difference lies entirely in what serves as the key. `line` could not
    /// memorize anything because its key hashes the *path*: the user replaces
    /// the `folder.jpg` under that path and nothing invalidates the entry.
    /// Here the identity additionally carries the source's stamp (see
    /// `SourceStamp`) — replacing the file therefore changes the identity, and
    /// the old rendition is never served again — and the rules it was produced
    /// under (see `Rendition::tag`), so a settings change lands too. It then
    /// evicts itself on its own, like the rest.
    ///
    /// **Both consumers, and that is new.** The page's 224 px square and the
    /// socket of a subscribed display want the very same bytes, and each used
    /// to decode and re-encode the image on its own side: on a Pi 2, a second
    /// core busy for several hundred milliseconds per published cover. What
    /// once forbade sharing was the identity, not the desire — the display
    /// path had no way to name what it had built. Now it has.
    renditions: RwLock<VecDeque<Rendered>>,
    /// The renditions in progress, one entry per cache key.
    ///
    /// `in_flight` one stage higher already spares two displays subscribed at
    /// the same instant, but it never covered the route: two browsers opening
    /// the page together decoded the same image twice, and so did a display
    /// and a browser arriving together. This rendezvous is keyed by the cache
    /// key rather than the full identity, because concurrent callers on one
    /// key necessarily see the same source — the stamp is not yet known when
    /// the registration happens, and waiting to know it would mean reading the
    /// share before finding out somebody else already is.
    renditions_in_flight: tokio::sync::Mutex<HashMap<String, RenditionInFlight>>,
    /// How many renditions were **actually** run, whichever consumer asked.
    ///
    /// Under `cfg(test)`, the same trade-off as `builds` just above and for
    /// the same reason: the only proof that a cache saves work is a count of
    /// executions. Comparing two responses says nothing — two successive
    /// builds return the same bytes.
    ///
    /// **Counting per consumer would prove nothing**, and a counter that once
    /// watched the route alone taught us so: it stayed at one however many
    /// times the display path re-encoded, because the display path never
    /// touched it. An assertion that cannot fail is not an assertion.
    #[cfg(test)]
    renditions_built: std::sync::atomic::AtomicUsize,
    /// Full-size embedded extractions in progress, one entry per cache key.
    ///
    /// **What `renditions_in_flight` does not cover.** That rendezvous spares
    /// two consumers building the *same thumbnail* together, but a full-size
    /// request (no `?size=thumbnail`) never asks for a rendition at all —
    /// `cover_get`'s `CoverPayload::Embedded` branch used to call
    /// `read_embedded_bounded` on its own, no rendezvous of any kind guarding
    /// it. That route is unauthenticated on the LAN, so N browsers enlarging
    /// the very same embedded cover (`PlayerCard.vue`'s zoom) each ran their
    /// own `lofty` parse of the whole container, holding the full picture N
    /// times over: with `cover_source_max_mio` at its 20 MiB default, three
    /// concurrent viewers already transiently allocate on the order of a
    /// Pi's entire RAM budget, and ten exhaust it outright.
    embedded_in_flight: tokio::sync::Mutex<HashMap<String, EmbeddedInFlight>>,
    /// How many full-size embedded extractions were **actually** run.
    ///
    /// Under `cfg(test)`, the same trade-off as `builds` and
    /// `renditions_built` above, and for the same reason: only a count of
    /// executions can prove the rendezvous above spares the extraction.
    /// **Cannot be folded into `renditions_built`**: that one counts
    /// *renditions* (thumbnails), and the full-size route this counter
    /// watches never produces one — a counter that watched the wrong path
    /// would stay at one however many times this path actually extracted,
    /// proving nothing.
    #[cfg(test)]
    embedded_extractions: std::sync::atomic::AtomicUsize,
    /// Test-only hold on the one extraction a flight performs.
    ///
    /// **Why production code carries a test hook at all.** The property to
    /// establish is that N callers arriving on one flight cause a single
    /// extraction, and that is a statement about N callers being inside the
    /// flight at the same instant. Spawning N callers does not obtain it:
    /// `read_embedded_bounded` hands its work to a blocking thread, and when
    /// that thread finishes before the handle is first polled, the `await`
    /// returns `Ready` without ever yielding. The first caller then runs
    /// register / extract / remove in a single poll and every follower
    /// arrives to an empty table — eight extractions, and a rendezvous never
    /// exercised. Measured: 13 failures in 60 runs with the test binary
    /// pinned to one CPU, and on CI it turned main red.
    ///
    /// Seeding the cell by hand, the answer for the rendezvous tests that
    /// need only *one* follower (see
    /// `a_picture_from_the_rendezvous_is_labelled_with_its_own_stamp`),
    /// cannot express this property: the first caller to read a seeded cell
    /// removes it, so followers two and up would extract for real.
    ///
    /// **A `Semaphore` rather than a `Notify`**, so that the order of
    /// install-then-release cannot matter: a permit added before the
    /// extraction reaches its `acquire` is still there to be taken, whereas
    /// a notification sent before anyone waits is lost — and a lost wake-up
    /// here is a test that hangs rather than one that fails.
    #[cfg(test)]
    extraction_hold: std::sync::Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    /// The full-size downloads in progress, one entry per cache key.
    ///
    /// **What no rendezvous above covers**, because until this task nothing
    /// on this path downloaded at all: the bare URL of a
    /// `CoverPayload::Pair` served the supplied thumbnail as a stand-in.
    /// Now that it fetches the full size, the shape of the hazard is the one
    /// `embedded_in_flight` documents, moved outward: the route is
    /// unauthenticated on the LAN, so N browsers enlarging the same cover
    /// would issue N requests for the same 2.5 MiB image — not to a disk of
    /// ours this time, but to `coverartarchive.org`, which turns one local
    /// request into an amplifier aimed at a third party — and hold N copies
    /// of the answer at once.
    full_in_flight: tokio::sync::Mutex<HashMap<String, FullInFlight>>,
    /// How many full-size downloads were **actually** performed.
    ///
    /// Under `cfg(test)`, the same trade-off as the three counters above.
    /// **Cannot be folded into any of them**: they watch a decode, a
    /// re-encode and a container parse, none of which this path performs —
    /// a counter watching the wrong stage would sit still however many times
    /// this one went out on the network, which is the definition of an
    /// assertion that cannot fail.
    #[cfg(test)]
    full_downloads: std::sync::atomic::AtomicUsize,
    /// Test-only hold on the one download a flight performs, the twin of
    /// `extraction_hold` and installed for the same reason: N callers must
    /// be inside the flight *at the same instant* for the rendezvous to be
    /// exercised at all, and spawning N tasks does not obtain that on its
    /// own. A `Semaphore` and not a `Notify`, again so that a permit added
    /// before the download reaches its `acquire` is still there to be taken:
    /// a lost wake-up here is a test that hangs rather than one that fails.
    #[cfg(test)]
    full_download_hold: std::sync::Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    /// Test-only body handed back in place of a download.
    ///
    /// **The seam exists because the test binary cannot reach the network,
    /// and must not.** `allowed_target` refuses every literal IP address by
    /// design, so the local HTTP server the `download` tests use is
    /// unreachable through a `CoverRef::Url`, and a hostname would mean a
    /// suite that depends on somebody else's server being up. So under
    /// `cfg(test)` this field *is* the network: absent, a download yields
    /// nothing — which is exactly the failure case the fall-back has to
    /// handle — and present, it yields these bytes.
    ///
    /// **It applies the caller's cap itself**, as `download` does chunk by
    /// chunk. Without that, no test could tell `source_max` from
    /// `download_max` on this path, and the one setting choice this task had
    /// to get right would be unprovable.
    #[cfg(test)]
    canned_full_download: std::sync::Mutex<Option<(Vec<u8>, &'static str)>>,
    /// Keys whose full size has already been reported as unfetchable. See
    /// `report_unfetchable`, which is the only thing that reads or writes it,
    /// and which documents both why a repetition is silenced and why this set
    /// is bounded.
    ///
    /// A `std::sync` lock and not tokio's, like `settings`: the critical
    /// section is a hash lookup, never an IO, and no guard crosses a
    /// suspension point.
    warned_full: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Test-only count of the `warn` lines `report_unfetchable` has actually
    /// emitted.
    ///
    /// **A counter and not a captured log**, though the log was tried first:
    /// `tracing::subscriber::set_default` installs a *thread-local* default,
    /// while the sibling tests of this module reach the very same callsite on
    /// other threads at the same instant. The capture was therefore a race —
    /// measured, one failure in a full run — and a test that reads a global
    /// side effect of the test binary cannot be made deterministic by trying
    /// harder. This counter sits inside the branch the throttle guards, so
    /// removing the throttle moves it, which is the property to prove.
    #[cfg(test)]
    unfetchable_reports: std::sync::atomic::AtomicUsize,
    /// Test-only count of callers that reached a full-size rendezvous —
    /// `extract_embedded`'s or `full_size`'s.
    ///
    /// What lets a test wait on a *state* instead of on a duration: it yields
    /// until every caller it launched has registered, then lifts
    /// `extraction_hold` or `full_download_hold`. Counted at the rendezvous
    /// rather than at the work because followers never reach the work — that
    /// is the very thing the flight exists to spare them.
    ///
    /// **One counter for both**, and not one per rendezvous: an entry is
    /// either an `Embedded` or a `Pair`, so a test exercises exactly one of
    /// the two paths, and a second counter would be a second name for one
    /// property rather than a second property.
    #[cfg(test)]
    rendezvous_arrivals: std::sync::atomic::AtomicUsize,
}

/// A retained rendition: its identity (see `rendition_identity`), its MIME
/// type, and its bytes.
///
/// A named type rather than a triple: the first field is the only one that
/// could be confused with another `String`, and it carries precisely the
/// property that makes this cache safe — see the `renditions` field.
struct Rendered {
    identity: String,
    mime: &'static str,
    bytes: Arc<Vec<u8>>,
}

/// Hard cap on how many entries `entries` may hold, **regardless of what
/// they cost**.
///
/// **Not a memory bound, and it must never be presented as one in the
/// config page** — the user reasons in bytes (`CoverSettings::budget`), and
/// this constant measures something else entirely. It exists only because
/// `File` and `Embedded` cost nothing (see `payload_cost`): a NAS library
/// large enough would grow `entries` forever, since a byte budget can never
/// trigger on a collection whose every member costs zero. This is the belt
/// for exactly that case — nothing more, nothing tuned to any particular
/// amount of memory.
const MAX_ENTRIES: usize = 256;

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The rendition already produced under this identity, if there is one.
    async fn cached_rendition(&self, identity: &str) -> Option<(&'static str, Arc<Vec<u8>>)> {
        self.renditions
            .read()
            .await
            .iter()
            .find(|v| v.identity == identity)
            .map(|v| (v.mime, v.bytes.clone()))
    }

    /// Retains a rendition under its identity, then reconciles the whole
    /// cache against the byte budget — see `evict_to_budget`. The
    /// just-retained identity is passed as the one to protect: a budget so
    /// tight it cannot even hold the rendition just built must still serve
    /// that one, not discard it on arrival.
    ///
    /// **No count-based cap on top, and that is deliberate.** The config
    /// page's estimate of how many covers a budget holds (see
    /// `CoverSettings::budget`'s doc) is built entirely from that budget;
    /// for a NAS library, a rendition is the *only* thing that ever costs
    /// anything (`payload_cost`), so a hidden extra cap here would make
    /// that estimate a lie in exactly the way this chantier exists to
    /// remove. Stale thumbnails do not pile up under a generous budget
    /// either: `evict_to_budget` purges every rendition whose rules no
    /// longer match the live settings **before it even looks at the
    /// budget**, and the budget itself bounds what is left, oldest first.
    /// That "before" is the whole load-bearing word, and it was missing:
    /// while the purge sat inside the eviction loop it only ever ran once
    /// the cache was *already* over budget — which is never, for the local
    /// library this argument is about, since a `File` entry costs nothing.
    async fn remember_rendition(&self, identity: String, mime: &'static str, bytes: Arc<Vec<u8>>) {
        {
            let mut v = self.renditions.write().await;
            v.retain(|e| e.identity != identity);
            v.push_back(Rendered { identity: identity.clone(), mime, bytes });
        }
        self.evict_to_budget(None, Some(&identity)).await;
    }

    /// Frees memory until `entries` and `renditions` together fit
    /// `CoverSettings::budget`, cheapest-to-rebuild first:
    ///
    /// 1. renditions whose rules no longer match the live settings — pure
    ///    waste, nobody will ever ask for them again (`rendition_is_current`).
    ///    **Unconditional**, whatever the budget says;
    /// 2. the oldest remaining rendition;
    /// 3. the oldest remaining source.
    ///
    /// **Why that order.** A rendition rebuilds from its source on the very
    /// next request, at the cost of a decode; a source may need a fresh
    /// download or a read from a sleeping share. The cheap side is spent
    /// first so the expensive side is only touched once the cheap side
    /// truly cannot make room.
    ///
    /// `keep_entry`/`keep_rendition` name what the caller just inserted:
    /// never evicted, so a budget too small for even one cover still serves
    /// that one instead of discarding it the instant it arrives.
    ///
    /// **Step 1 runs unconditionally, before the budget is even measured**,
    /// and that placement is the fix for a concrete failure. It used to sit
    /// inside the loop, past the `total <= budget` guard, so it only ran on a
    /// cache already over budget. Take the gesture it exists to serve: a NAS
    /// library at the defaults, a hundred albums browsed, some thirty
    /// mebibytes of thumbnails held under a fifty-mebibyte budget, and the
    /// user unchecks **Re-encode covers** — precisely to give a Pi its memory
    /// back. Every retained rendition is waste from that instant, and no new
    /// one will ever be produced; but the sources are all local, hence free
    /// (`payload_cost`), the total stays under budget, the loop broke at the
    /// guard, and the thirty mebibytes were held until the process restarted.
    /// Any change to `cover_max_edge_px` fell into the same trap. The purge
    /// costs one `retain` over a deque of at most a few hundred entries, on a
    /// path that already takes both locks, so running it every time buys that
    /// correctness for nothing.
    ///
    /// **It is still `evict_to_budget` that reconciles, not
    /// `set_cover_settings`.** The settings setter is synchronous by
    /// construction (see its doc), so it cannot touch these `tokio` locks;
    /// the purge therefore lands on the first `insert` or `remember_rendition`
    /// after the change, which in service is the next cover the device
    /// handles.
    ///
    /// **Step 3 only ever considers a source that actually costs
    /// something.** A `File`/`Embedded` entry costs 0 (`payload_cost`), so
    /// evicting one can never shrink `total`: doing it anyway would both
    /// fail to fix the budget and destroy a perfectly usable cache entry
    /// for nothing. Entries are in insertion order, not cost order, so the
    /// oldest entry overall and the oldest *evictable* entry are not the
    /// same thing — a NAS `File` sitting before an internet `Bytes` cover
    /// must not be the one sacrificed just because it came first. Bounding
    /// how many zero-cost entries `entries` may hold is `MAX_ENTRIES`'s job
    /// alone, run right after this loop.
    async fn evict_to_budget(&self, keep_entry: Option<&str>, keep_rendition: Option<&str>) {
        let settings = self.settings();

        // Step 1: purge every rendition that answers a question the current
        // settings no longer ask — a lowered longest edge, or the switch
        // turned off altogether, in which case `rendition_is_current` matches
        // nothing and the whole deque goes. Outside the loop and ahead of the
        // budget test on purpose: see the doc above for the thirty mebibytes
        // this placement is what frees.
        {
            let mut renditions = self.renditions.write().await;
            renditions.retain(|r| {
                Some(r.identity.as_str()) == keep_rendition
                    || rendition_is_current(&r.identity, settings.rendition)
            });
        }

        loop {
            let mut renditions = self.renditions.write().await;
            let mut entries = self.entries.write().await;

            let total = renditions.iter().map(|r| r.bytes.len()).sum::<usize>()
                + entries.iter().map(|(_, p)| payload_cost(p)).sum::<usize>();
            if total <= settings.budget {
                break;
            }

            // Step 2: the oldest rendition that is not the one just built.
            if let Some(pos) =
                renditions.iter().position(|r| Some(r.identity.as_str()) != keep_rendition)
            {
                renditions.remove(pos);
                continue;
            }

            // Step 3: the oldest source that both costs something and is
            // not the one just inserted. A zero-cost source is skipped
            // rather than evicted — see the doc above for why.
            match entries
                .iter()
                .position(|(k, p)| Some(k.as_str()) != keep_entry && payload_cost(p) > 0)
            {
                Some(pos) => {
                    entries.remove(pos);
                    continue;
                }
                // Nothing left that would actually free anything: either
                // only zero-cost entries and the protected one remain, or
                // nothing remains at all. Stop rather than spinning through
                // a collection that cannot help.
                None => return,
            }
        }

        // Independent of the byte budget just enforced above — see
        // `MAX_ENTRIES`'s doc for why this exists at all.
        let mut entries = self.entries.write().await;
        while entries.len() > MAX_ENTRIES {
            match entries.iter().position(|(k, _)| Some(k.as_str()) != keep_entry) {
                Some(pos) => {
                    entries.remove(pos);
                }
                None => break,
            }
        }
    }

    /// Produces — or retrieves — the rendition of `key`. **The single place
    /// where a cover is decoded and re-encoded**, whoever is asking.
    ///
    /// `known_stamp` is what the caller already knows about the source's
    /// freshness, and it exists so that neither consumer pays for the other's
    /// shape:
    ///
    /// - `cover_get` passes `Some`. It has just opened the file and read its
    ///   metadata to answer conditionally, so it can name the identity up
    ///   front and be served from memory **without touching the share again**
    ///   — the property that makes a second browser tab free, and one this
    ///   rework must not regress.
    /// - `line` passes `None`. It knows nothing before reading, and reading is
    ///   what it must do anyway; it learns the stamp from the very metadata
    ///   call the read already makes, then looks the identity up. It pays a
    ///   read it was going to pay regardless, and skips the decode.
    ///
    /// `None` means "no rendition to serve" without distinguishing the cases:
    /// re-encoding disabled by the user, unreadable image, dimensions beyond
    /// the cap. Each caller then falls back to the source, which is the answer
    /// it would have given had this cache never existed.
    ///
    /// **The returned `SourceStamp` is the one the served bytes actually
    /// describe**, which is not always the caller's `known_stamp`: a caller
    /// that missed the cache and joined a rendezvous already in flight
    /// collects a picture read before its own stat. `cover_get` builds its
    /// `200`'s `ETag` from this value rather than from its stat — see
    /// `RenditionInFlight` for the browser-permanent staleness that costs.
    async fn rendition_for(
        &self,
        key: &str,
        known_stamp: Option<SourceStamp>,
    ) -> Option<(&'static str, Arc<Vec<u8>>, SourceStamp)> {
        // A single read of the settings for every stage below, like `line`:
        // two reads could straddle a change and produce a rendition under
        // rules that never coexisted.
        let settings = self.settings();
        let rules = settings.rendition?;
        if let Some(stamp) = known_stamp
            && let Some((mime, bytes)) =
                self.cached_rendition(&rendition_identity(key, &stamp, &rules)).await
        {
            // A hit here is filed under the caller's own stamp by
            // construction — the identity was built from it — so handing
            // it back is not an approximation.
            return Some((mime, bytes, stamp));
        }

        // Registration at the rendezvous. The table's lock only covers the
        // registration itself — never the work, which reads a file and
        // occupies a core.
        let cell = {
            let mut in_flight = self.renditions_in_flight.lock().await;
            in_flight.entry(key.to_string()).or_insert_with(RenditionInFlight::default).clone()
        };
        let result = cell
            .get_or_init(|| async {
                let (mime, bytes, stamp) = self.bytes(key, settings.source_max).await?;
                // **The identity comes from the read, never from the caller's
                // stamp**, even when the caller had one. The two can disagree:
                // `cover_get` stat'ed the file to answer conditionally, and
                // the share is not ours — the image may have been replaced in
                // between. Filing under the caller's stamp would then store
                // the *new* bytes under the *old* identity, and serve them to
                // everyone who still validates against it. Filing under what
                // was actually read cannot lie; the caller merely gets bytes
                // fresher than the ETag it will label them with, and the next
                // revalidation corrects that.
                let identity = rendition_identity(key, &stamp, &rules);
                // **Looked up again, now that the stamp is known.** This is
                // where the caller that could not name the identity up front
                // collects what the other one already built — the whole point
                // of sharing. A caller that arrived with its stamp has already
                // missed above, and pays one comparison per retained
                // rendition: `renditions` carries no count cap of its own, the
                // byte budget alone bounds it (`evict_to_budget`), so that is
                // a walk over however many a generous budget holds, not over
                // a fixed handful.
                if let Some((mime, bytes)) = self.cached_rendition(&identity).await {
                    return Some((mime, bytes, stamp));
                }
                #[cfg(test)]
                self.renditions_built.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (mime, bytes) = rendition(mime, bytes, rules).await?;
                let bytes = Arc::new(bytes);
                self.remember_rendition(identity, mime, bytes.clone()).await;
                // The stamp of the read, travelling out with the bytes it
                // describes: whoever joined this cell after stat'ing the file
                // must label the response with *this*, not with its own stat.
                Some((mime, bytes, stamp))
            })
            .await
            .clone();

        // **The removal is what keeps the rendezvous from becoming a second
        // cache**, exactly as for `in_flight`: a `OnceCell` keeps its value
        // forever, and this one is keyed by the bare key, which says nothing
        // about the source's freshness. All callers attempt it, and the
        // identity is checked first so that a fresher cell registered
        // meanwhile is not stolen from its owner.
        {
            let mut in_flight = self.renditions_in_flight.lock().await;
            if in_flight.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
                in_flight.remove(key);
            }
        }
        result
    }

    /// Extracts (or retrieves, mid-flight) `audio`'s embedded picture at
    /// **full size**, under the rendezvous keyed by `key`. This is what
    /// `cover_get`'s `CoverPayload::Embedded` branch now calls instead of
    /// reaching for `read_embedded_bounded` on its own — see
    /// `embedded_in_flight`'s doc for the concrete failure this closes.
    ///
    /// **The same shape as `line` and `rendition_for`, not a third variant**:
    /// a `OnceCell` per key behind an `Arc`, registered under the table's
    /// lock and then awaited outside it — the lock never covers the work,
    /// which occupies a blocking-pool thread parsing a container — removed
    /// afterwards by whichever caller finds it still pointing at the cell it
    /// registered.
    ///
    /// **The picture travels as `axum::body::Bytes`, and that is the one
    /// property that matters.** Cloning the `OnceCell`'s answer must stay a
    /// refcount bump: an early version of this rendezvous shared an
    /// `Arc<Vec<u8>>` instead, then had `cover_get` clone it out into an
    /// owned `Vec` to build the response body — which meant every one of the
    /// N waiters still paid for a full copy of the picture on its way out,
    /// the rendezvous sparing the extraction but spending the memory right
    /// back on the very last step. `Bytes` is refcounted the same way `Arc`
    /// is, but is *also* what `axum::body::Body` is built from directly
    /// (`Bytes: IntoResponse`), so there is no clone-out left to perform:
    /// every response body shares the one buffer `read_embedded_bounded`
    /// produced.
    ///
    /// Keyed by the bare cache key rather than a full identity, exactly like
    /// `renditions_in_flight`: concurrent callers on one key necessarily read
    /// the same audio file, and there is no stamp yet to fold into an
    /// identity before that read happens.
    async fn extract_embedded(
        &self,
        key: &str,
        audio: &std::path::Path,
        cap: usize,
    ) -> Option<(&'static str, axum::body::Bytes, SourceStamp)> {
        let cell = {
            let mut in_flight = self.embedded_in_flight.lock().await;
            in_flight.entry(key.to_string()).or_insert_with(EmbeddedInFlight::default).clone()
        };
        // Registered, whether this caller goes on to extract or to wait. See
        // the `rendezvous_arrivals` field.
        #[cfg(test)]
        self.rendezvous_arrivals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = cell
            .get_or_init(|| async {
                let (mime, bytes, stamp) = self.read_embedded_bounded(audio, cap).await?;
                // `Bytes::from(Vec<u8>)` takes ownership of the allocation
                // as is — no copy, unlike `Arc::new` followed by a later
                // clone-out would have needed.
                //
                // **The stamp is relayed, not dropped.** This cell used to
                // discard it, which is what let `cover_get` label a picture
                // extracted before a retag with the `ETag` of its own, later
                // stat — see `EmbeddedInFlight`.
                Some((mime, axum::body::Bytes::from(bytes), stamp))
            })
            .await
            .clone();

        // Same reasoning as `line`/`rendition_for`: a `OnceCell` keeps its
        // value forever, and this rendezvous is keyed by the bare cache key,
        // which says nothing about the source's freshness — left in the
        // table, it would go on serving a picture read long ago to a caller
        // showing up after the audio file changed under its path.
        {
            let mut in_flight = self.embedded_in_flight.lock().await;
            if in_flight.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
                in_flight.remove(key);
            }
        }
        result
    }

    /// The full-size half of a pair, fetched **once** however many readers
    /// enlarge the cover, and memoised so that the next one is free.
    ///
    /// The same shape as the three rendezvous above — a `OnceCell` per key
    /// behind an `Arc`, registered under the table's lock and awaited
    /// outside it, removed afterwards by whichever caller still finds the
    /// table pointing at the cell it registered. That last check is
    /// `Arc::ptr_eq` and not mere presence, for the reason `rendition_for`
    /// spells out: a fresher cell registered meanwhile belongs to its own
    /// callers and must not be stolen from them.
    ///
    /// **The rendezvous and the memo answer two different questions**, and
    /// only having both bounds the traffic. The rendezvous collapses callers
    /// that are inside the flight *together*; the memo bounds callers that
    /// come *one after another*, which is the half one forgets. Without it,
    /// every enlargement of the same cover asks a third party for the same
    /// 2.5 MiB again, and a loop on one valid key — the key itself is not
    /// attacker-chosen, but the repetition is anybody's — turns this local
    /// route into an amplifier aimed at that third party.
    ///
    /// **What the memo does not bound, and it must be said.** It bounds the
    /// repetition *per key, for as long as that key's entry survives*. A
    /// client on the LAN that rotates over enough keys for their full sizes
    /// to exceed `CoverSettings::budget` — some twenty pairs of 2.5 MiB under
    /// a 50 MiB budget — evicts each memo before coming back to it, and every
    /// cycle downloads afresh. That is inherent to a memo bounded by a
    /// budget, not a defect of this one: the alternatives are a memory that
    /// ignores the budget, or a per-client rate limit on a route that has no
    /// notion of a client. It is named here rather than discovered later, and
    /// the two facts that keep it from being alarming are that the appliance
    /// is on a home network and that the rate is bounded by the budget's
    /// turnover, not by the client's own cadence.
    ///
    /// **Two failures are deliberately not memoised**, and both are residuals
    /// rather than oversights — see `report_unfetchable` for the log side of
    /// the first:
    ///
    /// * a **failed** fetch: a broken target asked again costs one failed
    ///   request per request, and what bounds the repetition is **not** a
    ///   human gesture — the client is a process on the local network, free
    ///   to loop at its own cadence. What bounds it is the size of a failure:
    ///   a 404 or a refused target is a few hundred bytes against 2.5 MiB for
    ///   a success, and `report_unfetchable` already keeps the journal from
    ///   growing with it. Bounding the *requests* would take a memory of
    ///   failures — new machinery for a small cost, set aside at this stage
    ///   rather than overlooked;
    /// * a **local** full size: `CoverRef::Path` is served and never
    ///   memoised. This module holds no local file in memory — a `File`
    ///   payload costs a path and is streamed, and a three-megabyte
    ///   `folder.jpg` on a NAS is commonplace. The rule of this repository is
    ///   that the network means the internet: the share is local, re-readable
    ///   at the cost of a read, and memoising it would charge the budget for
    ///   megabytes the appliance has no reason to hold.
    async fn full_size(
        &self,
        key: &str,
        full: &CoverRef,
        cap: usize,
    ) -> Option<(&'static str, axum::body::Bytes)> {
        let cell = {
            let mut in_flight = self.full_in_flight.lock().await;
            in_flight.entry(key.to_string()).or_insert_with(FullInFlight::default).clone()
        };
        // Registered, whether this caller goes on to download or to wait. See
        // the `rendezvous_arrivals` field.
        #[cfg(test)]
        self.rendezvous_arrivals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = cell
            .get_or_init(|| async {
                // **The memo, read again from inside the cell.** `cover_get`
                // has already looked at the payload it read, but it read it
                // before this flight existed: a caller that arrived just
                // before another flight wrote its memo back would otherwise
                // download a second time. Closing that window costs one read
                // of the entries table, taken only by the caller that is
                // about to go out on the network anyway.
                if let Some((bytes, mime)) = self.memoised_full(key).await {
                    return Some((mime, shared_body(bytes)));
                }
                let Some((mime, bytes)) = self.download_full(full, cap).await else {
                    // The one place that knows a fetch failed, hence the one
                    // place that reports it — and reports it at most once per
                    // key, see `report_unfetchable`.
                    self.report_unfetchable(key, full);
                    return None;
                };
                let bytes = Arc::new(bytes);
                // **Only a success is memoised, and only a remote one.** The
                // two exclusions are argued on this method's doc; both are
                // accepted residuals, and saying so is what keeps the next
                // reader from taking either for an oversight.
                if matches!(full, CoverRef::Url { .. }) {
                    self.remember_full(key, bytes.clone(), mime).await;
                    // This key answers again: a later outage deserves to be
                    // reported afresh rather than silenced by the memory of
                    // the last one.
                    self.warned_full.lock().unwrap().remove(key);
                }
                Some((mime, shared_body(bytes)))
            })
            .await
            .clone();

        {
            let mut in_flight = self.full_in_flight.lock().await;
            if in_flight.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
                in_flight.remove(key);
            }
        }
        result
    }

    /// Reports a full size that could not be fetched — **at most once per
    /// key**, and at `debug` afterwards.
    ///
    /// **A `warn` per click would let the LAN write the journal.** This route
    /// needs no authentication, the failure is not memoised (deliberately, so
    /// that a recovered target is picked up on the next click), and a client
    /// looping on one broken key would therefore fill `journalctl` at its own
    /// cadence. The first failure of a key is the event worth a `warn` — the
    /// owner sees a soft image and needs the reason; the hundredth adds
    /// nothing the first did not say.
    ///
    /// **The set is bounded**, and cleared wholesale rather than trimmed by
    /// age: a client rotating over keys must not be able to grow it without
    /// end either. The worst a clear can do is allow one further `warn` per
    /// key — this remembers a repetition, it does not keep a history.
    fn report_unfetchable(&self, key: &str, full: &CoverRef) {
        let first = {
            let mut warned = self.warned_full.lock().unwrap();
            if warned.len() >= MAX_ENTRIES {
                warned.clear();
            }
            warned.insert(key.to_string())
        };
        if first {
            #[cfg(test)]
            self.unfetchable_reports.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tracing::warn!(
                "cover {key}: the full size at {full:?} could not be fetched, \
                 serving the supplied thumbnail instead"
            );
        } else {
            tracing::debug!("cover {key}: the full size at {full:?} is still unfetchable");
        }
    }

    /// The full size already downloaded for `key`, if there is one. The
    /// clone is a refcount bump — see `CoverPayload::Pair`'s `fetched`.
    async fn memoised_full(&self, key: &str) -> Option<(Arc<Vec<u8>>, &'static str)> {
        self.entries.read().await.iter().find(|(k, _)| k == key).and_then(|(_, p)| match p {
            CoverPayload::Pair { fetched, .. } => fetched.clone(),
            _ => None,
        })
    }

    /// Writes a downloaded full size back into its entry, then reconciles the
    /// cache against the byte budget — the same reconciliation `insert`
    /// performs, and for the same reason: these bytes are now held, so they
    /// are now charged (`payload_cost`).
    ///
    /// **Mutated in place rather than re-inserted.** `insert` moves an entry
    /// to the back of the deque, which is the eviction order; enlarging a
    /// cover is no reason for an older entry to outlive a newer one.
    ///
    /// `key` is handed to `evict_to_budget` as the entry to protect, exactly
    /// as `insert` does: the entry that just grew must not be evicted to pay
    /// for its own growth, when the caller is holding the very bytes it is
    /// about to serve.
    async fn remember_full(&self, key: &str, bytes: Arc<Vec<u8>>, mime: &'static str) {
        {
            let mut entries = self.entries.write().await;
            let Some((_, CoverPayload::Pair { fetched, .. })) =
                entries.iter_mut().find(|(k, _)| k == key)
            else {
                // Evicted, or replaced by another payload, while the download
                // was in flight. There is nothing to memoise onto and nothing
                // to reconcile: the caller still gets its bytes, and a later
                // enlargement will download again.
                return;
            };
            *fetched = Some((bytes, mime));
        }
        self.evict_to_budget(Some(key), None).await;
    }

    /// Obtains the full-size half of a pair. **The one place a full size is
    /// actually fetched**, hence the one true place to count one — the same
    /// reasoning as `read_embedded_bounded` and `embedded_extractions`: a
    /// counter incremented by a caller would prove that a caller ran, never
    /// that a download did.
    ///
    /// **`cap` is `source_max` and not `download_max`, and that distinction
    /// is the whole reason the pair needed no new setting.** The two bound
    /// different things: `download_max` bounds what the appliance fetches
    /// **by itself**, on the announcement of every track it plays, where two
    /// mebibytes is already generous; `source_max` bounds what it agrees to
    /// read at all, whoever asked. An enlargement is neither automatic nor
    /// per-track — it is one gesture of one reader — so it is the second
    /// bound that applies. Under the first, a 2.5 MiB original would be
    /// refused here for exactly the reason it was refused on announcement,
    /// and the pair would have bought nothing at all.
    async fn download_full(
        &self,
        full: &CoverRef,
        cap: usize,
    ) -> Option<(&'static str, Vec<u8>)> {
        #[cfg(test)]
        self.full_downloads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Before any work, so that a test can hold this one flight open while
        // it watches the followers arrive. See the `full_download_hold`
        // field. The guard is a temporary of its own statement, so it is
        // released before the `await` below — a `std::sync::MutexGuard` held
        // across an await point would not compile here, and should not.
        #[cfg(test)]
        {
            let hold = self.full_download_hold.lock().unwrap().clone();
            if let Some(hold) = hold {
                let _permit = hold.acquire().await;
            }
        }
        self.perform_full_download(full, cap).await
    }

    /// Where the fetch is aimed: the same two doors as `fetch_ref`, spelt
    /// here because this one wants the bytes rather than a payload.
    ///
    /// **A `Pair` whose other half is a local file is no longer built** —
    /// `fetch` pairs only over a remote full size, for the reason written
    /// there — but this branch stays: the payload type still lets such a pair
    /// be expressed, and answering nothing for a half whose path is in hand
    /// would be a hole nobody would think to look for.
    ///
    /// **This whole method runs under test as it does in service** — the
    /// filter, the local read, its cap and its time bound. Only the body of a
    /// remote fetch diverges, at `fetch_url` below, and that is as low as the
    /// seam can be pushed.
    async fn perform_full_download(
        &self,
        full: &CoverRef,
        cap: usize,
    ) -> Option<(&'static str, Vec<u8>)> {
        match full {
            CoverRef::Url { url } => {
                if !allowed_target(url) {
                    tracing::debug!("cover fetch refused: target not allowed");
                    return None;
                }
                self.fetch_url(url, cap).await
            }
            CoverRef::Path { path } => read_file_bounded(std::path::Path::new(path), cap)
                .await
                .map(|(mime, bytes, _)| (mime, bytes)),
        }
    }

    /// The remote body, in the shipped binary.
    #[cfg(not(test))]
    async fn fetch_url(&self, url: &str, cap: usize) -> Option<(&'static str, Vec<u8>)> {
        match download(url, cap).await {
            Some(CoverPayload::Bytes(bytes, mime)) => Some((mime, bytes)),
            // `download` only ever produces `Bytes`; anything else would be a
            // defect there, and serving nothing is the same answer as a
            // failed fetch.
            _ => None,
        }
    }

    /// The remote body, under test: the canned one, or nothing.
    ///
    /// **These are the only two lines of this path the test binary replaces**
    /// — `download`'s call and the match around it. Everything above them is
    /// the real code: `allowed_target` judges the real URL, so a refused
    /// target is proven without a socket ever being opened, and a local full
    /// size goes through the real `read_file_bounded`. See the
    /// `canned_full_download` field for why the network itself cannot be
    /// reached from here.
    #[cfg(test)]
    async fn fetch_url(&self, _url: &str, cap: usize) -> Option<(&'static str, Vec<u8>)> {
        let (bytes, mime) = self.canned_full_download.lock().unwrap().clone()?;
        // The cap the caller handed down, applied as `download` applies it.
        if bytes.len() > cap {
            tracing::debug!("cover fetch refused: over {cap} bytes");
            return None;
        }
        Some((mime, bytes))
    }

    /// How many times a frame was built since the cache was created.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many renditions were run since the cache was created, both
    /// consumers taken together. See the `renditions_built` field.
    #[cfg(test)]
    pub(crate) fn renditions_built(&self) -> usize {
        self.renditions_built.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many full-size embedded extractions ran since the cache was
    /// created. See the `embedded_extractions` field.
    #[cfg(test)]
    pub(crate) fn embedded_extractions(&self) -> usize {
        self.embedded_extractions.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Installs the hold described on the `extraction_hold` field: every
    /// extraction begun afterwards waits for a permit before doing any work.
    #[cfg(test)]
    pub(crate) fn hold_extractions(&self, hold: Arc<tokio::sync::Semaphore>) {
        *self.extraction_hold.lock().unwrap() = Some(hold);
    }

    /// How many full-size downloads ran since the cache was created. See the
    /// `full_downloads` field.
    #[cfg(test)]
    pub(crate) fn full_downloads(&self) -> usize {
        self.full_downloads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Installs the hold described on the `full_download_hold` field: every
    /// download begun afterwards waits for a permit before doing any work.
    #[cfg(test)]
    pub(crate) fn hold_full_downloads(&self, hold: Arc<tokio::sync::Semaphore>) {
        *self.full_download_hold.lock().unwrap() = Some(hold);
    }

    /// Installs the body a download answers with, in place of the network the
    /// test binary cannot reach. See the `canned_full_download` field.
    #[cfg(test)]
    pub(crate) fn answer_full_downloads_with(&self, bytes: Vec<u8>, mime: &'static str) {
        *self.canned_full_download.lock().unwrap() = Some((bytes, mime));
    }

    /// How many unfetchable full sizes were **reported** since the cache was
    /// created — `warn` lines, not failures. See the `unfetchable_reports`
    /// field.
    #[cfg(test)]
    pub(crate) fn unfetchable_reports(&self) -> usize {
        self.unfetchable_reports.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many callers have reached a full-size rendezvous. See the
    /// `rendezvous_arrivals` field.
    #[cfg(test)]
    pub(crate) fn rendezvous_arrivals(&self) -> usize {
        self.rendezvous_arrivals.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Publishes new settings, taken into account at the next publication.
    ///
    /// **Nothing to invalidate, and this line used to say so for the wrong
    /// reason.** It read "nothing is memorized", which stopped being true the
    /// day renditions were cached: lowering the longest edge then kept serving
    /// the old size until the entry fell out on its own. What settles it is
    /// not an absence of memory but the identity — a rendition is filed under
    /// the rules that produced it (see `Rendition::tag`), so other rules mean
    /// another identity, and the stale entry is simply never asked for again.
    ///
    /// **Correctness, not memory**, and the distinction cost thirty mebibytes
    /// once: an identity nobody asks for is still an identity somebody is
    /// paying for. Reclaiming it is `evict_to_budget`'s unconditional first
    /// step, which runs on the next `insert` or `remember_rendition` — this
    /// method cannot run it itself, being synchronous on purpose (the lock
    /// below is a `std::sync` one so that `Core::set_settings` need not become
    /// `async`, contaminating its signature and every test caller).
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
    ///
    /// `pub(crate)`: `Core::start_cover_fetch` reads `download_max` off it to
    /// hand to `cover::fetch`, which must not read the cache's own lock
    /// itself — it runs detached, well past where this cache's `Arc` was
    /// cloned out.
    pub(crate) fn settings(&self) -> CoverSettings {
        match self.settings.read() {
            Ok(g) => *g,
            Err(e) => *e.into_inner(),
        }
    }

    /// Retains `p` under `key`, then reconciles the whole cache against the
    /// byte budget — see `evict_to_budget`.
    ///
    /// **No longer a count-based cap.** This method used to enforce
    /// `CoverSettings::entries`, a number of covers; that setting is gone,
    /// replaced by a memory budget in bytes (`CoverSettings::budget`), and a
    /// cache that is meant to stay under a byte budget cannot decide
    /// anything by counting its entries — a `Bytes` payload and a `File`
    /// payload are not the same cost at all (see `CoverPayload` and
    /// `payload_cost`). `key` is passed to `evict_to_budget` as the entry to
    /// protect: the one just inserted is never the one evicted to make room
    /// for itself.
    pub async fn insert(&self, key: String, p: CoverPayload) {
        {
            let mut e = self.entries.write().await;
            e.retain(|(k, _)| k != &key);
            e.push_back((key.clone(), p));
        }
        self.evict_to_budget(Some(&key), None).await;
    }

    pub async fn contains(&self, key: &str) -> bool {
        self.entries.read().await.iter().any(|(k, _)| k == key)
    }

    async fn read(&self, key: &str) -> Option<CoverPayload> {
        self.entries.read().await.iter().find(|(k, _)| k == key).map(|(_, p)| p.clone())
    }

    /// Materializes the bytes of a cover: `(mime, bytes, stamp)`.
    ///
    /// The stamp comes back with the bytes rather than from a separate call,
    /// and that is what makes it trustworthy: for a file it is read from the
    /// metadata of the **descriptor that was read**, so no window separates
    /// the stamp from the content it describes. It also costs nothing —
    /// `read_file_bounded` already asked for that metadata to check the size,
    /// and threw the modification date away.
    ///
    /// **Precisely what the HTTP route avoids doing for `File`.** That one
    /// opens, checks the header and *streams* without ever holding the whole
    /// image. Pushing onto a socket leaves no such choice, hence this method —
    /// and hence the cap, which did not exist on the local side (see
    /// `COVER_MAX_BYTES` and the doc of `fetch`). For `Embedded`, the route
    /// has no such choice to begin with: a container is not a stream of image
    /// bytes, so extracting through `read_embedded_bounded` is the only way to
    /// get any, whether the caller is this method or, through
    /// `extract_embedded`'s rendezvous, the route's own body.
    ///
    /// **The `Embedded` branch goes through `extract_embedded`, not straight
    /// to `read_embedded_bounded`.** This method is the socket side's door in
    /// (`line` → `rendition_for` → here) and `cover_get`'s bare-URL branch is
    /// the HTTP side's; each calling the reader on its own meant a display
    /// subscribing at the very instant a browser enlarged the same cover ran
    /// two independent `lofty` parses of one container, each holding the whole
    /// picture — the exact duplication `embedded_in_flight` was installed to
    /// end, left half-open because only one of the two callers used it. The
    /// `to_vec` below is the price: `extract_embedded` shares
    /// `axum::body::Bytes` because that is what a response body is built from,
    /// and this side needs an owned `Vec` to re-encode or to put in a
    /// `ritornello_proto::Cover`. One memcpy against one avoided container
    /// parse is not a trade worth hesitating over.
    ///
    /// `None` covers indistinctly: unknown key, file vanished or unreadable,
    /// share not answering, content that is no longer an image, and **size
    /// beyond the cap**. The caller has nothing to distinguish among them: in
    /// every case the display has no image, just as it has none when the fetch
    /// fails.
    /// The cap is **passed by the caller** rather than re-read here, so that
    /// `line` reads the settings only once: two reads could straddle a change,
    /// and produce a thumbnail under rules that never coexisted.
    async fn bytes(
        &self,
        key: &str,
        cap: usize,
    ) -> Option<(&'static str, Vec<u8>, SourceStamp)> {
        // The lock is released **before** any IO. A local cover commonly lives
        // on a sleeping share: holding the read lock during `FILE_TIMEOUT`
        // would block the cache's insertions, hence the detached task of
        // `Core::start_cover_fetch`, for one image.
        //
        // The `Bytes` branch answers under the lock rather than going through
        // `read`: that one clones the whole `CoverPayload`, which would make
        // two copies of the bytes instead of one.
        // `File` and `Embedded` both name a path rather than bytes, and both
        // are read below the lock's release; only the read itself differs —
        // straight for one, through a container for the other.
        enum OnDisk {
            File(PathBuf),
            Embedded(PathBuf),
        }
        let source = {
            let e = self.entries.read().await;
            match e.iter().find(|(k, _)| k == key).map(|(_, p)| p) {
                None => return None,
                // Already in memory, and already bounded by construction:
                // these bytes come from an HTTP body that `download` cut at
                // `CoverSettings::download_max`.
                //
                // The `cap` given to this method (`source_max`) is checked
                // anyway: it is a **different** setting from `download_max`,
                // and it can be lowered below it, at which point the
                // construction-time bound no longer says anything. Without
                // this check, `source_max` would only apply to local files —
                // true today by the mere coincidence of the two defaults, and
                // false as soon as either is touched.
                Some(CoverPayload::Bytes(v, mime)) => {
                    if v.len() > cap {
                        tracing::warn!(
                            "network cover not pushed: {} bytes over the {cap}-byte limit",
                            v.len()
                        );
                        return None;
                    }
                    return Some((*mime, v.clone(), SourceStamp::Frozen));
                }
                // **The thumbnail, never the full size.** This method is the
                // socket side's door in, and what the displays receive is a
                // square of a couple of hundred pixels: they have never wanted
                // the full-size image. Nor does a memoised one change that —
                // `fetched` is filled by a reader enlarging the cover in a
                // browser, and pushing those megabytes at a twenty-column
                // display would be the very waste the pair exists to
                // avoid. Same cap and same
                // `Frozen` stamp as `Bytes` above, for the same reasons — this
                // is a network image, frozen under its key.
                Some(CoverPayload::Pair { thumb, thumb_mime, .. }) => {
                    if thumb.len() > cap {
                        tracing::warn!(
                            "supplied thumbnail not pushed: {} bytes over the {cap}-byte limit",
                            thumb.len()
                        );
                        return None;
                    }
                    return Some((*thumb_mime, thumb.clone(), SourceStamp::Frozen));
                }
                Some(CoverPayload::File(c)) => OnDisk::File(c.clone()),
                Some(CoverPayload::Embedded(c)) => OnDisk::Embedded(c.clone()),
            }
        };
        match source {
            OnDisk::File(path) => read_file_bounded(&path, cap).await,
            OnDisk::Embedded(audio) => {
                let (mime, bytes, stamp) = self.extract_embedded(key, &audio, cap).await?;
                Some((mime, bytes.to_vec(), stamp))
            }
        }
    }

    /// Builds the `DisplayFrame::Cover` protocol line for `key`/`href`: the
    /// complete JSON, base64 included, terminated by a newline, ready to be
    /// written as is onto a socket.
    ///
    /// **The line itself is built at every call and never memorized, and that
    /// is the property that matters.** An encoded line kept from one call to
    /// the next was tried here, then removed: the cache key hashes the *path*,
    /// not the content, so a kept line became wrong as soon as the user
    /// replaced the image under that path. And the gesture leading there takes
    /// three clicks — disable the display from the admin page, replace the
    /// `folder.jpg`, re-enable it: the reconnected relay starts over with its
    /// deduplication guard at zero (`main::display_relay`, `CoverTracking`),
    /// asks for the current cover again, and used to receive the line from
    /// before. Nothing invalidated it because nothing *could* invalidate it:
    /// replacing a file on a share goes through no code of ours. A visibly
    /// wrong image is the worst defect of this device, far above a memory
    /// spike.
    ///
    /// **The rendition inside it, on the other hand, is now shared** — and it
    /// is the same objection that made it possible rather than a change of
    /// mind about it. What forbade memorizing was the key, not the desire;
    /// `rendition_for` files its work under an identity that carries the
    /// source's stamp (see `SourceStamp`), so the replaced `folder.jpg` is a
    /// different identity and the reconnected relay above still gets the new
    /// image. What this call no longer pays for is the decode the page's
    /// square may already have paid — several hundred milliseconds of a Pi 2's
    /// core, per published cover.
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
                // One read of the settings for this frame's own decisions
                // — which cap to read under, and whether to render at all.
                // Two reads *here* could straddle a change and mix a cap from
                // before with a switch from after.
                //
                // It is **not** the only read on the path: `rendition_for`
                // takes its own, and must, since it is also entered straight
                // from `cover_get`. So a settings change landing between the
                // two can have this frame read under one cap and rendered
                // under rules published a moment later. Benign — both values
                // are ones the user has just asked for, and the identity a
                // rendition is filed under carries the rules that produced it
                // (`Rendition::tag`), so nothing is ever *served* under rules
                // it was not built with. The claim to avoid is the tidy one
                // this comment used to make: that a single read covered every
                // stage.
                let settings = self.settings();
                // The rendition is asked of `rendition_for` and no longer
                // performed here, so that what the page's square already had
                // re-encoded is not re-encoded a second time for the socket.
                // `None` as the stamp: this path knows nothing of the source
                // before reading it, and reading is what it must do anyway.
                //
                // The rendition applies **on the push path only**. The HTTP
                // route `cover_get`, for its part, streams the local file
                // without ever holding it whole: forcing a re-encode on it
                // would make it lose exactly the property that makes it cheap,
                // for an image the browser resizes and caches on its side.
                let (mime, bytes) = match settings.rendition {
                    None => {
                        let (mime, bytes, _) = self.bytes(key, settings.source_max).await?;
                        (mime, bytes)
                    }
                    // Cloned out of the `Arc`: `ritornello_proto::Cover` owns
                    // its bytes, as a wire type should — expressing sharing in
                    // it would put an `Arc` inside a serialized structure, the
                    // very thing the doc below refuses. A memcpy of a few
                    // hundred kibibytes against the several hundred
                    // milliseconds of a decode is not a trade worth hesitating
                    // over.
                    // The stamp comes back too and is dropped here: this
                    // path publishes bytes on a socket under no validator at
                    // all, so there is nothing for it to label. Only
                    // `cover_get` has an `ETag` to get right.
                    Some(_) => {
                        let (mime, bytes, _) = self.rendition_for(key, None).await?;
                        (mime, bytes.as_ref().clone())
                    }
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
/// **This function memorizes nothing — its caller does.** The distinction was
/// blurred here, and the line used to read "nothing is memorized" while
/// `rendition_for` was already filing the result away. What is true is
/// narrower and worth keeping straight: the decode is a pure function of
/// `(bytes, rules)`, so it is the wrong place to hold anything. Whether a
/// result may be reused depends on facts this function cannot see — whether
/// the source is still the one that produced those bytes — and that judgment
/// belongs to `rendition_for`, which has the stamp to make it.
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
    // The rule, shared with `cover_get`'s `Pair` arm — see
    // `Rendition::leaves_alone` for why it is a method and not a condition
    // written out twice.
    if r.leaves_alone(bytes.len(), (width, height)) {
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
    if output.len() > r.net() {
        tracing::warn!(
            "cover not pushed: rendered to {} bytes, over the {}-byte net derived from a \
             {} px edge. The net is two bytes per pixel, against the 0.61 measured at q90 on \
             the heaviest cover of a real library, so the likeliest cause is a high quality \
             (currently {}): lowering it is the adjustment to try first. At a moderate \
             quality this should not be reachable, and reaching it is then worth reporting.",
            output.len(),
            r.net(),
            r.max_edge_px,
            r.jpeg_quality
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
        // Trimmed for the same reason as the JPEG below and as `download`:
        // this buffer is about to be retained under the byte budget, which
        // measures `len()` where the allocator measures `capacity()`.
        output.shrink_to_fit();
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
    output.shrink_to_fit();
    Some(("image/jpeg", output))
}

/// What the bounded read of a cover file returns, before the image type is
/// validated.
enum BoundedRead {
    Bytes(Vec<u8>, SourceStamp),
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
) -> Option<(&'static str, Vec<u8>, SourceStamp)> {
    let read_attempt = tokio::time::timeout(FILE_TIMEOUT, async {
        let file = tokio::fs::File::open(path).await?;
        // The **whole** metadata and not just its length: the modification
        // date completes the source's stamp, and it is free here — this call
        // was already being made for the size check. Taken from the descriptor
        // that is about to be read, so the stamp and the bytes cannot describe
        // two different versions of the file.
        let meta = file.metadata().await?;
        let size = meta.len();
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
        Ok(BoundedRead::Bytes(bytes, SourceStamp::of_file(&meta)))
    })
    .await;
    let (bytes, stamp) = match read_attempt {
        Ok(Ok(BoundedRead::Bytes(v, stamp))) => (v, stamp),
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
    Some((mime, bytes, stamp))
}

/// Reads the picture embedded in an audio file, bounded like a file read.
///
/// The counterpart of `read_file_bounded` for `CoverPayload::Embedded`, and
/// deliberately its twin: same cap, same time bound, same return shape. The
/// only difference is irreducible — a container must be parsed to find the
/// bytes, where a `folder.jpg` can be read straight.
///
/// `lofty` is strictly blocking, hence `spawn_blocking`. The stamp describes
/// the **audio file**, since that is what a caller will validate against.
///
/// **A method, not a free function, purely to host `embedded_extractions`.**
/// This is the only place a container is actually parsed for its picture, so
/// it is the one true place to count an extraction: both of this method's
/// callers below (`bytes`, and — through `extract_embedded` —
/// `cover_get`) go through here, and neither is left free to increment the
/// counter on its own, which would only prove that a caller ran, not that an
/// extraction did.
impl CoverCache {
    async fn read_embedded_bounded(
        &self,
        audio: &std::path::Path,
        cap: usize,
    ) -> Option<(&'static str, Vec<u8>, SourceStamp)> {
        #[cfg(test)]
        self.embedded_extractions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Before any work, so that a test can keep this one flight open while
        // it watches the followers arrive. See the `extraction_hold` field.
        // The guard is a temporary of its own statement, so it is released
        // before the `await` below — a `std::sync::MutexGuard` held across an
        // await point would not compile here, and should not.
        #[cfg(test)]
        {
            let hold = self.extraction_hold.lock().unwrap().clone();
            if let Some(hold) = hold {
                let _permit = hold.acquire().await;
            }
        }
        let path = audio.to_path_buf();
        let work = tokio::time::timeout(
            FILE_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                let meta = std::fs::metadata(&path).ok()?;
                let stamp = SourceStamp::of_file(&meta);
                let file = lofty::probe::Probe::open(&path).ok()?.read().ok()?;
                let bytes = lofty::file::TaggedFileExt::primary_tag(&file)
                    .or_else(|| lofty::file::TaggedFileExt::first_tag(&file))?
                    .pictures()
                    .first()?
                    .data()
                    .to_vec();
                if bytes.len() > cap {
                    tracing::warn!(
                        "embedded cover in {} is {} bytes, over the {cap}-byte limit",
                        path.display(),
                        bytes.len()
                    );
                    return None;
                }
                let mime = image_type(&bytes)?;
                Some((mime, bytes, stamp))
            }),
        )
        .await;
        match work {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!("embedded cover extraction panicked: {e}");
                None
            }
            Err(_) => {
                tracing::warn!(
                    "embedded cover in {} did not answer in {FILE_TIMEOUT:?}",
                    audio.display()
                );
                None
            }
        }
    }
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
///
/// `cap` is `CoverSettings::download_max`, in bytes, **given by the caller**
/// rather than read from a constant here: that is what makes the cut
/// testable at more than one value, where a hard-coded bound could only ever
/// be exercised at the one figure baked into the binary.
async fn download(url: &str, cap: usize) -> Option<CoverPayload> {
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
        if bytes.len() + chunk.len() > cap {
            tracing::debug!("cover fetch refused: over {cap} bytes");
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let mime = image_type(&bytes)?;
    // **Trimmed before it is cached, and that is an accounting matter, not
    // tidiness.** `payload_cost` charges this buffer's `len()` against
    // `CoverSettings::budget`, while the allocator charges its `capacity()`.
    // Grown chunk by chunk from an empty `Vec`, that capacity is geometric —
    // up to nearly twice the length — so an untrimmed buffer makes the budget
    // understate what the process actually holds by up to about 100 %. At the
    // 256 MiB ceiling of `COVER_CACHE_BUDGET_MIO` that is the difference
    // between a quarter and a half of a 1 GiB Pi's memory. The copy costs one
    // pass over an image that has just crossed the network.
    bytes.shrink_to_fit();
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
///
/// **`Embedded` costs nothing here.** By the time a `CoverSource::Embedded`
/// reaches this function, `player::mpv::embedded_cover` has already probed
/// the container and found a picture — that is precisely what let it exist.
/// There is therefore no IO left to attempt, unlike `Ref(CoverRef::Path)`
/// just below, which still has to open the file once to check its header:
/// the probe already read the picture itself to compute its `content`
/// fingerprint, and the picture is read a second time, on demand, only if a
/// consumer actually asks for the bytes (`CoverCache::bytes`, `cover_get`).
///
/// `download_max` is `CoverSettings::download_max`, in bytes: the caller's
/// job, not this function's, to read the current setting — `fetch` runs
/// detached (`Core::start_cover_fetch`), well away from any lock on the
/// settings.
///
/// **`thumb` inverts what gets downloaded, and that is the whole point of the
/// pair.** When the contributor supplied a ready-made thumbnail, it is *that*
/// one that crosses the network here, and the full size is merely copied into
/// the payload as a reference. This is what lets a 2.5 MiB original be
/// announced at all: under `download_max` it would simply be refused, which
/// is why `musicbrainz` used to announce `front-500` as if it were the cover
/// itself. The full size costs exactly a string until somebody enlarges the
/// cover, at which point `cover_get` fetches it under `source_max` and
/// memoises it in the payload (`CoverPayload::Pair`'s `fetched`) — a cost
/// paid on a gesture, never on an announcement.
pub async fn fetch(
    s: &CoverSource,
    thumb: Option<&CoverRef>,
    download_max: usize,
) -> Option<CoverPayload> {
    let r = match s {
        CoverSource::Embedded { audio, .. } => return Some(CoverPayload::Embedded(audio.clone())),
        CoverSource::Ref(r) => r,
    };
    // A supplied thumbnail short-circuits the full size entirely: the full
    // size is not touched, not even to read a header.
    //
    // **The pair only forms around bytes, and only over a remote full size.**
    // Two exclusions, neither of them an oversight:
    //
    // * a thumbnail that materializes as a path (`CoverRef::Path`) is not
    //   paired: a local file already costs a path and nothing else, so there
    //   would be nothing to gain and a half of the pair would have to hold
    //   something other than bytes;
    // * a **local full size** is not paired either. It too costs a path, it
    //   is streamed rather than held, and `cover_get`'s bare URL serves a
    //   pair `immutable` for a year under `"{key}"` — a promise the local
    //   branch deliberately refuses to make, since a file on a share can
    //   change under the appliance, which is why that branch stamps its
    //   answers with the modification date instead. The pair would buy such
    //   a cover nothing and would make the route lie about it. Judged
    //   **before** the thumbnail is fetched, so nothing crosses the network
    //   for a pair that cannot form.
    if let Some(t) = thumb
        && matches!(r, CoverRef::Url { .. })
    {
        match fetch_ref(t, download_max).await {
            Some(CoverPayload::Bytes(thumb, thumb_mime)) => {
                // `fetched` starts empty, and that is the whole economy of
                // the pair: the full size is a reference until a reader
                // enlarges it, at which point `cover_get` fills this in.
                return Some(CoverPayload::Pair {
                    thumb,
                    thumb_mime,
                    full: r.clone(),
                    fetched: None,
                });
            }
            other => {
                // **A remote thumbnail that failed is not a reason to try the
                // full size.** Both halves come from the same host, through
                // the same door (`fetch_ref`), under the same cap, and the
                // full size is the heavier of the two — 2.67 MiB against
                // 73 KiB for the one shipped contributor. `download` reads
                // chunk by chunk and ignores the announced `Content-Length`
                // on purpose, so the refusal is only reached after the whole
                // `download_max` has been pulled from a third party and
                // thrown away, once per track played. A refusal on the small
                // image is a refusal on the big one; asking is pure cost.
                //
                // A **local** thumbnail that yielded no bytes still falls
                // through: it cost no network, and the full size beside it is
                // the answer this function would have given without any
                // thumbnail at all.
                if matches!(t, CoverRef::Url { .. }) {
                    tracing::debug!(
                        "supplied thumbnail unavailable; the full size is not tried after it"
                    );
                    return None;
                }
                tracing::debug!(
                    "supplied thumbnail yielded no bytes ({}), falling back to the full size",
                    if other.is_some() { "not a network image" } else { "unavailable" }
                );
            }
        }
    }
    fetch_ref(r, download_max).await
}

/// One reference, fetched. Split out of [`fetch`] so that a supplied
/// thumbnail goes through exactly the same door as a full size — the same
/// timeout on a share, the same `allowed_target`, the same download cap.
/// Written twice, the two halves of a pair would eventually be fetched under
/// different rules.
async fn fetch_ref(r: &CoverRef, download_max: usize) -> Option<CoverPayload> {
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
            download_ref(url, download_max).await
        }
    }
}

/// The body of a remote reference, in the shipped binary.
///
/// **A seam of exactly one call, for the reason written on
/// `CoverCache::fetch_url`**: `allowed_target` refuses every literal IP
/// address by design, so the local HTTP server the `download` tests use is
/// unreachable through a `CoverRef::Url`, and a hostname would mean a suite
/// depending on somebody else's server being up. Splitting this call out is
/// what lets a test prove which half of a `Pair` holds which reference
/// without a socket ever being opened — everything above it, `allowed_target`
/// included, stays the real code.
#[cfg(not(test))]
async fn download_ref(url: &str, download_max: usize) -> Option<CoverPayload> {
    download(url, download_max).await
}

/// The body of a remote reference, under test: the canned one, or nothing.
///
/// **The network stays unreachable by construction, not behind a flag.**
/// Absent a canned body the answer is `None` — exactly what every test
/// written before this seam existed already got from a URL it could not
/// reach, which is why adding it changes none of them.
#[cfg(test)]
async fn download_ref(url: &str, download_max: usize) -> Option<CoverPayload> {
    tests::canned_download(url, download_max)
}

/// ETag of a local file: unlike the cache key — which hashes the **source**
/// (the path), not the content — this file remains modifiable afterwards on
/// its share. The ETag must therefore follow the content, not just the path,
/// otherwise a conditional request would validate forever an image the user
/// has in fact replaced.
///
/// **Derived from the stamp, and no longer the other way round.** This
/// function used to be the one place where "has this source changed?" was
/// written down, and the cache borrowed its answer — a quoted HTTP validator
/// ending up as part of a key in the core's memory. The core now stamps its
/// own sources (`SourceStamp`) and HTTP dresses that stamp in its own
/// vocabulary here, which is the direction that lets the display path share
/// the answer without knowing what an ETag is.
fn file_etag(stamp: &SourceStamp) -> String {
    format!("\"{}\"", stamp.tag())
}

/// The validator for one **variant** of a stamped source: the thumbnail and
/// the original are the same content but not the same served bytes, and two
/// different responses under a single validator would make a browser serve one
/// for the other.
///
/// A function rather than the two inline `format!`s it replaces, because the
/// `ETag` of a `200` is no longer always derived from the same stamp as the
/// `304`'s: the conditional answer uses the route's own stat, while a body
/// obtained from a rendezvous uses the stamp of the read that produced it (see
/// `RenditionInFlight`). Two derivations spelt out twice each would have
/// drifted.
fn variant_etag(stamp: &SourceStamp, thumbnail: bool, rules: Option<Rendition>) -> String {
    let base = file_etag(stamp);
    if thumbnail {
        format!("\"v-{}-{}\"", rules_tag(rules), base.trim_matches('"'))
    } else {
        base
    }
}

/// What the rendition rules contribute to a **validator**.
///
/// **A thumbnail's validator must follow the settings that produced it**, and
/// that it did not was a defect this worksite could only make worse. The
/// `ETag` encoded the source and never the rules, while the core's own memory
/// of a thumbnail has included `Rendition::tag` all along: lower the longest
/// edge from 640 to 320 px and the core re-renders at once, but the browser
/// revalidates against an unchanged validator, is told `304`, and goes on
/// displaying the 640. Now that the page predicts the weight a setting will
/// produce, the owner has every reason to touch those settings — and nothing
/// would have moved on screen.
///
/// **Re-encoding unchecked gets a representation of its own** rather than an
/// empty one: unchecking is itself a change of what the route answers with
/// (the source, untouched), so an absent tag would make that one change
/// invisible to every browser holding a thumbnail. `raw` cannot collide with
/// a `Rendition::tag`, which is digits and dashes only.
fn rules_tag(rules: Option<Rendition>) -> String {
    match rules {
        Some(r) => r.tag(),
        None => "raw".to_string(),
    }
}

/// How a **frozen** cover — a network body checked in full, unable to change
/// behind a key that hashes the URL it came from — is cached and labelled,
/// for the size that was asked for.
///
/// **Two answers, because the two URLs do not determine their content
/// equally.** The bare URL names the source as it arrived: nothing the owner
/// can change alters those bytes, so a year of `immutable` is exactly right
/// and the key alone identifies them.
///
/// `?size=thumbnail` names *a rendering of* that source, and what it answers
/// with depends on settings the owner can change from the admin page. Both
/// halves therefore differ there:
///
/// * the validator carries the rules (`rules_tag`), for the reason written
///   out on that function;
/// * and it is `no-cache`, as the `File` and `Embedded` thumbnails already
///   were. A validator that changes is worth nothing to a browser that has
///   been told not to ask again for a year — `immutable` on a rendering was
///   the same defect in its severest form, the one with no recourse at all
///   from the server side. What this costs is one conditional request
///   answered by a bodiless `304`, and it is reversible; a year of
///   `immutable` on content that has since moved is not. Between two
///   imperfect headers, take the reversible one.
///
/// The alternative — keeping `immutable` and making the rules part of the
/// **URL** — is the better shape in principle and out of reach here: that URL
/// is `cover_href`, published in the protocol and appended to by the web app,
/// so it would take a protocol change to settle a header question.
fn frozen_headers(key: &str, thumbnail: bool, rules: Option<Rendition>) -> (String, String) {
    if thumbnail {
        ("no-cache".to_string(), format!("\"{key}-v-{}\"", rules_tag(rules)))
    } else {
        (crate::web::IMMUTABLE_CACHE_CONTROL.to_string(), format!("\"{key}\""))
    }
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
        // is a broken promise, not an ordinary case. The cache is bounded by
        // `CoverSettings::budget`, so the suspect is eviction — this is the
        // line that will say so, where the screen only showed a ♫ with no
        // explanation and the owner reported "no warn at all".
        tracing::warn!("cover {key} requested but no longer in the cache (evicted?)");
        return (StatusCode::NOT_FOUND, "inconnue").into_response();
    };
    match p {
        CoverPayload::Bytes(bytes, mime) => {
            // A network cover is frozen under its key: its validator has
            // nothing to carry beyond the key, the requested size, and — for
            // a thumbnail alone — the rules that produced it. See
            // `frozen_headers`, which also holds the reason a rendering is
            // not served `immutable`.
            //
            // **One read of the settings for both**, as `line` does: the
            // rules that go into the validator and the rules the verdict
            // below is reached under must not straddle a change.
            let rules = state.covers.settings().rendition;
            let (cache_control, etag) = frozen_headers(&key, thumbnail_requested, rules);
            // **Ahead of both sizes now**, where this check used to guard the
            // thumbnail alone: the bare URL of a frozen cover answered a full
            // body to a browser that already held it and said so. The
            // validator is a pure function of the key, the size and the
            // rules, so nothing has to be read to answer.
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok())
                == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            if thumbnail_requested {
                // The stamp is dropped rather than used: a network cover
                // is `Frozen`, so every caller's stamp is the same one and no
                // skew is expressible here — the `ETag` is the key.
                if let Some((mime, small, _)) =
                    state.covers.rendition_for(&key, Some(SourceStamp::Frozen)).await
                {
                    return (
                        [
                            (header::CONTENT_TYPE, mime.to_string()),
                            (header::CACHE_CONTROL, cache_control),
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
                //
                // **Still labelled as the thumbnail variant**, not as the
                // bare URL: the answer to this URL is a function of the
                // frozen source and the current rules, whichever branch
                // produced it, so one validator describes it exactly — and
                // the day the rules change, this fall-back revalidates like
                // any other.
            }
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, cache_control),
                    (header::ETAG, etag),
                ],
                bytes,
            )
                .into_response()
        }
        CoverPayload::Pair { thumb, thumb_mime, full, fetched } => {
            // Frozen under its key, exactly like `Bytes`: both halves came
            // from a network body already fully checked, and neither can
            // change behind this key. **One read of the settings for the
            // whole arm**: the rules that go into a validator, the rules the
            // acceptance verdict is reached under and the ceiling a download
            // obeys must not straddle a change.
            let settings = state.covers.settings();
            if thumbnail_requested {
                let (cache_control, etag) = frozen_headers(&key, true, settings.rendition);
                if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok())
                    == Some(etag.as_str())
                {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                // **The acceptance rule is the threshold rule, and that
                // convergence is the point of the pair.** Not a second
                // spelling of it either: `Rendition::leaves_alone` is the
                // one place the rule lives, and `rendition` asks it the same
                // question about any cover at all. A supplied thumbnail that
                // satisfies it would come out of the encoder byte-identical
                // at best — and re-encoded, measurably not identical at all.
                //
                // Judged **before** `rendition_for` rather than inside it,
                // and that is what the test's `renditions_built() == 0`
                // pins: `rendition` reaches the same verdict, but only after
                // a rendezvous, a lookup and a counted build. Here it costs
                // one header read.
                //
                // Re-encoding disabled (`rendition` at `None`) accepts the
                // thumbnail too: nothing is being produced to compare it
                // against, and the setting says push the source as it is.
                let within_the_rule = match settings.rendition {
                    None => true,
                    Some(r) => dimensions(&thumb)
                        .is_some_and(|d| r.leaves_alone(thumb.len(), d)),
                };
                if !within_the_rule
                    && let Some((mime, small, _)) =
                        state.covers.rendition_for(&key, Some(SourceStamp::Frozen)).await
                {
                    return (
                        [
                            (header::CONTENT_TYPE, mime.to_string()),
                            (header::CACHE_CONTROL, cache_control),
                            (header::ETAG, etag),
                        ],
                        small.as_slice().to_vec(),
                    )
                        .into_response();
                }
                // Either within the rule — served untouched, which is what
                // the pair exists for — or out of it with nothing produced
                // (unreadable image, dimensions beyond the cap). In both
                // cases the supplied thumbnail is the answer, for the same
                // reason as the `Bytes` arm's fall-through: better an
                // oversized image than no image. And in both cases it is the
                // rendering of this frozen source under these rules, hence
                // the one validator.
                return (
                    [
                        (header::CONTENT_TYPE, thumb_mime.to_string()),
                        (header::CACHE_CONTROL, cache_control),
                        (header::ETAG, etag),
                    ],
                    thumb,
                )
                    .into_response();
            }
            // **The bare URL means the full size, and now it fetches one.**
            // Until this task nothing on this path ever downloaded: the route
            // read a cache the announcement path had filled, and this branch
            // served the supplied thumbnail as a stand-in. What made the
            // change affordable is that it is not automatic — a 2.5 MiB
            // original is fetched when a reader enlarges the cover, and never
            // on the off chance that one might.
            //
            // **It takes `"{key}"`, the validator the `Bytes` arm gives real
            // network bytes**, which the previous task deliberately kept free
            // by giving the stand-in a tag of its own. Any browser still
            // holding a stand-in therefore mismatches on its own and is
            // served a `200` — no decision left unwritten, no stand-in frozen
            // on a screen.
            let (cache_control, etag) = frozen_headers(&key, false, settings.rendition);
            // **Answered before anything is fetched, and that is the point.**
            // These bytes are frozen under this key: a browser holding this
            // validator holds the right image, whether or not this process
            // still has it. So a revalidation — after a restart, after an
            // eviction — costs a `304` and never a second request to a third
            // party.
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok())
                == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            // The memo first, the network second. See `CoverCache::full_size`
            // for why both a memo and a rendezvous are needed: one bounds the
            // readers who arrive together, the other those who come one after
            // another.
            let full_size = match fetched {
                // A refcount bump and no copy, on both halves: the payload's
                // clone shared this buffer (see `CoverPayload::Pair`), and
                // `shared_body` builds the response over it rather than over
                // a copy of it.
                Some((bytes, mime)) => Some((mime, shared_body(bytes))),
                None => state.covers.full_size(&key, &full, settings.source_max).await,
            };
            if let Some((mime, bytes)) = full_size {
                return (
                    [
                        (header::CONTENT_TYPE, mime.to_string()),
                        (header::CACHE_CONTROL, cache_control),
                        (header::ETAG, etag),
                    ],
                    bytes,
                )
                    .into_response();
            }
            // **The download failed: serve the thumbnail, not a 404.** The
            // reader asked to see the image larger; a smaller version is an
            // honest answer where an empty square is not.
            //
            // **A validator of its own, and never `immutable`.** This branch
            // answers with something other than what its URL names, so the
            // rule is by *response* and not by route: the same URL will serve
            // the real full size as soon as the network comes back, and a
            // year of `immutable` would pin this browser to the stand-in for
            // that year with no recourse from the server side. `no-cache`
            // guarantees the browser asks again; the distinct `ETag` is what
            // makes the answer to that question correct — under `"{key}"` the
            // revalidation would compare equal, earn a `304`, and leave the
            // stand-in on screen for ever.
            //
            // **Distinct from the thumbnail branch's tag too**, not merely
            // from `"{key}"`. Within the rule the two branches do serve the
            // same bytes, which is what makes sharing tempting — but outside
            // it they do not: `?size=thumbnail` then answers with the
            // re-encoded rendition while this branch still answers with the
            // raw supplied thumbnail. One validator over two bodies is
            // exactly the fault the `File` arm's comment warns about.
            //
            // **Reported where the failure is known, not here.** A
            // `journalctl` that stayed silent would make the enlarged view's
            // softness look like a rendition defect, so it is said — but by
            // `CoverCache::report_unfetchable`, which alone can say it at
            // most once per key. Repeated here, a client looping on a broken
            // target would write the journal at its own cadence, on a route
            // that needs no authentication.
            let etag = format!("\"{key}-standin\"");
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok())
                == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            (
                [
                    (header::CONTENT_TYPE, thumb_mime.to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                    (header::ETAG, etag),
                ],
                thumb,
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
            // one for the other. **Nor is it the same across settings**: a
            // thumbnail is a rendering, so the rules that produced it belong
            // in its validator — see `rules_tag`. Read once for the whole
            // arm, so that the label and the rendition below cannot straddle
            // a change.
            let rules = state.covers.settings().rendition;
            let stamp = SourceStamp::of_file(&meta);
            let etag = variant_etag(&stamp, thumbnail_requested, rules);
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            // **After the 304 and not before**: a conditional request must
            // build nothing, and that is what makes the thumbnail cheap in
            // steady state. The stamp is handed over rather than left to be
            // rediscovered — this route has just opened the file and read its
            // metadata, so a memorized rendition is served **without touching
            // the share again**. And because the stamp is part of the
            // identity, a `folder.jpg` replaced on the share is a different
            // identity, and its old rendition is never served again.
            if thumbnail_requested
                && let Some((mime, small, served)) =
                    state.covers.rendition_for(&key, Some(stamp)).await
            {
                return (
                    [
                        (header::CONTENT_TYPE, mime.to_string()),
                        (header::CACHE_CONTROL, "no-cache".to_string()),
                        // **`served`, not `stamp`.** A caller that missed the
                        // cache and joined a rendezvous already under way gets
                        // a picture read before its own stat, and labelling it
                        // with `stamp` would pin those older bytes under a
                        // validator that keeps matching for the life of that
                        // browser's cache. `served` makes the label describe
                        // the bytes; the next request stats afresh,
                        // mismatches, and is served the current image. See
                        // `RenditionInFlight`.
                        (header::ETAG, variant_etag(&served, true, rules)),
                    ],
                    small.as_slice().to_vec(),
                )
                    .into_response();
            }
            // Falling through means either no thumbnail was asked for, or
            // there was nothing to shrink. Either way we stream the original
            // below, with `stamp`'s ETag — rightly: what gets streamed comes
            // from the descriptor this route opened and stat'ed itself, with
            // no rendezvous in between, so the label describes those bytes.
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
        CoverPayload::Embedded(audio) => {
            // **One bounding mechanism, shared with `File`**: `FILE_TIMEOUT`,
            // not `Health` — see the doc of `FILE_TIMEOUT` for why a circuit
            // breaker does not fit an already-async path.
            //
            // A bare `metadata`, deliberately **not** an open file kept
            // around: unlike `File`, nothing downstream reuses a descriptor —
            // a container is not a stream of image bytes, so serving a body
            // means extracting through `read_embedded_bounded` regardless,
            // which stats the file **again** on its own. Stat-only here is
            // what keeps a conditional request from ever touching `lofty`.
            //
            // That second stat is not a cost merely tolerated for the sake of
            // this route's simplicity — it must stay. Passing this route's
            // own stamp down instead would mean filing the rendition, once
            // read, under the *caller's* stamp rather than the one describing
            // the bytes actually read: exactly the hazard `rendition_for`'s
            // comment on "the identity comes from the read, never from the
            // caller's stamp" refuses. An identity must come from the read
            // that produced the bytes, not from a stat taken earlier by
            // someone else.
            let stat = tokio::time::timeout(FILE_TIMEOUT, tokio::fs::metadata(&audio)).await;
            let meta = match stat {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    tracing::warn!("cover {key} unreadable: {e}");
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
                Err(_) => {
                    tracing::warn!(
                        "cover file {} did not answer in {FILE_TIMEOUT:?}",
                        audio.display()
                    );
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
            };
            // Read once for the whole arm, as in the `File` arm above and
            // for the same reason: a thumbnail is a rendering, and the rules
            // that produced it belong in its validator (`rules_tag`).
            let rules = state.covers.settings().rendition;
            let stamp = SourceStamp::of_file(&meta);
            // **The conditional answer is derived from this route's own stat,
            // and that must not change.** It is the only stamp available
            // before a byte is read, and comparing it against what the browser
            // holds is exactly what keeps a `304` free of any `lofty` probe.
            // What follows below is the other half: a `200` is labelled from
            // the stamp of the read that produced its bytes, which is not
            // necessarily this one.
            let etag = variant_etag(&stamp, thumbnail_requested, rules);
            // **Before any parsing of the container**: this is the whole
            // point of stamping from `metadata` rather than from the picture
            // itself — a conditional request costs one `stat`, exactly like
            // `File`, never a `lofty` probe.
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            if thumbnail_requested
                && let Some((mime, small, served)) =
                    state.covers.rendition_for(&key, Some(stamp)).await
            {
                return (
                    [
                        (header::CONTENT_TYPE, mime.to_string()),
                        (header::CACHE_CONTROL, "no-cache".to_string()),
                        // `served`, not `stamp` — same reasoning as the
                        // `File` arm above.
                        (header::ETAG, variant_etag(&served, true, rules)),
                    ],
                    small.as_slice().to_vec(),
                )
                    .into_response();
            }
            // Falling through means no thumbnail was asked for, or there was
            // nothing to shrink: the original is served below — same
            // reasoning as `File`.
            // Only reached once a body is actually needed: the container is
            // parsed here, and only here — never to answer a 304.
            //
            // **Through `extract_embedded`, not `read_embedded_bounded`
            // directly.** This bare URL is the enlarged view of
            // `PlayerCard.vue`, unauthenticated on the LAN: nothing upstream
            // of this branch deduplicates concurrent callers the way
            // `rendition_for`/`renditions_in_flight` do for the thumbnail
            // branch just above, so N browsers enlarging the same cover at
            // once used to run N independent `lofty` parses. See
            // `CoverCache::embedded_in_flight` for the full account.
            let cap = state.covers.settings().source_max;
            let Some((mime, bytes, served)) = state.covers.extract_embedded(&key, &audio, cap).await
            else {
                tracing::warn!("cover {key} unreadable: {}", audio.display());
                return (StatusCode::NOT_FOUND, "illisible").into_response();
            };
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                    // **`served`, not the `etag` computed from this route's
                    // stat.** The rendezvous above may hand back a picture
                    // another caller extracted *before* the file was retagged;
                    // labelling it with this route's newer stamp would make
                    // that browser cache the wrong image under a validator
                    // that keeps matching — `no-cache` revalidates, the stat
                    // still yields the newer stamp, the `304` returns the same
                    // stale bytes, and nothing ever corrects it. See
                    // `EmbeddedInFlight`.
                    (header::ETAG, variant_etag(&served, thumbnail_requested, rules)),
                ],
                // **No clone here, unlike `line`'s `Some(_)` branch.** `bytes`
                // is `axum::body::Bytes`, not `Arc<Vec<u8>>`: it is what
                // `Body` is built from directly, and cloning it is a
                // refcount bump. N concurrent callers sharing one
                // `extract_embedded` result therefore share one allocation
                // all the way to their sockets — see `EmbeddedInFlight`'s
                // doc for why this path can afford that and `line`'s
                // rendition branch cannot (it must base64-encode into an
                // owned buffer regardless).
                bytes,
            )
                .into_response()
        }
    }
}

/// `GET /api/cover-cache`. Read-only, and read **on demand**: the page fetches
/// it when its panel opens and when the reader asks again, never on a timer.
/// A periodic refresh would repeat the fault measured on the MPD side, where
/// the server woke its clients once a second for nothing.
pub async fn cache_json(
    State(state): State<crate::status::AppState>,
) -> axum::Json<CacheSnapshot> {
    axum::Json(state.covers.snapshot().await)
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
        let a = CoverSource::Ref(CoverRef::Url { url: "https://x.org/a.jpg".into() });
        let b = CoverSource::Ref(CoverRef::Url { url: "https://x.org/b.jpg".into() });
        assert_eq!(key(&a), key(&a), "the key must be stable: it is published in a URL");
        assert_ne!(key(&a), key(&b));
        // A different form for the same string must not collide.
        assert_ne!(
            key(&a),
            key(&CoverSource::Ref(CoverRef::Path { path: "/https://x.org/a.jpg".into() }))
        );
        // Hexadecimal, so no surprises inside a URL path.
        assert!(key(&a).chars().all(|c| c.is_ascii_hexdigit()), "{}", key(&a));
    }

    #[test]
    fn an_embedded_key_never_collides_with_a_ref_carrying_the_same_string() {
        // The discriminant byte is what makes this hold: without it, an
        // embedded cover whose `content` happens to equal some `Path`'s
        // string would collide and serve the wrong image.
        let embedded =
            CoverSource::Embedded { audio: PathBuf::from("/a.mp3"), content: "same".into() };
        let as_path = CoverSource::Ref(CoverRef::Path { path: "same".into() });
        assert_ne!(key(&embedded), key(&as_path));
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

    /// **A snapshot that counts bytes must be checked against known bytes**,
    /// not against itself. The payloads are therefore laid down by hand, with
    /// distinct, non-round sizes so that a wrong sum could not land right by
    /// accident.
    #[tokio::test]
    async fn the_snapshot_counts_the_bytes_it_actually_holds() {
        let cache = CoverCache::new();
        cache.insert("net".into(), CoverPayload::Bytes(vec![0u8; 7_777], "image/jpeg")).await;
        cache.insert("file".into(), CoverPayload::File("/music/a/cover.jpg".into())).await;
        cache.insert("tags".into(), CoverPayload::Embedded("/music/a/01.flac".into())).await;

        let s = cache.snapshot().await;
        assert_eq!(s.entries, 3);
        assert_eq!(s.entries_free, 2, "a File and an Embedded cost a path, not bytes");
        assert_eq!(s.used_bytes, 7_777, "only the network payload weighs anything");
        assert_eq!(s.renditions, 0);
        assert_eq!(s.renditions_bytes, 0);
        assert_eq!(s.max_entries, MAX_ENTRIES);
        assert_eq!(s.budget_bytes, cache.settings().budget);
    }

    /// The panel's most important line: the real average weight of a
    /// thumbnail, to be set against the predicted weight the page announces.
    /// The division is done by the page — the core renders the total and the
    /// count — so it is their accuracy that this test checks.
    #[tokio::test]
    async fn the_snapshot_reports_the_real_weight_of_retained_thumbnails() {
        let cache = CoverCache::new();
        let rules = cache.settings().rendition.expect("the product default re-encodes");
        let stamp = SourceStamp::Frozen;
        for (i, size) in [40_000usize, 60_000, 110_000].iter().enumerate() {
            let identity = rendition_identity(&format!("k{i}"), &stamp, &rules);
            cache
                .remember_rendition(identity, "image/jpeg", Arc::new(vec![0u8; *size]))
                .await;
        }

        let s = cache.snapshot().await;
        assert_eq!(s.renditions, 3);
        assert_eq!(s.renditions_bytes, 210_000);
        assert_eq!(s.renditions_stale, 0, "all three were produced under the live rules");
        assert_eq!(s.used_bytes, 210_000, "renditions are charged to the budget too");
    }

    /// Stale, not merely old. The production change this would catch:
    /// counting thumbnails without checking them against the live rules,
    /// which would report as useful what `evict_to_budget` will discard
    /// first.
    #[tokio::test]
    async fn the_snapshot_tells_stale_thumbnails_apart() {
        let cache = CoverCache::new();
        let stamp = SourceStamp::Frozen;
        let old = Rendition {
            max_edge_px: 320,
            jpeg_quality: 85,
            passthrough_max: 150 * 1024,
            pixel_cap: 16_000_000,
        };
        cache
            .remember_rendition(
                rendition_identity("k", &stamp, &old),
                "image/jpeg",
                Arc::new(vec![0u8; 1_000]),
            )
            .await;
        // The live rules are the product's default (640 px), so the identity
        // above no longer describes them.
        let s = cache.snapshot().await;
        assert_eq!(s.renditions, 1);
        assert_eq!(s.renditions_stale, 1, "produced under 320 px, the cache now asks 640");
    }

    async fn served_cache_json(cache: &Arc<CoverCache>) -> (u16, Vec<u8>) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let app = crate::status::router(crate::status::AppState {
            covers: cache.clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get("/api/cover-cache").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn the_cache_route_serves_the_snapshot_as_json() {
        let cache = Arc::new(CoverCache::new());
        cache.insert("net".into(), CoverPayload::Bytes(vec![0u8; 4_096], "image/jpeg")).await;
        let (status, body) = served_cache_json(&cache).await;
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["entries"], 1);
        assert_eq!(v["used_bytes"], 4_096);
        // The field names are a contract with the page: renaming one without
        // touching `CachePayload` on the web side would break the panel
        // silently, since TypeScript sees nothing of a JSON body.
        assert!(v["budget_bytes"].is_number());
        assert!(v["max_entries"].is_number());
    }

    /// The same request as `served_body`, plus the validator the route puts on
    /// its answer.
    ///
    /// Separate rather than folded into `served_body`: the `ETag` only
    /// matters to the handful of tests below that assert *which* stamp a
    /// response was labelled from, and threading a third element through the
    /// twenty existing call sites would have obscured them for nothing.
    async fn served_with_etag(
        cache: &Arc<CoverCache>,
        key: &str,
        query: &str,
    ) -> (u16, String, Vec<u8>) {
        let (status, etag, _, body) = served_with_headers(cache, key, query).await;
        (status, etag, body)
    }

    /// The same request, returning **both** headers a cached response is
    /// judged by: `(status, etag, cache-control, body)`.
    ///
    /// The two are read together because they answer one question between
    /// them and neither is sufficient alone: a validator that changes buys
    /// nothing from a browser told not to ask again for a year, and a
    /// revalidation buys nothing if the validator cannot tell two bodies
    /// apart.
    async fn served_with_headers(
        cache: &Arc<CoverCache>,
        key: &str,
        query: &str,
    ) -> (u16, String, String, Vec<u8>) {
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
        let header_of = |name: header::HeaderName| {
            resp.headers().get(name).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
        };
        let etag = header_of(header::ETAG);
        let cache_control = header_of(header::CACHE_CONTROL);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, etag, cache_control, bytes.to_vec())
    }

    /// The same request, made **conditional** on `etag`. Returns the status
    /// alone: `304` or `200` is the entire question a validator answers.
    async fn served_if_none_match(
        cache: &Arc<CoverCache>,
        key: &str,
        query: &str,
        etag: &str,
    ) -> u16 {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let app = crate::status::router(crate::status::AppState {
            covers: cache.clone(),
            ..crate::status::tests_support::app_state()
        });
        app.oneshot(
            Request::get(format!("/api/cover/{key}{query}"))
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
        .as_u16()
    }

    // -- The stamp a response is labelled with ------------------------------

    #[tokio::test]
    async fn a_picture_from_the_rendezvous_is_labelled_with_its_own_stamp() {
        // **The one skew that never revalidates away.** `cover_get` stats the
        // audio file, derives an `ETag` from that stat, and only then joins
        // `embedded_in_flight` — where it can collect a picture another caller
        // pulled out of the container *before* the file was retagged. Labelled
        // with this route's newer stamp, those older bytes are pinned in that
        // browser for good: the response is `no-cache`, so the browser
        // revalidates, the stat still yields the same newer stamp, the `304`
        // hands back the same stale bytes, and nothing ever corrects it.
        //
        // Named production change this guards: labelling the `200` with the
        // route's own `etag` (the variable the `304` compares against) instead
        // of with `variant_etag(&served, …)`.
        //
        // **The rendezvous is seeded rather than raced.** A test that spawned
        // two callers and hoped one suspended inside `lofty` at the right
        // instant would be measuring the machine's load, not the code — the
        // flake class this project has already paid for twice. Placing the
        // cell by hand produces the *result* of that race, deterministically.
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("track.mp3");
        std::fs::write(&audio, b"a container, as it was when caller A read it").unwrap();
        let old = SourceStamp::of_file(&std::fs::metadata(&audio).unwrap());

        let cache = Arc::new(CoverCache::new());
        cache.insert("k".into(), CoverPayload::Embedded(audio.clone())).await;
        let cell: EmbeddedInFlight = Arc::new(tokio::sync::OnceCell::new());
        cell.set(Some(("image/jpeg", axum::body::Bytes::from_static(b"PICTURE-A"), old)))
            .expect("a fresh cell");
        cache.embedded_in_flight.lock().await.insert("k".to_string(), cell);

        // The retag. **Two different lengths on purpose**: the stamp is
        // modification date *and* size, and on Windows the clock advances only
        // about every 15 ms — same reasoning as
        // `the_route_stops_serving_the_thumbnail_of_a_replaced_file`.
        std::fs::write(&audio, b"the same container after the owner retagged the album").unwrap();
        let new = SourceStamp::of_file(&std::fs::metadata(&audio).unwrap());
        assert_ne!(old, new, "the two writes must produce different stamps");

        let (status, etag, body) = served_with_etag(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, b"PICTURE-A", "the rendezvous is what served this body");
        assert_eq!(
            etag,
            file_etag(&old),
            "the label must describe the bytes actually served, not the stat taken after them"
        );
        assert_ne!(etag, file_etag(&new), "labelling from the route's own stat is the defect");
    }

    #[tokio::test]
    async fn a_thumbnail_from_the_rendezvous_is_labelled_with_its_own_stamp() {
        // The same skew one stage up, on `renditions_in_flight`, and it
        // pre-dates the memory-budget work: a caller that misses the rendition
        // cache registers *after* its own stat and can collect a thumbnail
        // built from an earlier read. Same seeded-cell technique, same
        // permanence, same fix.
        //
        // Named production change this guards: putting `etag` (built from this
        // route's stat) back on the thumbnail `200` in place of
        // `variant_etag(&served, true)`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(64, 64)).unwrap();
        let old = SourceStamp::of_file(&std::fs::metadata(&path).unwrap());

        let cache = Arc::new(CoverCache::new());
        cache.insert("k".into(), CoverPayload::File(path.clone())).await;
        let cell: RenditionInFlight = Arc::new(tokio::sync::OnceCell::new());
        cell.set(Some(("image/jpeg", Arc::new(b"THUMB-A".to_vec()), old))).expect("a fresh cell");
        cache.renditions_in_flight.lock().await.insert("k".to_string(), cell);

        std::fs::write(&path, fixtures::jpeg_decodable(96, 32)).unwrap();
        let new = SourceStamp::of_file(&std::fs::metadata(&path).unwrap());
        assert_ne!(old, new, "the two writes must produce different stamps");

        let (status, etag, body) = served_with_etag(&cache, "k", "?size=thumbnail").await;
        assert_eq!(status, 200);
        assert_eq!(body, b"THUMB-A", "the rendezvous is what served this body");
        let rules = cache.settings().rendition;
        assert_eq!(
            etag,
            variant_etag(&old, true, rules),
            "the label must describe the bytes served"
        );
        assert_ne!(
            etag,
            variant_etag(&new, true, rules),
            "labelling from the route's own stat is the defect"
        );
    }

    #[tokio::test]
    async fn the_display_path_extracts_through_the_same_rendezvous_as_the_route() {
        // **The deferred half of the rendezvous.** `CoverCache::bytes` called
        // `read_embedded_bounded` directly, so the socket side and the HTTP
        // side could each parse the same container at the same instant — the
        // very duplication `embedded_in_flight` exists to end, left open on
        // one of its two callers.
        //
        // Named production change this guards: `bytes`'s `OnDisk::Embedded`
        // arm going back to `self.read_embedded_bounded(&audio, cap)`.
        //
        // **The audio path does not exist**, and that is the whole proof: a
        // direct read could only fail, so a frame can come out at all only if
        // the rendezvous was consulted. No timing, no ffmpeg.
        let cache = Arc::new(CoverCache::new());
        // Re-encoding off so the seeded bytes reach the frame unchanged: this
        // test is about which door the read goes through, not about decoding.
        cache.set_cover_settings(CoverSettings { rendition: None, ..CoverSettings::default() });
        cache
            .insert("k".into(), CoverPayload::Embedded(PathBuf::from("/nowhere/never-opened.mp3")))
            .await;
        let cell: EmbeddedInFlight = Arc::new(tokio::sync::OnceCell::new());
        cell.set(Some((
            "image/jpeg",
            axum::body::Bytes::from_static(b"SHARED-PICTURE"),
            SourceStamp::File { modified_nanos: 1, size: 2 },
        )))
        .expect("a fresh cell");
        cache.embedded_in_flight.lock().await.insert("k".to_string(), cell);

        let line = cache.line("k", "/api/cover/k").await.expect("a frame must be produced");
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&line).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => assert_eq!(
                c.bytes,
                b"SHARED-PICTURE".to_vec(),
                "the push path must collect what the rendezvous already holds"
            ),
            other => panic!("expected a cover frame, got {other:?}"),
        }
        assert_eq!(
            cache.embedded_extractions(),
            0,
            "no container may have been parsed: the picture was there for the taking"
        );
    }

    #[tokio::test]
    async fn unchecking_re_encoding_frees_the_thumbnails_it_makes_useless() {
        // **The gesture is the fix's whole subject**: a user unchecks
        // "Re-encode covers" precisely to give a Pi its memory back. Every
        // retained thumbnail is waste from that instant and no new one will
        // ever be produced — but a NAS library's sources cost 0
        // (`payload_cost`), so the total stays comfortably under budget, and
        // the purge, sitting behind `evict_to_budget`'s `total <=
        // settings.budget` guard, never ran. The memory was held until the
        // process restarted. Any `cover_max_edge_px` change fell into the same
        // trap.
        //
        // Named production change this guards: moving step 1 back inside the
        // loop, after that guard. The budget below is deliberately generous —
        // 50 MiB against 30 KiB of thumbnails — so nothing but the
        // unconditional purge can free them.
        let cache = CoverCache::new();
        let rules = CoverSettings::default().rendition.expect("rendition on by default");
        cache.insert("src".into(), CoverPayload::File(PathBuf::from("/nas/a.jpg"))).await;
        let identities: Vec<String> = (0..3)
            .map(|i| rendition_identity(&format!("k{i}"), &SourceStamp::Frozen, &rules))
            .collect();
        for identity in &identities {
            cache
                .remember_rendition(identity.clone(), "image/jpeg", Arc::new(vec![0u8; 10 * 1024]))
                .await;
        }
        for identity in &identities {
            assert!(
                cache.cached_rendition(identity).await.is_some(),
                "the thumbnails must be there to begin with, or this test proves nothing"
            );
        }

        cache.set_cover_settings(CoverSettings { rendition: None, ..CoverSettings::default() });
        // The reconcile is lazy, as in service: `set_cover_settings` is
        // synchronous by construction and cannot take these locks, so the next
        // cache write is what runs it.
        cache.insert("trigger".into(), CoverPayload::File(PathBuf::from("/nas/b.jpg"))).await;

        for identity in &identities {
            assert!(
                cache.cached_rendition(identity).await.is_none(),
                "a thumbnail no rule can ask for again must not survive under a generous budget"
            );
        }
        assert!(cache.contains("src").await, "and nothing else may be evicted along the way");
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

    /// A paired payload: `thumb`'s bytes in hand, and a full-size reference
    /// nobody has downloaded yet.
    ///
    /// A helper rather than one literal per test, and the reason has now been
    /// collected: the variant gained a fourth field (`fetched`), and one
    /// helper was corrected where a dozen literals would each have had to be
    /// revisited.
    fn paired_entry(thumb: Vec<u8>) -> CoverPayload {
        paired_with(thumb, CoverRef::Url { url: "https://example.org/front.jpg".into() })
    }

    /// The same, with the full-size half named by the caller: a refused
    /// target, or a file on the share.
    fn paired_with(thumb: Vec<u8>, full: CoverRef) -> CoverPayload {
        CoverPayload::Pair {
            thumb,
            thumb_mime: "image/jpeg",
            full,
            // Nothing downloaded yet: the state every one of these tests
            // starts from, and the state an entry stays in until a reader
            // enlarges the cover.
            fetched: None,
        }
    }

    /// The body a canned download answers with — the "full size" of these
    /// tests. Visibly different from the thumbnails they pair it with, so
    /// that "which half did the route serve?" is answered by the bytes and
    /// not by their length alone.
    fn full_size_fixture() -> Vec<u8> {
        fixtures::jpeg_decodable(1200, 1200)
    }

    thread_local! {
        /// Bodies standing in for the network, by URL, for the lifetime of a
        /// [`CannedUrl`] guard. Read by [`super::download_ref`], which is
        /// the test binary's only substitution on the announcement path.
        ///
        /// **Thread-local rather than a module-level `static`, deliberately.**
        /// `#[tokio::test]` builds a current-thread runtime, so a test's own
        /// `fetch` runs on the very thread that installed the body, while a
        /// `static` would be shared with every sibling test running at the
        /// same instant — the class of leak this suite has already been bitten
        /// by. The guard removes the entry on drop, so a panicking test leaves
        /// nothing behind for the next one either.
        static CANNED_URLS: std::cell::RefCell<HashMap<String, Vec<u8>>> =
            std::cell::RefCell::new(HashMap::new());
    }

    /// Serves `bytes` for `url` until the returned guard drops.
    #[must_use]
    fn canned_url(url: &str, bytes: Vec<u8>) -> CannedUrl {
        CANNED_URLS.with(|m| m.borrow_mut().insert(url.to_string(), bytes));
        CannedUrl(url.to_string())
    }

    /// Lifetime of one canned body. Bind it — `let _net = canned_url(..)` —
    /// rather than dropping it on the spot, or the network goes away before
    /// the fetch under test reaches it.
    struct CannedUrl(String);

    impl Drop for CannedUrl {
        fn drop(&mut self) {
            CANNED_URLS.with(|m| m.borrow_mut().remove(&self.0));
        }
    }

    /// What [`super::download_ref`] answers with under test.
    ///
    /// **The cap and the magic bytes are applied here as `download` applies
    /// them**, so a body over the ceiling is still refused and a body that is
    /// not an image still yields nothing: only the socket is missing.
    pub(super) fn canned_download(url: &str, cap: usize) -> Option<CoverPayload> {
        let bytes = CANNED_URLS.with(|m| m.borrow().get(url).cloned())?;
        if bytes.len() > cap {
            return None;
        }
        let mime = image_type(&bytes)?;
        Some(CoverPayload::Bytes(bytes, mime))
    }

    /// **A supplied thumbnail that respects the rule is served byte for
    /// byte.** Binary identity is the assertion that counts: a
    /// decode/encode round trip would produce different bytes even at equal
    /// dimensions, so equality proves there was none.
    #[tokio::test]
    async fn a_supplied_thumbnail_within_the_rule_is_served_untouched() {
        let cache = Arc::new(CoverCache::new());
        // 500 px, like Cover Art Archive's `front-500`, under the 640 px of
        // the shipped default.
        let thumb = fixtures::jpeg_decodable(500, 500);
        assert!(
            thumb.len() <= 150 * 1024,
            "the fixture must sit under the default threshold for this test to mean anything"
        );
        cache.insert("k".into(), paired_entry(thumb.clone())).await;

        let (status, body) = served_body(&cache, "k", "?size=thumbnail").await;
        assert_eq!(status, 200);
        assert_eq!(body, thumb, "a supplied thumbnail within the rule must not be re-encoded");
        assert_eq!(cache.renditions_built(), 0, "and nothing must have been produced");
    }

    /// The other edge. A supplied thumbnail that is too heavy **is**
    /// re-encoded, and from itself — not from the full size, which has not
    /// even been downloaded.
    #[tokio::test]
    async fn a_supplied_thumbnail_outside_the_rule_is_re_encoded_from_itself() {
        let cache = Arc::new(CoverCache::new());
        // 900 px: beyond the 640 px edge, hence out of the rule by its
        // dimensions.
        let thumb = fixtures::jpeg_decodable(900, 900);
        cache.insert("k".into(), paired_entry(thumb.clone())).await;

        let (status, body) = served_body(&cache, "k", "?size=thumbnail").await;
        assert_eq!(status, 200);
        assert_ne!(body, thumb, "out of the rule, it must be re-encoded");
        assert_eq!(dimensions(&body), Some((640, 640)), "down to the configured edge");
        assert_eq!(cache.renditions_built(), 1);
    }

    /// **A failed download answers with the thumbnail, and never freezes
    /// it.** The reader asked to see the image larger; a smaller version is
    /// an honest answer where an empty square is not.
    ///
    /// The header is the half that needed pinning. This response says
    /// something other than what its URL names, and the same URL will serve
    /// the real full size the moment the target answers again — so a year of
    /// `immutable` would pin this browser to the stand-in for that year, with
    /// no recourse at all from the server side.
    ///
    /// Named production changes this guards: serving a `404` when the fetch
    /// fails; giving this response the `immutable` the real full size gets;
    /// or handing it `"{key}"`, under which a browser holding a stand-in
    /// would earn a `304` for ever.
    #[tokio::test]
    async fn a_failed_full_size_download_serves_the_thumbnail_without_freezing_it() {
        let cache = Arc::new(CoverCache::new());
        let thumb = fixtures::jpeg_decodable(500, 500);
        cache.insert("k".into(), paired_entry(thumb.clone())).await;
        // No canned body is installed, so the download yields nothing — see
        // `CoverCache::canned_full_download`. That is the failure this test
        // is about, obtained without a network and without a clock.

        let (status, etag, cache_control, body) = served_with_headers(&cache, "k", "").await;
        assert_eq!(status, 200, "a failed fetch must not turn into a 404");
        assert_eq!(body, thumb, "the thumbnail is the honest answer");
        assert_eq!(cache_control, "no-cache", "a stand-in must never be frozen for a year");
        assert_ne!(etag, "\"k\"", "nor may it claim the real full size's validator");
        assert_eq!(cache.full_downloads(), 1, "it did try, once");

        // **The failure is not memoised**, deliberately: the next click tries
        // again rather than serving the stand-in for ever from memory. The
        // cost of that choice is one failed request per click, which is what
        // this second call measures.
        let (status, _, _, again) = served_with_headers(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(again, thumb);
        assert_eq!(cache.full_downloads(), 2, "a broken target is retried, not remembered");
    }

    /// **Within the edge, over the threshold** — the half of the rule the two
    /// tests above cannot reach. One sits under both bounds, the other is out
    /// by its dimensions alone, so dropping `len <= passthrough_max` from
    /// `Rendition::leaves_alone` would survive both of them.
    ///
    /// The threshold is lowered rather than the fixture inflated: a fixture
    /// heavy enough to cross 150 KiB while staying under 640 px would rest on
    /// how well a gradient happens to compress, which is not a property to
    /// pin a rule on.
    #[tokio::test]
    async fn a_supplied_thumbnail_within_the_edge_but_over_the_threshold_is_re_encoded() {
        let cache = Arc::new(CoverCache::new());
        let rules = cache.settings().rendition.expect("the product default re-encodes");
        cache.set_cover_settings(CoverSettings {
            // 500 px stays well within the 640 px edge; 1 KiB is a threshold
            // no real cover clears.
            rendition: Some(Rendition { passthrough_max: 1024, ..rules }),
            ..cache.settings()
        });
        let thumb = fixtures::jpeg_decodable(500, 500);
        // The two halves of this case, stated on the **input**: comfortably
        // within the edge, and over the threshold. It is the second alone
        // that must send it through the encoder.
        assert_eq!(dimensions(&thumb), Some((500, 500)));
        assert!(500 <= rules.max_edge_px, "the fixture must be within the edge");
        assert!(thumb.len() > 1024, "and over the lowered threshold");
        cache.insert("k".into(), paired_entry(thumb.clone())).await;

        let (status, body) = served_body(&cache, "k", "?size=thumbnail").await;
        assert_eq!(status, 200);
        assert_ne!(body, thumb, "over the threshold, it must be re-encoded");
        assert_eq!(cache.renditions_built(), 1);
    }

    /// **The stand-in must not wear the validator the real full size
    /// wears.** `no-cache` only guarantees the browser asks again; the answer
    /// is the `ETag`'s business. Under `"{key}"` — the tag the `Bytes` arm
    /// gives real network bytes, and the one a fetched full size now takes —
    /// the browser's revalidation would compare equal, earn a `304`, and
    /// leave the stand-in on screen for ever. Its counterpart from the other
    /// side is `a_fetched_full_size_takes_the_validator_of_the_real_image`.
    ///
    /// Distinct from the thumbnail branch's tag too, and the third assertion
    /// is the one that matters there: out of the rule the two branches serve
    /// **different** bodies, so a shared validator would let a browser be
    /// served one size for the other.
    #[tokio::test]
    async fn the_stand_in_carries_a_validator_of_its_own() {
        let cache = Arc::new(CoverCache::new());
        // Out of the rule by its dimensions, so that the two branches really
        // do answer with different bytes — the case a shared validator would
        // corrupt.
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(900, 900))).await;

        let (status, bare_etag, _) = served_with_etag(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_ne!(
            bare_etag, "\"k\"",
            "the stand-in must not claim the validator the real full size will carry"
        );
        let (_, thumb_etag, _) = served_with_etag(&cache, "k", "?size=thumbnail").await;
        assert_ne!(bare_etag, thumb_etag, "two different bodies, two validators");

        // Its own validator does earn a 304: `no-cache` costs a round trip,
        // never a body.
        assert_eq!(served_if_none_match(&cache, "k", "", &bare_etag).await, 304);
        // And it earns one **only on its own branch**. This is the assertion
        // that would fail if the stand-in borrowed `"{key}-v"`: the thumbnail
        // branch would hand back a `304` for bytes it never served.
        assert_eq!(
            served_if_none_match(&cache, "k", "?size=thumbnail", &bare_etag).await,
            200,
            "the thumbnail branch must not honour the stand-in's validator"
        );
    }

    /// **The documented fall-back, which had no test.** A thumbnail that is a
    /// local path forms no pair — half a pair would then have to hold
    /// something other than bytes, and a local file costs a path anyway — so
    /// `fetch` answers with the full size, exactly as it would have without
    /// any thumbnail at all.
    ///
    /// The production change that would make this fail: pairing on anything
    /// `fetch_ref` returns rather than on bytes, which would put a `File`
    /// payload's path where the thumbnail's bytes belong; or treating an
    /// unusable thumbnail as a failure of the whole fetch, which would trade
    /// the cover for the optimization.
    #[tokio::test]
    async fn a_local_thumbnail_forms_no_pair_and_the_full_size_answers() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("front.jpg");
        let thumb = dir.path().join("front-500.jpg");
        std::fs::write(&full, fixtures::jpeg_decodable(1200, 1200)).unwrap();
        std::fs::write(&thumb, fixtures::jpeg_decodable(500, 500)).unwrap();

        let source = CoverSource::Ref(CoverRef::Path {
            path: full.to_string_lossy().into_owned(),
        });
        let thumb_ref = CoverRef::Path { path: thumb.to_string_lossy().into_owned() };
        let p = fetch(&source, Some(&thumb_ref), download_cap())
            .await
            .expect("the full size must still be obtained");
        match p {
            CoverPayload::File(got) => assert_eq!(got, full, "the full size, not the thumbnail"),
            other => panic!("a local thumbnail must form no pair, got {other:?}"),
        }
    }

    /// **The pair `fetch` actually builds, end to end** — and until now the
    /// only path of this worksite with no test at all. The sixteen tests of
    /// the `Pair` arm start from a pair written by hand, which proves what the
    /// route does with a pair and nothing about how one is assembled.
    ///
    /// Named production changes this kills: writing `full: t.clone()` instead
    /// of `r.clone()` — verified by mutation, the assertion on the full-size
    /// half fails — pairing the full size's bytes as the thumbnail, and
    /// dropping the `return` so the full size is fetched on announcement
    /// anyway.
    #[tokio::test]
    async fn a_remote_thumbnail_pairs_its_own_bytes_with_the_full_size_reference() {
        // **Both URLs answer**, which is what makes the interversion visible:
        // with only one canned body, swapping the halves would fail for want
        // of a body rather than for naming the wrong reference.
        let thumb_bytes = fixtures::jpeg_decodable(500, 500);
        let full_bytes = full_size_fixture();
        let _thumb_net = canned_url("https://example.org/front-500.jpg", thumb_bytes.clone());
        let _full_net = canned_url("https://example.org/front.jpg", full_bytes.clone());
        assert_ne!(thumb_bytes, full_bytes, "the two halves must be distinguishable");

        let full_url = "https://example.org/front.jpg";
        let source = CoverSource::Ref(CoverRef::Url { url: full_url.into() });
        let t = CoverRef::Url { url: "https://example.org/front-500.jpg".into() };
        let p = fetch(&source, Some(&t), download_cap())
            .await
            .expect("a remote thumbnail must produce a pair");
        match p {
            CoverPayload::Pair { thumb, thumb_mime, full, fetched } => {
                assert_eq!(thumb, thumb_bytes, "the thumbnail's bytes, not the full size's");
                assert_eq!(thumb_mime, "image/jpeg");
                assert_eq!(
                    full,
                    CoverRef::Url { url: full_url.into() },
                    "the full-size half must name the full size, not the thumbnail"
                );
                assert!(fetched.is_none(), "nothing is downloaded at the announcement");
            }
            other => panic!("a remote thumbnail must form a pair, got {other:?}"),
        }
    }

    /// **A local full size forms no pair, and its thumbnail is not even
    /// fetched.** The pair's bare URL is served `immutable` for a year, while
    /// a local full size is re-read on every request precisely because it can
    /// change under the appliance: pairing it would buy nothing (a path is
    /// already all it costs) and would make the route promise what it
    /// disproves one branch away.
    ///
    /// Named production changes this kills: dropping the `matches!(r, Url)`
    /// guard from `fetch`, which brings the contradiction back and, on the
    /// way, spends a download on a pair that buys nothing.
    #[tokio::test]
    async fn a_local_full_size_forms_no_pair_and_costs_no_download() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("front.jpg");
        std::fs::write(&full, fixtures::jpeg_decodable(1200, 1200)).unwrap();
        // A perfectly good thumbnail is waiting on the network: if the guard
        // went, this is what would come back as half of a pair.
        let _net = canned_url(
            "https://example.org/front-500.jpg",
            fixtures::jpeg_decodable(500, 500),
        );

        let source = CoverSource::Ref(CoverRef::Path {
            path: full.to_string_lossy().into_owned(),
        });
        let t = CoverRef::Url { url: "https://example.org/front-500.jpg".into() };
        let p = fetch(&source, Some(&t), download_cap())
            .await
            .expect("the local full size must still answer");
        match p {
            CoverPayload::File(got) => assert_eq!(got, full, "the file itself, not a pair"),
            other => panic!("a local full size must form no pair, got {other:?}"),
        }
    }

    /// **A remote thumbnail that fails takes the fetch down with it**, rather
    /// than falling back to the full size. Both halves come from the same
    /// host through the same door under the same cap, and the full size is
    /// the heavier by a factor of thirty-five (2.67 MiB against 73 KiB for
    /// the one shipped contributor): the fall-back would pull the whole
    /// `download_max` from a third party, chunk by chunk, only to refuse it —
    /// once per track played whose thumbnail failed.
    ///
    /// Named production change this kills: restoring the fall-back for a
    /// `CoverRef::Url` thumbnail, which would answer the canned full size
    /// here instead of nothing.
    #[tokio::test]
    async fn a_failed_remote_thumbnail_does_not_fall_back_to_the_full_size() {
        // The full size *would* answer — that is the whole point. Only the
        // thumbnail's URL is missing from the network.
        let _net = canned_url("https://example.org/front.jpg", full_size_fixture());
        let source = CoverSource::Ref(CoverRef::Url {
            url: "https://example.org/front.jpg".into(),
        });
        let t = CoverRef::Url { url: "https://example.org/front-500.jpg".into() };
        assert!(
            fetch(&source, Some(&t), download_cap()).await.is_none(),
            "a refusal on the small image is a refusal on the big one, and asking costs 2 MiB \
             of somebody else's bandwidth per track"
        );
    }

    #[tokio::test]
    async fn a_paired_entry_is_charged_its_thumbnail_and_not_its_full_size() {
        // The full-size half is only a reference as long as nobody enlarges
        // it: it must cost the budget nothing. The production change that
        // would make this fail: downloading the full size on announcement,
        // which is exactly what this task avoids.
        let cache = CoverCache::new();
        cache.insert("k".into(), paired_entry(vec![0u8; 3_333])).await;
        assert_eq!(cache.snapshot().await.used_bytes, 3_333);
    }

    /// **Proved by a counter, not by a duration.** The full size must not be
    /// looked for until somebody enlarges the cover; an assertion on a delay
    /// would only measure how fast the machine is.
    #[tokio::test]
    async fn the_full_size_is_not_fetched_until_someone_enlarges() {
        let cache = Arc::new(CoverCache::new());
        cache.answer_full_downloads_with(full_size_fixture(), "image/jpeg");
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;

        // The player's square, as many times as one likes: it asks for the
        // thumbnail, which is already there.
        for _ in 0..5 {
            let (status, _) = served_body(&cache, "k", "?size=thumbnail").await;
            assert_eq!(status, 200);
        }
        assert_eq!(cache.full_downloads(), 0, "nobody enlarged, nothing was downloaded");

        // And the counter is not simply inert: the very same cache downloads
        // as soon as the bare URL is asked for. Without this line the
        // assertion above would pass just as well against a cache that can
        // never download at all.
        let (status, body) = served_body(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, full_size_fixture(), "enlarging is what fetches the full size");
        assert_eq!(cache.full_downloads(), 1);
    }

    #[tokio::test]
    async fn concurrent_enlargements_download_the_full_size_once() {
        // The route is unauthenticated on the LAN: ten browsers enlarging the
        // same cover must produce one download, not ten multi-mebibyte bodies
        // held in memory at once — and not ten requests to a third party's
        // server, which is what turns one local request into an amplifier.
        //
        // Only a count of executions can prove it: the eight bodies are
        // byte-identical whether one download ran or eight.
        let cache = Arc::new(CoverCache::new());
        cache.answer_full_downloads_with(full_size_fixture(), "image/jpeg");
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;
        // **The eight callers are made simultaneous, not hoped to be**, on
        // the model of `concurrent_full_size_requests_extract_once`: a canned
        // download returns without ever suspending, so the first caller would
        // otherwise run register / download / remove in a single poll and
        // every follower would arrive to an empty table — eight downloads,
        // and a rendezvous never exercised.
        let hold = Arc::new(tokio::sync::Semaphore::new(0));
        cache.hold_full_downloads(hold.clone());

        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let c = cache.clone();
                tokio::spawn(async move { served_body(&c, "k", "").await })
            })
            .collect();

        // Waiting on a state, never on a duration — no clock appears in this
        // test. The bound exists so that a caller which never reaches the
        // rendezvous *fails* the test instead of hanging it.
        let mut spins = 0;
        while cache.rendezvous_arrivals() < 8 {
            spins += 1;
            assert!(
                spins < 100_000,
                "only {} of the eight callers reached the rendezvous",
                cache.rendezvous_arrivals()
            );
            tokio::task::yield_now().await;
        }
        // **One permit per caller, not one in total.** A rendezvous that
        // stopped collapsing would have all eight callers waiting here; with
        // a single permit, seven would wait for ever and the suite would hang
        // instead of reporting a failure. Intact, the flight consumes exactly
        // one and the spare permits are never taken.
        hold.add_permits(8);

        let mut bodies = Vec::new();
        for t in tasks {
            let (status, body) = t.await.expect("no task may panic");
            assert_eq!(status, 200);
            bodies.push(body);
        }
        assert!(
            bodies.iter().all(|b| b == &full_size_fixture()),
            "all eight must get the full size, not a stand-in"
        );
        assert_eq!(cache.full_downloads(), 1, "eight enlargements, one download");
    }

    /// **What validation does not protect against.** The route is
    /// unauthenticated on the LAN and the full size lives on a third party's
    /// server, so without memoising, every enlargement of the same cover asks
    /// coverartarchive.org for 2.5 MiB again -- and a loop on one valid key
    /// turns a local request into an amplifier aimed at someone else.
    ///
    /// The key itself is *not* attacker-chosen: `cover_get` only serves keys
    /// already in the cache, at most `MAX_ENTRIES` of them, and an unknown key
    /// answers 404. It is the **repetition** that had to be bounded, not the
    /// input -- which is why the fix is a memo and not a filter.
    #[tokio::test]
    async fn a_second_enlargement_does_not_download_again() {
        let cache = Arc::new(CoverCache::new());
        cache.answer_full_downloads_with(full_size_fixture(), "image/jpeg");
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;

        let (first, body) = served_body(&cache, "k", "").await;
        assert_eq!(first, 200);
        assert!(!body.is_empty());
        let (second, again) = served_body(&cache, "k", "").await;
        assert_eq!(second, 200);
        assert_eq!(again, body, "the same bytes, served from the memo");
        assert_eq!(
            cache.full_downloads(),
            1,
            "two enlargements, one download: the second must be free"
        );
    }

    #[tokio::test]
    async fn a_memoised_full_size_is_charged_to_the_budget() {
        // It is held in memory, so it must be paid for -- otherwise the budget
        // stops describing what the appliance holds, which is the one thing it
        // exists to do.
        //
        // The assertion is on the **increase**, not on an absolute value: the
        // downloaded body is decided by the test harness, and a hard-coded
        // figure here would be pinning the fixture rather than the rule.
        let cache = Arc::new(CoverCache::new());
        let full = full_size_fixture();
        cache.answer_full_downloads_with(full.clone(), "image/jpeg");
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;

        let before = cache.snapshot().await.used_bytes;
        let (status, _) = served_body(&cache, "k", "").await;
        assert_eq!(status, 200);
        let after = cache.snapshot().await.used_bytes;
        assert_eq!(
            after - before,
            full.len(),
            "the memoised full size must be charged, to the byte"
        );
    }

    /// **The full size obeys the source ceiling, not the download ceiling**,
    /// and the whole affordability of the pair rests on that distinction:
    /// `download_max` bounds what the appliance fetches by itself for every
    /// track it plays, `source_max` bounds what it agrees to read at all. An
    /// enlargement is a gesture of the reader, so it is the second that
    /// applies — under the first, a 2.5 MiB original would be refused here
    /// for exactly the reason it was refused on announcement, and no new
    /// setting could have been avoided.
    ///
    /// Named production change this guards: handing `download_max` to
    /// `full_size`, under which the first half of this test serves a stand-in.
    #[tokio::test]
    async fn the_full_size_obeys_the_source_ceiling_and_not_the_download_ceiling() {
        let settings = CoverSettings::default();
        // Between the two ceilings, which is the whole point: the product
        // ships 2 MiB of download and 20 MiB of source.
        let big = {
            let mut v = fixtures::jpeg_decodable(64, 64);
            v.resize(3 * 1024 * 1024, 0);
            v
        };
        assert!(big.len() > settings.download_max, "the fixture must be over the download cap");
        assert!(big.len() < settings.source_max, "and under the source cap");

        let cache = Arc::new(CoverCache::new());
        cache.answer_full_downloads_with(big.clone(), "image/jpeg");
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;
        let (status, body) = served_body(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, big, "the source ceiling is what applies to an enlargement");

        // The other edge, so that the assertion above cannot be satisfied by
        // a ceiling that is simply never applied: lower `source_max` under
        // the same body and the fetch must be refused, leaving the thumbnail.
        let thumb = fixtures::jpeg_decodable(500, 500);
        let tight = Arc::new(CoverCache::new());
        tight.set_cover_settings(CoverSettings { source_max: 1024 * 1024, ..settings });
        tight.answer_full_downloads_with(big, "image/jpeg");
        tight.insert("k".into(), paired_entry(thumb.clone())).await;
        let (status, body) = served_body(&tight, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, thumb, "over the source ceiling, the stand-in answers");
    }

    /// **The real full size takes `"{key}"`, and that is what un-freezes the
    /// stand-ins already in browsers.** The previous task gave the stand-in a
    /// tag of its own precisely so this one could take the tag the `Bytes`
    /// arm gives real network bytes: a browser still holding a stand-in then
    /// mismatches on its own and is served a `200`, with no decision left
    /// unwritten anywhere.
    ///
    /// Named production changes this guards: reusing `"{key}-standin"` or
    /// `"{key}-v"` for the fetched full size — the first would answer `304`
    /// to every browser holding a stand-in and leave it on screen for ever,
    /// the second would let the thumbnail URL and the bare URL be served one
    /// for the other.
    #[tokio::test]
    async fn a_fetched_full_size_takes_the_validator_of_the_real_image() {
        let cache = Arc::new(CoverCache::new());
        cache.answer_full_downloads_with(full_size_fixture(), "image/jpeg");
        // Out of the rule by its dimensions, so that the thumbnail branch
        // really does answer with different bytes — the case a shared
        // validator would corrupt.
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(900, 900))).await;

        let (status, etag, cache_control, body) = served_with_headers(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, full_size_fixture());
        assert_eq!(etag, "\"k\"", "the real full size wears the key itself");
        assert!(
            cache_control.contains("immutable"),
            "this URL determines its content, so a year is right: {cache_control}"
        );

        // A browser that cached the stand-in — the answer this URL gave
        // before anything was downloaded — must be served the real image, not
        // a 304 confirming what it holds.
        assert_eq!(
            served_if_none_match(&cache, "k", "", "\"k-standin\"").await,
            200,
            "a cached stand-in must not survive the arrival of the real full size"
        );
        // And the thumbnail URL must not honour the full size's validator:
        // out of the rule the two serve different bytes.
        let (_, thumb_etag, _) = served_with_etag(&cache, "k", "?size=thumbnail").await;
        assert_ne!(thumb_etag, etag, "two different bodies, two validators");
        assert_eq!(served_if_none_match(&cache, "k", "?size=thumbnail", &etag).await, 200);
    }

    /// **A thumbnail's validator must follow the settings that produced
    /// it**, at all four arms of the route.
    ///
    /// The symptom this closes, reported against the shipped device: the
    /// owner takes the longest edge from 640 down to 320 px and saves. The
    /// core re-renders at once — a rendition's identity has carried
    /// `Rendition::tag` all along — the browser revalidates against an
    /// unchanged `ETag`, is told `304`, and goes on displaying the 640. For a
    /// cover from the internet it was worse: served `immutable` for a year,
    /// the browser did not even ask.
    ///
    /// **The proof is entirely in the headers.** A test comparing two bodies
    /// would prove nothing about it: the core was already producing the right
    /// bytes, and it is the browser that was never allowed to see them.
    ///
    /// Named production changes this guards: dropping `rules_tag` from
    /// `variant_etag` or from `frozen_headers`; giving `rendition: None` no
    /// representation of its own, under which unchecking re-encoding alone
    /// would go unnoticed; and putting `immutable` back on a network
    /// thumbnail, which would make a changed validator unreachable.
    #[tokio::test]
    async fn a_thumbnail_validator_follows_the_rules_that_produced_it() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder.jpg");
        std::fs::write(&folder, fixtures::jpeg_decodable(900, 900)).unwrap();

        let cache = Arc::new(CoverCache::new());
        cache
            .insert(
                "net".into(),
                CoverPayload::Bytes(fixtures::jpeg_decodable(900, 900), "image/jpeg"),
            )
            .await;
        cache.insert("pair".into(), paired_entry(fixtures::jpeg_decodable(900, 900))).await;
        cache.insert("file".into(), CoverPayload::File(folder)).await;
        let mut arms = vec!["net", "pair", "file"];
        // The fourth arm needs a real container. Skipped rather than faked
        // when ffmpeg is absent, like every other embedded test here — the
        // three arms above still fail on the same defect.
        match crate::player::mpv::tests::mp3_with_cover_from(dir.path(), "color=c=red:s=64x64:d=1")
        {
            Some(track) => {
                cache.insert("tags".into(), CoverPayload::Embedded(track)).await;
                arms.push("tags");
            }
            None => eprintln!("ffmpeg missing: leaving the Embedded arm out of this test"),
        }

        let base = CoverSettings::default();
        cache.set_cover_settings(CoverSettings {
            rendition: Some(test_rendition(400, 512 * 1024, 16_000_000)),
            ..base
        });
        let mut wide = Vec::new();
        for arm in &arms {
            let (status, etag, cache_control, _) =
                served_with_headers(&cache, arm, "?size=thumbnail").await;
            assert_eq!(status, 200, "{arm}");
            assert!(!etag.is_empty(), "{arm}: a thumbnail must carry a validator");
            // Without this, a validator that changes is worth nothing: the
            // browser has been told not to ask again for a year.
            assert!(
                !cache_control.contains("immutable"),
                "{arm}: a rendering must stay revalidatable, got {cache_control}"
            );
            wide.push((*arm, etag));
        }

        cache.set_cover_settings(CoverSettings {
            rendition: Some(test_rendition(100, 512 * 1024, 16_000_000)),
            ..base
        });
        let mut narrow = Vec::new();
        for (arm, before) in &wide {
            let (_, etag, _, _) = served_with_headers(&cache, arm, "?size=thumbnail").await;
            assert_ne!(&etag, before, "{arm}: a new longest edge must be a new validator");
            assert_eq!(
                served_if_none_match(&cache, arm, "?size=thumbnail", before).await,
                200,
                "{arm}: the validator of the previous size must not earn a 304"
            );
            narrow.push((*arm, etag));
        }

        // Unchecking re-encoding is a change of what the route answers with
        // too — the source, untouched — so it must be a change of validator
        // as well, which an absent rules tag would have hidden.
        cache.set_cover_settings(CoverSettings { rendition: None, ..base });
        for ((arm, wide_etag), (_, narrow_etag)) in wide.iter().zip(narrow.iter()) {
            let (_, etag, _, _) = served_with_headers(&cache, arm, "?size=thumbnail").await;
            assert_ne!(&etag, wide_etag, "{arm}: unchecking must not look like the 400 px rules");
            assert_ne!(&etag, narrow_etag, "{arm}: nor like the 100 px rules");
            assert_eq!(
                served_if_none_match(&cache, arm, "?size=thumbnail", narrow_etag).await,
                200,
                "{arm}: the validator of the last rendering must not earn a 304 either"
            );
        }
    }

    /// **A revalidation must never reach the network.** The bare URL of a
    /// pair answers `304` from the key alone — these bytes are frozen under
    /// it — so a browser that already holds the full size costs a header and
    /// nothing else, whether or not this process still has the image.
    ///
    /// Named production change this guards: moving the `If-None-Match` check
    /// down beside the response it labels, which reads naturally and would
    /// make every revalidation fetch 2.5 MiB from a third party before
    /// discarding it.
    #[tokio::test]
    async fn a_conditional_enlargement_never_reaches_the_network() {
        let cache = Arc::new(CoverCache::new());
        cache.answer_full_downloads_with(full_size_fixture(), "image/jpeg");
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;

        // Nothing has been downloaded on this cache, deliberately: the answer
        // must come from the validator, not from a memo.
        assert_eq!(served_if_none_match(&cache, "k", "", "\"k\"").await, 304);
        assert_eq!(
            cache.full_downloads(),
            0,
            "a browser that already holds the image must cost nobody a request"
        );
    }

    /// **A memo that is charged must also be reconciled**, and against the
    /// right entry. `remember_full` grows an entry by megabytes, so the cache
    /// has to be brought back under its budget then and there — and the entry
    /// that grew is the one to protect, never the one to sacrifice.
    ///
    /// Named production changes this guards: dropping `evict_to_budget` from
    /// `remember_full`, under which the budget is exceeded until the next
    /// insertion happens to run it; and passing `None` where `Some(key)` goes,
    /// under which the entry pays for its own growth — evicted on the spot,
    /// so the reader's next click downloads all over again.
    #[tokio::test]
    async fn memoising_a_full_size_reconciles_the_budget_around_it() {
        let thumb = fixtures::jpeg_decodable(500, 500);
        let big = {
            let mut v = fixtures::jpeg_decodable(64, 64);
            v.resize(3 * 1024 * 1024, 0);
            v
        };
        let cache = Arc::new(CoverCache::new());
        // A budget the memo alone blows through, so that the reconciliation
        // has no choice but to act.
        cache.set_cover_settings(CoverSettings {
            budget: 1024 * 1024,
            ..CoverSettings::default()
        });
        cache.answer_full_downloads_with(big, "image/jpeg");
        cache.insert("enlarged".into(), paired_entry(thumb.clone())).await;
        cache.insert("neighbour".into(), paired_entry(thumb)).await;
        assert!(cache.contains("neighbour").await, "both fit before the enlargement");

        let (status, _) = served_body(&cache, "enlarged", "").await;
        assert_eq!(status, 200);
        assert!(
            cache.contains("enlarged").await,
            "the entry that grew must not be evicted to pay for its own growth"
        );
        assert!(
            !cache.contains("neighbour").await,
            "and the budget must be reconciled the moment the memo lands"
        );
        let (status, _) = served_body(&cache, "enlarged", "").await;
        assert_eq!(status, 200);
        assert_eq!(cache.full_downloads(), 1, "the memo survived, so the second click is free");
    }

    /// **The memo must cost a refcount, not a copy.** `CoverCache::read`
    /// clones the whole payload before the route has even looked at the size
    /// it was asked for — the player's square and a `304` both go through
    /// that clone — so an owned buffer here would make every later request on
    /// an enlarged cover copy up to `source_max` on an unauthenticated route.
    ///
    /// Named production change this guards: `fetched` holding a `Vec<u8>`
    /// instead of an `Arc<Vec<u8>>` — under which this test does not even
    /// compile, there being no pointer left to compare.
    #[tokio::test]
    async fn a_memoised_full_size_is_shared_and_not_copied() {
        let cache = Arc::new(CoverCache::new());
        cache.answer_full_downloads_with(full_size_fixture(), "image/jpeg");
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;
        let (status, _) = served_body(&cache, "k", "").await;
        assert_eq!(status, 200);

        // **Both held at once**, as in `concurrent_full_size_requests_share_
        // one_allocation`: read one at a time and dropped, the allocator
        // could hand the second read the address the first just freed.
        let one = cache.read("k").await.expect("the entry is still there");
        let two = cache.read("k").await.expect("the entry is still there");
        let (
            CoverPayload::Pair { fetched: Some((first, _)), .. },
            CoverPayload::Pair { fetched: Some((second, _)), .. },
        ) = (&one, &two)
        else {
            panic!("both reads must carry the memo");
        };
        assert!(
            Arc::ptr_eq(first, second),
            "two reads of the payload must share one buffer, not copy it"
        );
        assert!(
            Arc::strong_count(first) >= 3,
            "the entry and both reads point at it: {}",
            Arc::strong_count(first)
        );

        // The hot path this protects, exercised: a conditional thumbnail
        // request goes through that very clone before answering `304`.
        let (_, thumb_etag, _) = served_with_etag(&cache, "k", "?size=thumbnail").await;
        assert_eq!(served_if_none_match(&cache, "k", "?size=thumbnail", &thumb_etag).await, 304);
    }

    /// **The filter runs before anything is fetched, and it is the real
    /// one.** `allowed_target` is what stops the SSE of a third-party source
    /// from pointing this appliance at its own network; the enlargement path
    /// must go through it exactly as the announcement path does.
    ///
    /// This test opens no socket, and could not: the refusal happens before
    /// the fetch, which is precisely what makes the property provable without
    /// a network.
    ///
    /// Named production change this guards: reaching for the body before
    /// judging the target.
    #[tokio::test]
    async fn a_refused_target_is_never_fetched() {
        for url in ["http://example.org/front.jpg", "https://192.168.1.10/front.jpg"] {
            let cache = Arc::new(CoverCache::new());
            // A body is waiting to be served: if the filter did not run, this
            // is what would come back.
            cache.answer_full_downloads_with(full_size_fixture(), "image/jpeg");
            let thumb = fixtures::jpeg_decodable(500, 500);
            cache
                .insert("k".into(), paired_with(thumb.clone(), CoverRef::Url { url: url.into() }))
                .await;

            let (status, body) = served_body(&cache, "k", "").await;
            assert_eq!(status, 200, "{url}");
            assert_eq!(body, thumb, "{url}: a refused target must leave the stand-in");
            assert_ne!(body, full_size_fixture(), "{url}");
        }
    }

    /// **A local full size is served and never memoised.** The rule of this
    /// module is that the network means the internet: a file on the share is
    /// local, re-readable at the cost of a read, and holding megabytes of it
    /// in memory would charge the budget for something a `File` payload
    /// costs nothing at all.
    ///
    /// **The pair used here is written by hand, and `fetch` no longer builds
    /// one like it**: a local full size is not paired at all (see `fetch`,
    /// and the test that pins it). What is exercised here is therefore the
    /// route's own robustness — `CoverPayload` still lets such a pair be
    /// expressed, and answering nothing for a half whose path is in hand
    /// would be worse than reading it. Keeping it is what makes this the one
    /// branch of `perform_full_download` that runs for real under test: no
    /// canned body is installed here, so a served image proves
    /// `read_file_bounded` itself ran, cap and time bound included.
    ///
    /// Named production changes this guards: memoising a local full size,
    /// which the budget assertion catches; and handing the read a cap it does
    /// not apply, which the second half catches.
    #[tokio::test]
    async fn a_local_full_size_is_read_afresh_and_never_memoised() {
        let dir = tempfile::tempdir().unwrap();
        let on_the_share = dir.path().join("front.jpg");
        let original = fixtures::jpeg_decodable(1200, 1200);
        std::fs::write(&on_the_share, &original).unwrap();
        let full = CoverRef::Path { path: on_the_share.to_string_lossy().into_owned() };
        let thumb = fixtures::jpeg_decodable(500, 500);

        let cache = Arc::new(CoverCache::new());
        cache.insert("k".into(), paired_with(thumb.clone(), full.clone())).await;
        let charged = cache.snapshot().await.used_bytes;

        let (status, body) = served_body(&cache, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, original, "the file on the share is what the bare URL serves");
        assert_eq!(
            cache.snapshot().await.used_bytes,
            charged,
            "a local full size costs a read, never a place in the budget"
        );
        let (_, again) = served_body(&cache, "k", "").await;
        assert_eq!(again, original);
        assert_eq!(cache.full_downloads(), 2, "read afresh rather than memoised");

        // The cap the real reader applies, at a value the file is over: the
        // ceiling is not merely passed down, it bites.
        let tight = Arc::new(CoverCache::new());
        tight.set_cover_settings(CoverSettings {
            source_max: 1024,
            ..CoverSettings::default()
        });
        tight.insert("k".into(), paired_with(thumb.clone(), full)).await;
        let (status, body) = served_body(&tight, "k", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, thumb, "over the source ceiling, the stand-in answers");
    }

    /// **A broken target must not let the LAN write the journal.** This route
    /// needs no authentication and the failure is deliberately not memoised,
    /// so a client looping on one broken key would otherwise produce one
    /// `warn` per request, at its own cadence.
    ///
    /// Counted rather than read out of a captured log, and the reason is
    /// written on the `unfetchable_reports` field: a thread-local subscriber
    /// cannot see — and cannot avoid racing with — the sibling tests that
    /// reach the same callsite on other threads.
    ///
    /// Named production changes this guards: dropping the throttle, and
    /// warning from `cover_get`'s fall-back instead, where the line reads
    /// naturally and where nothing can count how often it has already been
    /// said.
    #[tokio::test]
    async fn a_broken_target_is_reported_once_however_often_it_is_clicked() {
        let cache = Arc::new(CoverCache::new());
        // No canned body: every one of these enlargements fails.
        cache.insert("k".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;

        for _ in 0..5 {
            let (status, _) = served_body(&cache, "k", "").await;
            assert_eq!(status, 200);
        }
        assert_eq!(cache.full_downloads(), 5, "every click did try again");
        assert_eq!(cache.unfetchable_reports(), 1, "five clicks, one report");

        // **And the silence is per key, not global.** A second broken cover
        // is a different event and must be said: a throttle that swallowed it
        // would hide the failure this log exists to explain.
        cache.insert("other".into(), paired_entry(fixtures::jpeg_decodable(500, 500))).await;
        let (status, _) = served_body(&cache, "other", "").await;
        assert_eq!(status, 200);
        assert_eq!(cache.unfetchable_reports(), 2, "another key, another report");
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
        assert_eq!(cache.renditions_built(), 1, "the second request must be free");
    }

    #[tokio::test]
    async fn a_display_push_reuses_the_rendition_the_http_route_already_built() {
        // **The whole point of sharing.** A cover has two consumers — the
        // socket of a subscribed display and the page's 224 px square — and
        // each decoded then re-encoded the same image on its own side. On a
        // Pi 2 that is a second core busy for several hundred milliseconds,
        // producing bytes that are already in memory, byte for byte.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(1200, 1200)).unwrap();
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".to_string(), CoverPayload::File(path)).await;

        let (_, over_http) = served_body(&cache, "k", "?size=thumbnail").await;
        assert_eq!(cache.renditions_built(), 1, "the route builds the first one");

        let line =
            cache.line("k", "/api/cover/k").await.expect("a local image must produce a line");
        // **`renditions_built` and not `thumbnails_built`**: the latter only
        // watches the HTTP path, so it stays at one however many times the
        // display path re-encodes — an assertion that cannot fail proves
        // nothing. Only a count spanning both consumers can.
        assert_eq!(
            cache.renditions_built(),
            1,
            "the display push must reuse the rendition rather than pay for it again"
        );
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&line).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => assert_eq!(
                c.bytes, over_http,
                "and it must be the very bytes the route serves, not merely as many"
            ),
            other => panic!("expected a cover frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn changing_the_rendition_rules_stops_serving_the_previous_size() {
        // **A defect found while sharing this cache, not caused by it.**
        // Nothing invalidated a memorized rendition when the owner changed the
        // cover settings, and `set_cover_settings` still claimed there was
        // nothing to invalidate — true only before the cache existed. Lowering
        // the longest edge in the admin page kept serving the old size until
        // the entry happened to fall out on its own.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(1200, 1200)).unwrap();
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".to_string(), CoverPayload::File(path)).await;

        cache.set_cover_settings(CoverSettings {
            rendition: Some(test_rendition(400, 512 * 1024, 16_000_000)),
            ..CoverSettings::default()
        });
        let (_, wide) = served_body(&cache, "k", "?size=thumbnail").await;
        let (w, _) = dimensions(&wide).expect("a readable thumbnail");
        assert_eq!(w, 400, "the first rules apply");

        cache.set_cover_settings(CoverSettings {
            rendition: Some(test_rendition(100, 512 * 1024, 16_000_000)),
            ..CoverSettings::default()
        });
        let (_, narrow) = served_body(&cache, "k", "?size=thumbnail").await;
        let (w, _) = dimensions(&narrow).expect("a readable thumbnail");
        assert_eq!(
            w, 100,
            "the new longest edge must take effect at once, not at the next eviction"
        );
    }

    #[tokio::test]
    async fn the_route_stops_serving_the_thumbnail_of_a_replaced_file() {
        // The property the whole stamp exists for, seen from the browser: the
        // owner drops a new `folder.jpg` on the share, and the square must
        // stop showing the old one. Sharing the cache with the display path
        // widened the blast radius of getting this wrong, so it is asserted
        // here too and not only through `line`.
        //
        // **Two different shapes on purpose**: the stamp is modification date
        // *and* size, and on Windows the system clock advances only about
        // every 15 ms — two writes in a row can share a date. Different
        // dimensions make the size differ, so this test measures the cache and
        // not the host's clock granularity.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(1200, 1200)).unwrap();
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".to_string(), CoverPayload::File(path.clone())).await;

        let (_, before) = served_body(&cache, "k", "?size=thumbnail").await;
        std::fs::write(&path, fixtures::jpeg_decodable(900, 1500)).unwrap();
        let (_, after) = served_body(&cache, "k", "?size=thumbnail").await;

        assert_ne!(before, after, "the replaced image must reach the square");
        assert_eq!(cache.renditions_built(), 2, "and it must have been rendered afresh");
        let (w, h) = dimensions(&after).expect("a readable thumbnail");
        assert!(h > w, "the new cover is a portrait, the old one was square: {w}x{h}");
    }

    #[tokio::test]
    async fn two_browsers_asking_at_the_same_instant_decode_the_image_once() {
        // `line` had its rendezvous, the route had none: two tabs opening the
        // page together — or a display and a browser — each paid for the same
        // decode. Same proof as `eight_displays_…`: only a count of executions
        // can show it, since both would return identical bytes either way.
        let cache = Arc::new(CoverCache::new());
        // Real work to do: 600 × 400 is over the longest edge, so the image is
        // decoded and re-encoded rather than passed through. Without that the
        // first arrival would finish without ever suspending, no follower
        // would have time to show up, and the test would prove nothing.
        cache.set_cover_settings(CoverSettings {
            rendition: Some(test_rendition(64, 512 * 1024, 16_000_000)),
            ..CoverSettings::default()
        });
        cache
            .insert(
                "k".to_string(),
                CoverPayload::Bytes(fixtures::jpeg_decodable(600, 400), "image/jpeg"),
            )
            .await;

        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let c = cache.clone();
                tokio::spawn(async move { served_body(&c, "k", "?size=thumbnail").await })
            })
            .collect();
        let mut bodies = Vec::new();
        for t in tasks {
            let (status, body) = t.await.expect("no task may panic");
            assert_eq!(status, 200);
            bodies.push(body);
        }
        assert!(bodies.iter().all(|b| b == &bodies[0]), "all eight must get the same image");
        assert_eq!(cache.renditions_built(), 1, "eight callers, one decode");
    }

    /// **The guard for this task's whole deletion.** `CoverPayload::Embedded`
    /// names the user's own *audio* file, not a copy of ours — nothing here
    /// may ever delete it. Written and proven green before commit 901bbe2
    /// removed the temp-file machinery — the `remove_file` call this test
    /// guarded against — from `insert`, so that the removal was checked
    /// against a real assertion rather than against the absence of a crash.
    ///
    /// **Re-armed under `MAX_ENTRIES`, not the byte budget.** The
    /// count-based cap this test used to lower (`entries: 1`) is gone, and a
    /// tight *byte* budget can no longer stand in for it: an `Embedded`
    /// costs 0 (`payload_cost`), and `evict_to_budget`'s step 3 now skips
    /// every zero-cost entry rather than evict one for no gain (see its
    /// doc). The only cap left that ever touches a zero-cost entry is
    /// `MAX_ENTRIES`, so this test pushes `entries` one past it: `"a"`,
    /// inserted first, is the oldest, and the trim removes it to bring the
    /// count back down. The assertion on `contains("a")` proves the
    /// eviction really ran, not merely that nothing crashed; the assertion
    /// on `track.exists()` is the one that matters, and is what would have
    /// caught the `remove_file` this test was written against (removed in
    /// commit 901bbe2).
    ///
    /// No real MP3 needed: `insert` never opens the file, it only ever moves
    /// a path in and out of the cache — so a one-byte stand-in exercises the
    /// same code path as a real track, and this, the branch's most dangerous
    /// deletion, is guarded unconditionally rather than skipping whenever
    /// ffmpeg happens to be absent.
    #[tokio::test]
    async fn inserting_over_an_embedded_entry_deletes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("t.mp3");
        std::fs::write(&track, b"x").unwrap();
        let cache = CoverCache::new();
        cache.insert("a".into(), CoverPayload::Embedded(track.clone())).await;
        for i in 0..MAX_ENTRIES {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![1], "image/jpeg")).await;
        }
        assert!(!cache.contains("a").await, "the test must actually force an eviction to prove anything");
        assert!(track.exists(), "insertion must never delete the user's audio file");
    }

    /// Insertion must never touch a `folder.jpg` declared by a Source, which
    /// lives on its own share and is not ours to delete. See the doc of
    /// `inserting_over_an_embedded_entry_deletes_no_file` just above for why
    /// `MAX_ENTRIES`, not the byte budget, is what forces the eviction now.
    #[tokio::test]
    async fn inserting_over_a_source_folder_jpg_deletes_no_file() {
        let source_dir = tempfile::tempdir().unwrap();
        let folder_jpg = source_dir.path().join("folder.jpg");
        std::fs::write(&folder_jpg, b"x").unwrap();

        let cache = CoverCache::new();
        cache.insert("to-keep".into(), CoverPayload::File(folder_jpg.clone())).await;
        for i in 0..MAX_ENTRIES {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![1], "image/jpeg")).await;
        }

        assert!(!cache.contains("to-keep").await, "the test must actually force an eviction to prove anything");
        assert!(folder_jpg.exists(), "a Source's folder.jpg must never be deleted on our own initiative");
    }

    // -- `evict_to_budget`: the byte budget, not a count -------------------

    #[tokio::test]
    async fn the_budget_evicts_by_bytes_not_by_count() {
        // Fails the moment `insert` goes back to counting entries instead of
        // bytes (or drops `evict_to_budget` entirely): a count-based cap
        // would keep some fixed number of these regardless of their size.
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings {
            budget: 8 * 1024 * 1024,
            ..CoverSettings::default()
        });
        for i in 0..20 {
            cache
                .insert(format!("k{i}"), CoverPayload::Bytes(vec![0u8; 3 * 1024 * 1024], "image/jpeg"))
                .await;
        }
        let mut kept = 0;
        for i in 0..20 {
            if cache.contains(&format!("k{i}")).await {
                kept += 1;
            }
        }
        assert_eq!(kept, 2, "an 8 MiB budget must hold two 3 MiB covers, not twenty");
    }

    #[tokio::test]
    async fn an_entry_larger_than_the_whole_budget_is_still_served() {
        // Fails if `evict_to_budget` ever removes the entry the caller just
        // inserted while trying to satisfy an unsatisfiable budget:
        // `keep_entry` would no longer be honored, and a bad configuration
        // (a budget smaller than one cover) would leave the cache unable to
        // serve anything at all, or would hang trying.
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { budget: 10, ..CoverSettings::default() });
        cache.insert("huge".into(), CoverPayload::Bytes(vec![0u8; 1024], "image/jpeg")).await;
        assert!(
            cache.contains("huge").await,
            "the one cover a misconfigured budget was asked to hold must still be served"
        );
    }

    #[tokio::test]
    async fn local_entries_cost_nothing_and_do_not_loop_the_eviction() {
        // Fails if step 3 goes back to evicting whatever is oldest
        // regardless of cost: with the protected `Bytes` entry below the
        // only thing in the cache that costs anything, and every local
        // entry costing 0, step 3 has no eligible candidate at all and must
        // return immediately rather than walking through all fifty local
        // entries hunting for bytes that are not there.
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { budget: 1, ..CoverSettings::default() });
        for i in 0..50 {
            cache
                .insert(format!("local-{i}"), CoverPayload::File(PathBuf::from(format!("/nas/{i}.jpg"))))
                .await;
        }
        // Pushes the total permanently over budget: this entry alone is
        // over the 1-byte budget, is protected as the one just inserted,
        // and every local entry above costs 0 — nothing can ever bring the
        // total back under budget by evicting them, so none of them should
        // even be tried.
        cache.insert("network".into(), CoverPayload::Bytes(vec![0u8; 1024], "image/jpeg")).await;

        assert!(cache.contains("network").await, "the just-inserted entry is never evicted");
        let mut kept = 0;
        for i in 0..50 {
            if cache.contains(&format!("local-{i}")).await {
                kept += 1;
            }
        }
        assert_eq!(
            kept, 50,
            "a zero-cost entry must never be evicted in a vain search for bytes that do not \
             exist there — only {kept} of 50 local entries survived"
        );
    }

    #[tokio::test]
    async fn the_oldest_costly_source_is_evicted_not_a_zero_cost_one_in_between() {
        // Fails if step 3 goes back to picking the oldest entry regardless
        // of cost: `a` (a `File`, 0 bytes) sits before `b` (a `Bytes`, 2048
        // bytes) in insertion order. Evicting `a` first would free nothing,
        // permanently leave the budget exceeded, and destroy a perfectly
        // usable local entry for no gain — `b` is the one that must go.
        let cache = CoverCache::new();
        cache.insert("a".into(), CoverPayload::File(PathBuf::from("/nas/a.jpg"))).await;
        cache.insert("b".into(), CoverPayload::Bytes(vec![0u8; 2048], "image/jpeg")).await;
        // Lowered after the fact, exactly as `renditions_are_dropped_before_sources`
        // does it: eviction runs lazily, on the next write.
        cache.set_cover_settings(CoverSettings { budget: 100, ..CoverSettings::default() });
        cache.insert("c".into(), CoverPayload::File(PathBuf::from("/nas/c.jpg"))).await;

        assert!(cache.contains("a").await, "a zero-cost entry must never be evicted to free bytes it does not have");
        assert!(!cache.contains("b").await, "the entry that actually costs bytes must be the one evicted");
        assert!(cache.contains("c").await, "the just-inserted entry is never evicted");
    }

    #[tokio::test]
    async fn renditions_are_dropped_before_sources() {
        // Fails if step 3 (evict the oldest source) ever runs while step 2
        // (evict the oldest rendition) still has something to give: the
        // rendition here alone is enough to bring the total back under
        // budget, so if the source were touched instead, `contains("src")`
        // would go false.
        let cache = CoverCache::new();
        cache.insert("src".into(), CoverPayload::Bytes(vec![0u8; 1000], "image/jpeg")).await;
        // Built under the settings' current rules, so step 1 (purge stale
        // renditions) leaves it alone — this test is about step 2 versus
        // step 3, not about a fingerprint mismatch.
        let rules = CoverSettings::default().rendition.expect("rendition on by default");
        let identity = rendition_identity("src", &SourceStamp::Frozen, &rules);
        cache.remember_rendition(identity.clone(), "image/jpeg", Arc::new(vec![0u8; 500])).await;

        // The source alone (1000 bytes) fits; the rendition on top (1500
        // total) does not.
        cache.set_cover_settings(CoverSettings { budget: 1000, ..CoverSettings::default() });
        // Eviction runs lazily, on the next write — exactly as in
        // production, where nothing re-checks the budget on a settings
        // change alone.
        cache.insert("trigger".into(), CoverPayload::File(PathBuf::from("/nas/x.jpg"))).await;

        assert!(cache.contains("src").await, "a rendition must give way before its own source is touched");
        assert!(
            cache.cached_rendition(&identity).await.is_none(),
            "the rendition must have been evicted to bring the total back under budget"
        );
    }

    #[tokio::test]
    async fn a_local_file_that_is_not_an_image_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("folder.jpg");
        std::fs::write(&fake, b"this is not an image").unwrap();
        let r = CoverSource::Ref(CoverRef::Path { path: fake.to_string_lossy().into_owned() });
        assert!(
            fetch(&r, None, download_cap()).await.is_none(),
            "the header bytes must be checked: without this, a badly written contributor \
             would get any file of the system served on a public HTTP route"
        );

        let real = dir.path().join("cover.jpg");
        // Minimal JPEG header: SOI + APP0 marker.
        std::fs::write(&real, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = CoverSource::Ref(CoverRef::Path { path: real.to_string_lossy().into_owned() });
        match fetch(&r, None, download_cap()).await {
            Some(CoverPayload::File(p)) => assert_eq!(p, real),
            other => panic!("a local image must stay a path, not bytes: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetching_an_embedded_source_touches_no_disk() {
        // The whole point of the probe: by the time a `CoverSource::Embedded`
        // exists, `player::mpv::embedded_cover` has already established that a
        // picture is there. `fetch` must therefore neither open nor even stat
        // the audio file — a nonexistent path is the proof: any attempt to
        // touch it would make this fail, where the real production path only
        // ever hands `fetch` a path it just probed successfully.
        let audio = PathBuf::from("/does/not/exist.mp3");
        let s = CoverSource::Embedded { audio: audio.clone(), content: "abcd".into() };
        match fetch(&s, None, download_cap()).await {
            Some(CoverPayload::Embedded(p)) => assert_eq!(p, audio),
            other => panic!("an embedded source must yield CoverPayload::Embedded: {other:?}"),
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

    /// The download cap of the default settings, for `fetch` calls in tests
    /// that exercise a local `Ref`/`Embedded` source: `download_max` plays
    /// no role there, but `fetch` now takes it regardless, so every call site
    /// needs a value — the default is the one a factory-fresh device applies.
    fn download_cap() -> usize {
        CoverSettings::default().download_max
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
        assert_eq!(
            cache.bytes("k", cap()).await,
            Some(("image/png", image, SourceStamp::Frozen)),
            "a body read whole needs no stamp: nothing can change it under its key"
        );
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
        let (mime, read, stamp) = cache.bytes("k", cap()).await.expect("the file must be read");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(read, image);
        assert!(
            matches!(stamp, SourceStamp::File { size, .. } if size == image.len() as u64),
            "a file is stamped by what can change under its path, its size included: {stamp:?}"
        );
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
        let r = CoverSource::Ref(CoverRef::Path { path: path.to_string_lossy().into_owned() });
        let Some(p) = fetch(&r, None, download_cap()).await else { panic!("a local image must be accepted") };
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
            Some((mime, o, _)) => {
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

    // -- Embedded: the picture lives inside the audio file, not beside it --

    #[tokio::test]
    async fn an_embedded_cover_is_read_from_the_audio_file_itself() {
        let dir = tempfile::tempdir().unwrap();
        let Some(track) =
            crate::player::mpv::tests::mp3_with_cover_from(dir.path(), "color=c=red:s=32x32:d=1")
        else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::Embedded(track.clone())).await;

        let (mime, bytes, stamp) = cache
            .bytes("k", 8 * 1024 * 1024)
            .await
            .expect("the embedded picture must be readable through the cache");

        assert_eq!(mime, "image/jpeg");
        assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]), "expected a JPEG header");
        // The stamp must describe the AUDIO file: that is what a conditional
        // request will be validated against.
        let meta = std::fs::metadata(&track).unwrap();
        assert_eq!(stamp, SourceStamp::of_file(&meta));
    }

    #[tokio::test]
    async fn an_embedded_cover_over_the_cap_yields_nothing_like_a_file_does() {
        let dir = tempfile::tempdir().unwrap();
        let Some(track) =
            crate::player::mpv::tests::mp3_with_cover_from(dir.path(), "color=c=red:s=32x32:d=1")
        else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::Embedded(track)).await;
        // A cap of one byte: whatever ffmpeg produced is over it.
        assert!(cache.bytes("k", 1).await.is_none(), "the cap must apply to an embedded picture too");
    }

    #[tokio::test]
    async fn a_conditional_request_on_an_embedded_cover_parses_nothing() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let Some(track) =
            crate::player::mpv::tests::mp3_with_cover_from(dir.path(), "color=c=teal:s=32x32:d=1")
        else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".into(), CoverPayload::Embedded(track)).await;
        let app = crate::status::router(crate::status::AppState {
            covers: cache.clone(),
            ..crate::status::tests_support::app_state()
        });

        // First request: cold cache, a thumbnail must actually be built —
        // otherwise `renditions_built` below would prove nothing (see
        // `a_file_thumbnail_is_only_built_once`).
        let resp = app
            .clone()
            .oneshot(Request::get("/api/cover/k?size=thumbnail").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let etag = resp
            .headers()
            .get(header::ETAG)
            .expect("the route must publish a validator")
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(cache.renditions_built(), 1, "the first request must build the thumbnail");

        // Second request, conditional on the ETag the first one just handed
        // out: nothing about the source changed, so the answer must be a
        // cheap 304 — and, crucially, one that never parsed the container
        // again to get there.
        let resp = app
            .oneshot(
                Request::get("/api/cover/k?size=thumbnail")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            cache.renditions_built(),
            1,
            "a 304 must not decode the embedded picture again — it never needed to"
        );
    }

    #[tokio::test]
    async fn concurrent_full_size_requests_extract_once() {
        // **The gap `renditions_in_flight` does not cover.** A bare URL (no
        // `?size=thumbnail`) asks for no rendition at all, so the rendezvous
        // one stage up never engages — before this test, `cover_get`'s
        // `CoverPayload::Embedded` branch called `read_embedded_bounded`
        // directly, unguarded, on a route reachable without authentication
        // from the LAN. Eight browsers enlarging the very same embedded
        // cover (`PlayerCard.vue`'s zoom) used to run eight independent
        // `lofty` parses of the whole container, each briefly holding the
        // full picture.
        //
        // Only a count of executions can prove this: comparing the eight
        // response bodies proves nothing, they are byte-identical whether
        // one extraction ran or eight — same reasoning as
        // `two_browsers_asking_at_the_same_instant_decode_the_image_once`,
        // one stage lower (raw extraction, not re-encoding).
        let dir = tempfile::tempdir().unwrap();
        let Some(track) =
            crate::player::mpv::tests::mp3_with_cover_from(dir.path(), "color=c=red:s=64x64:d=1")
        else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".into(), CoverPayload::Embedded(track)).await;

        // **The eight callers are made simultaneous, not hoped to be.** This
        // test used to spawn them and trust that the first would still be
        // inside `lofty` when the others showed up; with a 248-byte picture
        // it usually was not, which is the flake that turned main red. See
        // `CoverCache::extraction_hold` for the measurement and the reason a
        // seeded cell cannot express this property.
        let hold = Arc::new(tokio::sync::Semaphore::new(0));
        cache.hold_extractions(hold.clone());

        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let c = cache.clone();
                tokio::spawn(async move { served_body(&c, "k", "").await })
            })
            .collect();

        // Waiting on a state, never on a duration — no clock appears in this
        // test. The bound exists so that a caller which never reaches the
        // rendezvous *fails* the test instead of hanging it.
        let mut spins = 0;
        while cache.rendezvous_arrivals() < 8 {
            spins += 1;
            assert!(
                spins < 100_000,
                "only {} of the eight callers reached the rendezvous",
                cache.rendezvous_arrivals()
            );
            tokio::task::yield_now().await;
        }
        // **One permit per caller, not one in total.** A rendezvous that
        // stopped collapsing would have all eight callers waiting here; with
        // a single permit, seven would wait for ever and the suite would
        // hang instead of reporting a failure. Intact, the flight consumes
        // exactly one and the spare permits are never taken.
        hold.add_permits(8);

        let mut bodies = Vec::new();
        for t in tasks {
            let (status, body) = t.await.expect("no task may panic");
            assert_eq!(status, 200);
            bodies.push(body);
        }
        assert!(bodies.iter().all(|b| b == &bodies[0]), "all eight must get the same picture");
        assert_eq!(cache.embedded_extractions(), 1, "eight callers, one extraction");
    }

    #[tokio::test]
    async fn concurrent_full_size_requests_share_one_allocation() {
        // **What the extraction count above cannot see.** Sharing the
        // extraction is not the whole property this task exists to
        // establish: `EmbeddedInFlight` could still hand each waiter a
        // distinct copy of the *result* (an `Arc<Vec<u8>>` cloned into an
        // owned `Vec` for the response, as an earlier version of this fix
        // did) and `concurrent_full_size_requests_extract_once` would not
        // notice — equal bytes look the same whether they are one buffer or
        // eight. Only comparing pointers can tell them apart.
        //
        // Named production change this guards: swapping `extract_embedded`'s
        // shared type from `axum::body::Bytes` back to `Arc<Vec<u8>>` (with
        // `cover_get` cloning it out via `.as_ref().clone()` to build the
        // response, as `line`'s rendition branch does) would make each
        // waiter's `Bytes` wrap its own freshly allocated buffer — same
        // content, a different `as_ptr()` each time — while still passing
        // the extraction-count test above.
        let dir = tempfile::tempdir().unwrap();
        let Some(track) =
            crate::player::mpv::tests::mp3_with_cover_from(dir.path(), "color=c=blue:s=64x64:d=1")
        else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".into(), CoverPayload::Embedded(track.clone())).await;
        let cap = cache.settings().source_max;

        // Simultaneous by construction, exactly as in the test above: this
        // one held only because its first `spawn_blocking` has to create the
        // blocking-pool thread and so cannot help but yield — a property of
        // the pool being cold, not of the test. One earlier `spawn_blocking`
        // on this path would have taken it away.
        let hold = Arc::new(tokio::sync::Semaphore::new(0));
        cache.hold_extractions(hold.clone());

        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let c = cache.clone();
                let audio = track.clone();
                tokio::spawn(async move { c.extract_embedded("k", &audio, cap).await })
            })
            .collect();

        let mut spins = 0;
        while cache.rendezvous_arrivals() < 8 {
            spins += 1;
            assert!(
                spins < 100_000,
                "only {} of the eight callers reached the rendezvous",
                cache.rendezvous_arrivals()
            );
            tokio::task::yield_now().await;
        }
        // **One permit per caller, not one in total.** A rendezvous that
        // stopped collapsing would have all eight callers waiting here; with
        // a single permit, seven would wait for ever and the suite would
        // hang instead of reporting a failure. Intact, the flight consumes
        // exactly one and the spare permits are never taken.
        hold.add_permits(8);

        // **The buffers are kept alive, and that is load-bearing.** Reading
        // `as_ptr()` and dropping each `Bytes` in turn let the allocator hand
        // the next extraction the address the last one just freed: eight
        // separate copies would then compare equal and this test would pass
        // on the very regression it names. Holding all eight at once makes
        // one address per buffer impossible unless they really are one.
        let mut buffers = Vec::new();
        for t in tasks {
            let (_, bytes, _) =
                t.await.expect("no task may panic").expect("the picture must be readable");
            buffers.push(bytes);
        }
        let ptrs: Vec<_> = buffers.iter().map(|b| b.as_ptr()).collect();
        assert!(
            ptrs.iter().all(|p| *p == ptrs[0]),
            "all eight must share the very same buffer, not merely equal bytes"
        );
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
    async fn the_download_cap_cuts_a_chunked_stream_before_the_end() {
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
            download(&url, download_cap()).await.is_none(),
            "the cap must cut the stream chunk by chunk, without waiting for the end"
        );
    }

    /// **The cap is a setting, not a constant** — this is the property the
    /// production change of this task exists to make true. Same body served
    /// twice, two different caps: a hard-coded `NETWORK_CAP` would have
    /// produced the same verdict both times, since nothing about the request
    /// would have changed.
    #[tokio::test]
    async fn the_download_cap_follows_the_setting() {
        let body = vec![0u8; 1_500_000];
        let mut jpeg_body = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        jpeg_body.extend(body);
        let make_response = || {
            http_response(
                &format!("Content-Type: image/jpeg\r\nContent-Length: {}\r\n", jpeg_body.len()),
                jpeg_body.clone(),
            )
        };

        let url = serve(make_response(), true).await;
        assert!(
            download(&url, 1_000_000).await.is_none(),
            "a cap below the body's size must refuse it"
        );

        let url = serve(make_response(), true).await;
        assert!(
            download(&url, 2_000_000).await.is_some(),
            "the very same body must go through once the cap is raised above it"
        );
    }

    #[tokio::test]
    async fn a_refused_content_type_never_reads_the_body() {
        // The server never sends the body it announces: if `download` read it
        // despite the refused content-type, this wait would stay blocked until
        // the timeout below.
        let response = http_response("Content-Type: text/html\r\nContent-Length: 1000000\r\n", Vec::new());
        let url = serve(response, false).await;
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            download(&url, download_cap()),
        )
        .await
        {
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
            match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                download(&url, download_cap()),
            )
            .await
            {
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
            download(&url, download_cap()).await.is_none(),
            "the content declares `image/png` but the received bytes are not: must be refused"
        );
    }
    // -- The rendition: what the core builds before pushing -----------------

    /// A `Rendition` whose every field is named by the test using it: the
    /// product defaults (640 px, 150 KiB, 16 Mpx) would make most cases
    /// unreachable without fabricating huge images.
    fn test_rendition(max_edge_px: u32, passthrough_max: usize, pixel_cap: u64) -> Rendition {
        Rendition { max_edge_px, jpeg_quality: 85, passthrough_max, pixel_cap }
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
            source_max: 8 * 1024 * 1024,
            rendition: Some(test_rendition(64, 512 * 1024, 16_000_000)),
            ..CoverSettings::default()
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
            source_max: 8 * 1024 * 1024,
            rendition: Some(test_rendition(64, 512 * 1024, 16_000_000)),
            ..CoverSettings::default()
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

    /// **The whole point of the split, in one test.** A single number used to
    /// answer both questions, so raising it to stop covers being dropped also
    /// let heavy originals through untouched, and lowering it did the reverse.
    /// Here the two are set in opposition: a threshold small enough that the
    /// image must be re-encoded, and a net large enough that the result is
    /// kept. Under the old shared number this combination could not be
    /// expressed at all.
    #[tokio::test]
    async fn the_threshold_and_the_net_are_no_longer_the_same_number() {
        // 700 x 700, so wider than the 640 edge: it will be resized whatever
        // the threshold says.
        let source = fixtures::jpeg_decodable(700, 700);
        let r = Rendition {
            max_edge_px: 640,
            jpeg_quality: 85,
            // Tighter than anything the encoder could produce, so the
            // pass-through cannot be what returns a result here.
            passthrough_max: 16 * 1024,
            pixel_cap: 16_000_000,
        };
        let (mime, output) = rendition("image/jpeg", source.clone(), r)
            .await
            .expect("the net is derived from the edge and must not refuse a normal thumbnail");
        assert_eq!(mime, "image/jpeg");
        assert_ne!(output, source, "a 700 px image must have been re-encoded to 640");
        assert!(
            output.len() > r.passthrough_max,
            "the produced thumbnail is allowed to exceed the *threshold*: {} bytes against a \
             threshold of {} — that is exactly what the old shared number forbade",
            output.len(),
            r.passthrough_max,
        );
        assert!(output.len() <= r.net(), "and it must stay under the derived net");
    }

    #[test]
    fn the_net_is_derived_from_the_edge_with_a_floor() {
        let at = |edge: u32| Rendition {
            max_edge_px: edge,
            jpeg_quality: 85,
            passthrough_max: 150 * 1024,
            pixel_cap: 16_000_000,
        }
        .net();
        // 640^2 x 2 = 819_200 bytes: two bytes per pixel, against the 246 KiB
        // maximum -- 0.61 byte per pixel -- the bench observed at 640 px and
        // q90, a factor of about 3.3 over the heaviest cover it measured. One
        // byte per pixel left 1.6, which a quality above q90 can spend, and
        // the net firing at a valid setting drops the cover.
        //
        // Named production change this kills: going back to one byte per
        // pixel, and dropping either half of the `max`.
        assert_eq!(at(640), 819_200);
        // The floor covers the small edges, where the square would fall under
        // what even a tiny thumbnail can weigh.
        assert_eq!(at(64), 256 * 1024, "the floor must win on a small edge");
        assert_eq!(at(2048), 2048 * 2048 * 2, "and lose on a large one");
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
    /// the pass-through threshold. Only the pixel guard refuses it. The test
    /// therefore fails if the guard disappears **and** if it is moved after
    /// the pass-through — it is this second case that matters, because a bomb
    /// is precisely an image tiny in bytes and outsized in pixels.
    #[tokio::test]
    async fn the_pixel_cap_refuses_before_any_decoding_and_before_the_pass_through() {
        let source = fixtures::jpeg_decodable(100, 100);
        assert!(
            source.len() < 512 * 1024,
            "the fixture must fit under the pass-through threshold, otherwise the test does not \
             prove the order"
        );
        assert_eq!(
            rendition("image/jpeg", source, test_rendition(640, 512 * 1024, 1_000)).await,
            None,
            "10000 pixels beyond a cap of 1000 must be refused"
        );
    }

    #[tokio::test]
    async fn a_thumbnail_over_the_net_is_not_pushed() {
        // The net is derived (`Rendition::net`) and floored at 256 KiB: it is
        // no longer a field a test can dial down to an arbitrary tiny cap, so
        // triggering it for real takes an image the encoder cannot compress.
        // A gradient (the usual fixture) will not do -- that is the whole
        // point of the floor. Uniform random noise, PNG-encoded on the
        // lossless path (an alpha channel forces it), defeats DEFLATE and
        // lands close to its raw RGBA weight: at 600 px that is roughly
        // 1.4 MB against a derived net of 600 x 600 = 360 000 bytes.
        let width = 600u32;
        let height = 600u32;
        let mut img = image::RgbaImage::new(width, height);
        let mut state: u32 = 0xC0FF_EE11;
        for px in img.pixels_mut() {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let r = (state >> 24) as u8;
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let g = (state >> 24) as u8;
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let b = (state >> 24) as u8;
            *px = image::Rgba([r, g, b, 255]);
        }
        let mut source = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut source), image::ImageFormat::Png)
            .expect("fixture encoding");
        let r = Rendition { max_edge_px: width, jpeg_quality: 85, passthrough_max: 1, pixel_cap: 16_000_000 };
        assert_eq!(
            rendition("image/png", source, r).await,
            None,
            "random noise, losslessly re-encoded, must exceed the derived net and be refused"
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
        cache.set_cover_settings(CoverSettings { source_max: cap(), rendition: None, ..CoverSettings::default() });
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
    /// **A measurement bench, not an assertion.** Ignored by default: it needs
    /// a corpus of real album covers, which this repository does not ship.
    ///
    /// It is what the weight rule shown on the configuration page is derived
    /// from -- "at these settings a thumbnail weighs about N KiB". Guessing
    /// that figure would put a made-up number in front of the owner, and the
    /// whole point of the setting it feeds is that nobody can guess it.
    ///
    ///     COVER_CORPUS=/path/to/covers cargo test -p ritornello-core \
    ///         the_weight_rule_of_a_thumbnail -- --ignored --nocapture
    ///
    /// It calls `rendition` -- the real pipeline, not a replica -- so the
    /// figures it prints are the ones the appliance would actually produce.
    #[tokio::test]
    #[ignore = "needs a corpus of real covers, see COVER_CORPUS"]
    async fn the_weight_rule_of_a_thumbnail() {
        let Ok(dir) = std::env::var("COVER_CORPUS") else {
            println!("COVER_CORPUS unset, nothing measured");
            return;
        };
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("the corpus directory must be readable")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jpg")))
            .collect();
        files.sort();
        println!("corpus: {} files from {dir}\n", files.len());

        // Read once, not once per pair: the corpus lives on a share and the
        // measurement is about encoding, not about I/O.
        let sources: Vec<(std::path::PathBuf, Vec<u8>, u32)> = files
            .iter()
            .filter_map(|p| {
                let bytes = std::fs::read(p).ok()?;
                let (w, h) = dimensions(&bytes)?;
                Some((p.clone(), bytes, w.max(h)))
            })
            .collect();
        let mut edges: Vec<u32> = sources.iter().map(|(_, _, e)| *e).collect();
        edges.sort_unstable();
        println!(
            "long edge of the corpus: min {} / p50 {} / max {}",
            edges.first().copied().unwrap_or(0),
            edges.get(edges.len() / 2).copied().unwrap_or(0),
            edges.last().copied().unwrap_or(0),
        );
        println!("({} of {} decoded a header)\n", sources.len(), files.len());

        for (edge, quality) in
            [(320u32, 85u8), (512, 85), (640, 75), (640, 85), (640, 90), (1024, 85)]
        {
            // `passthrough_max` and `pixel_cap` are opened wide on purpose: the
            // question here is what the encoder *produces*, so a net that
            // refused an answer would remove that answer from the sample.
            let r = Rendition {
                max_edge_px: edge,
                jpeg_quality: quality,
                passthrough_max: usize::MAX,
                pixel_cap: 1_000_000_000,
            };
            let mut produced: Vec<usize> = Vec::new();
            let mut passed_through = 0usize;
            for (_, bytes, source_edge) in &sources {
                // Only an image actually re-encoded says anything about the
                // rule; one already smaller than `edge` would report its own
                // weight and flatten the figures towards zero.
                if *source_edge <= edge {
                    passed_through += 1;
                    continue;
                }
                if let Some((_, out)) = rendition("image/jpeg", bytes.clone(), r).await {
                    produced.push(out.len());
                }
            }
            produced.sort_unstable();
            if produced.is_empty() {
                println!("edge {edge:4} q{quality:<3}: nothing re-encoded");
                continue;
            }
            let k = |n: usize| n / 1024;
            println!(
                "edge {edge:4} q{quality:<3}: n={:3} (+{passed_through:3} already small)  \
                 min {:4}  p50 {:4}  p90 {:4}  max {:4}  KiB",
                produced.len(),
                k(produced[0]),
                k(produced[produced.len() / 2]),
                k(produced[produced.len() * 9 / 10]),
                k(produced[produced.len() - 1]),
            );
        }
    }
    /// **The other half of the bench above, and the one that chooses the
    /// pass-through threshold.** Same corpus, same invocation, but it looks at
    /// the population the first table *skips*: images already no wider than
    /// `max_edge`, which is exactly the population the threshold governs.
    ///
    /// For each of them it compares what the image already weighs against what
    /// re-compressing it would produce. That comparison is the whole question:
    /// above the threshold an image is re-encoded, and the honest threshold is
    /// the weight past which re-encoding actually buys something.
    ///
    /// `encode` is called directly rather than `rendition`, on purpose:
    /// `rendition` would hand these images straight back untouched -- that is
    /// the very short-circuit being measured around.
    #[tokio::test]
    #[ignore = "needs a corpus of real covers, see COVER_CORPUS"]
    async fn where_the_passthrough_threshold_belongs() {
        let Ok(dir) = std::env::var("COVER_CORPUS") else {
            println!("COVER_CORPUS unset, nothing measured");
            return;
        };
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("the corpus directory must be readable")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jpg")))
            .collect();
        files.sort();

        let pct = |v: &[usize], p: usize| v.get(v.len() * p / 100).copied().unwrap_or(0) / 1024;
        for (edge, quality) in [(512u32, 85u8), (640, 85), (1024, 85)] {
            let r = Rendition {
                max_edge_px: edge,
                jpeg_quality: quality,
                passthrough_max: usize::MAX,
                pixel_cap: 1_000_000_000,
            };
            let mut before: Vec<usize> = Vec::new();
            let mut after: Vec<usize> = Vec::new();
            let mut worth_it = 0usize;
            for p in &files {
                let Ok(bytes) = std::fs::read(p) else { continue };
                let Some((w, h)) = dimensions(&bytes) else { continue };
                if w.max(h) > edge {
                    continue; // measured by the other bench
                }
                let was = bytes.len();
                let Some((_, out)) = encode(bytes, r, (r.pixel_cap as usize) * 4) else { continue };
                // "Worth it" at a factor of two: re-encoding costs a decode
                // plus an encode on a Pi, so shaving a few percent off is not
                // a reason to spend that.
                if out.len() * 2 <= was {
                    worth_it += 1;
                }
                before.push(was);
                after.push(out.len());
            }
            before.sort_unstable();
            after.sort_unstable();
            if before.is_empty() {
                println!("edge {edge:4} q{quality:<3}: no image already small enough");
                continue;
            }
            println!(
                "edge {edge:4} q{quality:<3}: n={:3}  already weigh p50 {:4} p90 {:4} max {:4}  \
                 -> re-encoded p50 {:4} p90 {:4} max {:4}  KiB  \
                 ({worth_it} of {} at least halved)",
                before.len(),
                pct(&before, 50),
                pct(&before, 90),
                before[before.len() - 1] / 1024,
                pct(&after, 50),
                pct(&after, 90),
                after[after.len() - 1] / 1024,
                before.len(),
            );
        }
    }
}
