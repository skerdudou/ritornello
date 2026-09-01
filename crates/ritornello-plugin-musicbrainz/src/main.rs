//! `metadata` plugin: recognizes a disc against MusicBrainz, and also acts as
//! a generic cover relay for everything else.
//!
//! Two intentions live side by side in this single binary:
//! - the **disc path** only reacts to disc identities (`kind: "disc"`),
//!   queries MusicBrainz **once per disc**, and then emits one enrichment per
//!   track from what it learned. It knows the TOC, so it knows what is
//!   playing: it overwrites (`fill_only: false`).
//! - the **generic path** searches for a cover as soon as the core announces
//!   a known artist and album, whatever the Source. It knows nothing beyond
//!   what it was given, so it only **completes** (`fill_only: true`): the core
//!   loses nothing by ignoring its answer if another contributor already
//!   holds a cover.
//!
//! This code used to live in the cd plugin, where a network call of several
//! seconds shared the process that must answer track commands. Here, its
//! failure or slowness only affects the metadata.

mod admin;
mod icy;
mod patterns;
mod musicbrainz;
// Only compiled under `cargo test`: `ui_placeholder_js` is used at run-time
// nowhere in this crate, only by `build.rs` (separate compilation, via
// `include!`) and by its own tests. Compiling it permanently into the binary
// would trigger a `dead_code` that `-D warnings` would refuse (see
// `ritornello-plugin-mpd/src/main.rs`, same trap).
#[cfg(test)]
mod placeholder;

use anyhow::Result;
use musicbrainz::DiscInfo;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{MetadataPlugin, Runtime};
use ritornello_proto::{CoverRef, Enrichment, NowPlaying};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

/// Embedded i18n catalog of the admin page (`admin.rs`). Named like `MPD_EN`
/// on the mpd plugin side: this is the name `Catalog::load` embeds as a last
/// resort when no external pack is present.
pub(crate) const MUSICBRAINZ_EN: &str = include_str!("locales/en.toml");

/// **Consecutive** validation failures before reprobing an already known
/// station.
///
/// A track MusicBrainz does not know is a perfectly legitimate failure on a
/// correct pattern: reprobing at the first failure would start a probe on
/// every obscure title, and — since the reverse order sometimes also returns
/// an acceptable result — could replace a good pattern with a bad one on a
/// single stroke of luck. Three failures in a row describe a station that
/// changed shape, not a title the catalog ignores.
const FAILURES_BEFORE_REPROBE: u32 = 3;

/// The name under which the core knows a stream's header.
///
/// Declared as `derived_from` by both enrichments of the ICY path: this plugin
/// **splits** that string, it does not bring it. See `Enrichment::derived_from`.
const SOURCE_ICY: &str = "icy";

/// Delays of the deferred retries of a cover search, in seconds.
///
/// **Three, widely spaced.** `search_release` has already retried three times
/// internally (2 s then 4 s): what reaches here is an outage lasting longer
/// than a few seconds, not a hiccup.
///
/// Measured on the device on 2026-08-28: six 503s out of nine requests in one
/// minute, the cover only arriving at the sixth — thirty-six seconds after the
/// start of the track. The cadence, for its part, was compliant (1.1 s between
/// requests, shared throttler), so those 503s come from MusicBrainz's search
/// server and nothing we do will avoid them.
///
/// The third retry at three minutes is thus a safety net for a rough patch
/// that lasts: it costs one request every three minutes at worst, very far
/// from the one request per second the service allows, and it fits within the
/// duration of a track. Beyond that, the absence stops being an outage and the
/// track change remains the ultimate retry.
const COVER_RETRIES_S: &[u64] = &[20, 60, 180];

/// Result of a query: the TOC concerned, and what was found.
/// What a MusicBrainz query reports.
///
/// An enum and not an `Option`, and that is the fix for a measured defect:
/// "the service did not answer" and "it answered that it does not know" call
/// for two opposite treatments.
///
/// The second is **memorized** — that is even the whole point of this
/// plugin's caches: not asking twelve times in a row for an unknown disc. The
/// first must above all not be. An earlier version confused them behind an
/// `Option`, and a transient MusicBrainz 503 — their servers return some under
/// their own load, even at a compliant cadence — then froze into "this album
/// has no cover" until the plugin restarted. Symptom reported by the owner,
/// and reproduced: restarting the plugin made the cover appear.
#[derive(Debug, Clone, PartialEq)]
enum Answer<T> {
    /// MusicBrainz answered. `None` = it does not know, and that is final.
    Known(Option<T>),
    /// No usable answer after the bounded attempts. Nothing to memorize: the
    /// next pass will relaunch the search.
    Unavailable,
}

/// What a queried disc yielded, as it is **memorized**.
type Found = (String, Option<DiscInfo>);

/// What the disc query task sends through the channel.
type DiscOutcome = (String, Answer<DiscInfo>);

/// Pair identifying a search of the generic relay: artist, then album. It is
/// also the memorization key (see `MusicBrainzPlugin`).
type GenericKey = (String, String);

/// Result of a generic search: the pair concerned, and the MBID found.
type FoundCover = (GenericKey, Option<String>);

/// What the cover search task sends through the channel.
type CoverOutcome = (GenericKey, Answer<String>);

/// What a disc identity teaches this plugin.
#[derive(Debug, Clone, PartialEq)]
struct Disc {
    toc: String,
    track: usize,
}

/// Reads an opaque identity and only keeps a disc out of it if it describes
/// one.
///
/// Pure function: it is the entry point of data coming from another process,
/// hence the place where an unexpected shape must be discarded quietly rather
/// than make the plugin panic.
fn disc_of(identity: &Value) -> Option<Disc> {
    if identity.get("kind").and_then(Value::as_str)? != "disc" {
        return None;
    }
    let toc = identity.get("toc").and_then(Value::as_str)?.trim();
    if toc.is_empty() {
        return None;
    }
    // A disc identity without a track index is unusable: we would not know
    // which title to announce.
    let track = identity.get("track").and_then(Value::as_u64)? as usize;
    Some(Disc { toc: toc.to_string(), track })
}

/// Reads an opaque identity and only keeps the URL if it describes a stream.
///
/// Pure function, same contract as [`disc_of`]: an unexpected shape is
/// discarded quietly rather than make the plugin panic.
fn stream_url(identity: &Value) -> Option<String> {
    if identity.get("kind").and_then(Value::as_str)? != "stream" {
        return None;
    }
    let url = identity.get("url").and_then(Value::as_str)?.trim();
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

/// Should a cover be searched for this partial state?
///
/// An artist **and** an album, never a lone ICY title: the latter is raw
/// text, deliberately not split in this project, and OUI FM emits
/// `Titre - ARTISTE` in the reverse of the usual order — handing it to
/// MusicBrainz would confidently return anything.
///
/// And nothing to do if a cover is already held: this plugin **completes**,
/// so the call would be thrown away by the core's arbitration — a request
/// whose uselessness is known in advance.
fn should_search(known: &ritornello_proto::Known) -> bool {
    !known.cover && known.artist.is_some() && known.album.is_some()
}

struct MusicBrainzPlugin {
    /// Current identity, echoed back in every enrichment — it is the staleness
    /// guard on the core side.
    identity: Option<Value>,
    disc: Option<Disc>,
    /// Last disc queried: raw TOC → result (`None` = queried, nothing found).
    /// A single disc is enough: there is only one tray. Memorizing failures
    /// too avoids re-querying MusicBrainz at every track change of an unknown
    /// disc — twelve tracks, twelve useless requests.
    known: Option<Found>,
    /// TOC whose query is in flight, so as not to launch it twice.
    in_flight: Option<String>,
    /// Enrichment ready to go. One is enough: the two paths are mutually
    /// exclusive (an identity is a disc, or it is not).
    ready: Option<Enrichment>,
    found_tx: mpsc::Sender<DiscOutcome>,
    found_rx: mpsc::Receiver<DiscOutcome>,

    // --- Generic relay (file without a cover, stream whose textual metadata
    // is enough...) ---
    /// Current identity for this path, echoed back. `None` = nothing to
    /// complete right now (disc path active, artist/album not both known yet,
    /// or cover already held).
    generic_identity: Option<Value>,
    /// (artist, album) pair currently targeted. This is the chosen
    /// memorization key: it is exactly what the MusicBrainz request carries,
    /// it changes as soon as the album changes (so never a cover of another
    /// album surviving a track change), and it stays stable as long as the
    /// album does not change (so not one request per received frame). A
    /// Source identity would not do: it can stay fixed while artist/album
    /// arrive over several frames (ICY), or change without the album changing
    /// (next track of the same disc of files).
    generic_key: Option<GenericKey>,
    /// Last pair searched, and the cover URL found (`None` = search done,
    /// nothing found). Memorizing failures too avoids re-querying MusicBrainz
    /// at every frame as long as the album does not change.
    known_cover: Option<FoundCover>,
    /// Pair whose search is in flight, so as not to launch it twice.
    cover_in_flight: Option<GenericKey>,
    /// The pair currently under **deferred retry** and the number of retries
    /// already consumed. See [`MusicBrainzPlugin::reschedule_cover`].
    cover_retries: Option<(GenericKey, usize)>,
    cover_tx: mpsc::Sender<CoverOutcome>,
    cover_rx: mpsc::Receiver<CoverOutcome>,

    // --- ICY path (radio) ---
    /// The store, **shared with the admin page**: both halves of the process
    /// read and write it, as both halves of the radio plugin share its state
    /// file.
    store: Arc<RwLock<patterns::Store>>,
    state_path: PathBuf,
    /// Last raw string handled. Icecast repeats the same header throughout a
    /// track: without this guard, every repetition would relaunch a request.
    icy_seen: Option<String>,
    /// **Consecutive** validation failures, per stream URL. In memory and not
    /// persisted: it is a sequence of events in progress, not an established
    /// fact about the station, and a restart is a legitimate reset.
    failures: HashMap<String, u32>,
    /// URL whose handling is in flight, so as not to launch it twice.
    icy_in_flight: Option<String>,
    icy_tx: mpsc::Sender<IcyOutcome>,
    icy_rx: mpsc::Receiver<IcyOutcome>,
}

/// What an ICY handling task reports, in **one single** message.
///
/// One message and not two ("here is the pattern", "here is the pair"): the
/// loop must be able to update the store, the failure counter and the
/// enrichment in the same turn, without an intermediate state where the
/// pattern would be kept but the counter not yet reset.
#[derive(Debug)]
struct IcyOutcome {
    url: String,
    /// The identity **received** from the core, carried along with the work
    /// to be echoed back.
    ///
    /// In the message and not in a plugin field, and both reasons matter:
    ///
    /// * **Received, not rebuilt.** This echo is the core's staleness guard,
    ///   which compares the *whole* value. Rebuilding it from the URL is right
    ///   today and wrong as soon as a source enriches its identity — a preset
    ///   number would be natural — and the failure mode would be a
    ///   **silent** rejection of every enrichment.
    /// * **Attached to the work, not to the plugin.** A field of `self` would
    ///   be overwritten by a more recent frame while a handling is still in
    ///   flight, and the outcome of an old track would leave with the new
    ///   one's identity. Making it travel ties the echo to what it describes.
    identity: Value,
    /// The string handled. Serves as a staleness guard: an outcome that does
    /// not describe the current string is thrown away, as the two other paths
    /// throw away an answer that no longer describes what is playing.
    raw: String,
    /// The pattern to keep when a probe took place. `None` = no probe
    /// (steady state), so nothing to learn.
    pattern: Option<patterns::Pattern>,
    /// The validated pair and its cover. `None` = validation failed.
    validated: Option<(String, String, Option<String>)>,
    /// The pair from the **local** split, whether validation succeeded or
    /// not.
    ///
    /// Distinct from `validated`, and the distinction carries a review fix: a
    /// track MusicBrainz does not know is a **validation** failure, not a
    /// reason to throw away a split whose pattern has already proven itself on
    /// this station. Without this field, the plugin emitted nothing in that
    /// case — and since a radio's identity does not change from one track to
    /// the next, the enrichment of the **previous** track remained the winner:
    /// the screen announced the previous artist, title and cover for the whole
    /// duration of the next one.
    pair: Option<(String, String)>,
}

impl MusicBrainzPlugin {
    fn new(store: Arc<RwLock<patterns::Store>>, state_path: PathBuf) -> Self {
        let (found_tx, found_rx) = mpsc::channel(4);
        let (cover_tx, cover_rx) = mpsc::channel(4);
        let (icy_tx, icy_rx) = mpsc::channel(4);
        Self {
            identity: None,
            disc: None,
            known: None,
            in_flight: None,
            ready: None,
            found_tx,
            found_rx,
            generic_identity: None,
            generic_key: None,
            known_cover: None,
            cover_in_flight: None,
            cover_retries: None,
            cover_tx,
            cover_rx,
            store,
            state_path,
            icy_seen: None,
            failures: HashMap::new(),
            icy_in_flight: None,
            icy_tx,
            icy_rx,
        }
    }

    /// Prepares the enrichment of the current track if the disc is known.
    fn prepare(&mut self) {
        let (Some(identity), Some(disc)) = (&self.identity, &self.disc) else { return };
        let Some((toc, Some(info))) = &self.known else { return };
        if toc != &disc.toc {
            return;
        }
        let Some(title) = info.tracks.get(disc.track) else {
            // Index out of bounds: the recognized disc does not have that many
            // tracks. Better to stay silent than announce another track's title.
            tracing::info!("track {} beyond the {} known titles", disc.track, info.tracks.len());
            return;
        };
        self.ready = Some(Enrichment {
            identity: identity.clone(),
            artist: Some(info.artist.clone()),
            title: Some(title.clone()),
            album: Some(info.album.clone()),
            // The TOC lookup carries the pressing date: the year is thus free
            // on the disc path, without one more request.
            year: info.year,
            // MusicBrainz would give the durations with `inc=recordings`, but
            // the duration is not displayed: nothing justifies a heavier request.
            duration_s: None,
            // This plugin does not know where playback stands: it answers on
            // a track's identity, not on its progress.
            position_s: None,
            // The TOC lookup already carried what is needed to build the URL,
            // and the choice of level (this pressing, or the album failing a
            // front cover) was made at parse time. No additional request here.
            cover: info.cover_url.clone().map(|url| CoverRef::Url { url }),
            // Disc path: the TOC says what is playing, so it overwrites (default).
            ..Default::default()
        });
    }

    /// Prepares the generic enrichment for the (artist, album) pair currently
    /// targeted: the cover found, or the admission of having found nothing.
    ///
    /// **The second case is also an answer**, and that is what was missing:
    /// "MusicBrainz has no cover for this album" and "MusicBrainz was never
    /// queried" looked the same on screen — that is, not at all. An
    /// enrichment carrying `searched` and nothing else is the only one the
    /// core accepts empty; it enters no arbitration and only adds one line to
    /// the origins.
    ///
    /// Not to be confused with an **outage**: that one emits nothing and
    /// reschedules itself (see `reschedule_cover`). What reaches here is an
    /// actual answer from the service.
    fn prepare_generic(&mut self) {
        let (Some(identity), Some(key)) = (&self.generic_identity, &self.generic_key) else {
            return;
        };
        let Some((known, found_url)) = &self.known_cover else { return };
        if known != key {
            return;
        }
        let Some(cover_url) = found_url else {
            self.ready = Some(Enrichment {
                identity: identity.clone(),
                searched: true,
                // `fill_only` for honesty of form: this contributor brings
                // nothing, so it cannot want to overwrite anything. No
                // practical effect — an empty enrichment is excluded from
                // arbitration on both sides — but a default `false` would
                // mean "I overwrite", which would be wrong.
                fill_only: true,
                ..Default::default()
            });
            return;
        };
        self.ready = Some(Enrichment {
            identity: identity.clone(),
            // URL already resolved by `search_release`: this path rebuilds
            // nothing. A search carries no `cover-art-archive` block, so what
            // comes out is the album's cover.
            cover: Some(CoverRef::Url { url: cover_url.clone() }),
            // It searched, and it found: say so too, so the origins know it
            // was queried.
            searched: true,
            // This path knows nothing beyond what it was given: it only
            // completes, never overwrites an already filled field.
            fill_only: true,
            ..Default::default()
        });
    }

    /// Launches the cover search for this (artist, album) pair, once only —
    /// same pattern as [`Self::search`] for the disc.
    fn search_cover(&mut self, key: GenericKey) {
        if self.cover_in_flight.as_ref() == Some(&key) {
            return;
        }
        if self.known_cover.as_ref().is_some_and(|(known_key, _)| known_key == &key) {
            return; // already searched, result memorized (found or not)
        }
        self.start_cover_search(key, Duration::ZERO);
    }

    /// Reschedules the search after a MusicBrainz outage, as long as the
    /// retry budget is not exhausted.
    ///
    /// **What this fixes, and why the three attempts were not enough.**
    /// `search_release` already retries three times internally, at 2 s then
    /// 4 s — a handful of seconds in all. If the outage lasts longer, the
    /// answer is `Unavailable`, nothing is memorized (which is right: a 503
    /// must not become "this album has no cover")... and **nothing restarts
    /// anymore**. The comment back then said "the next frame will retry", but
    /// there is no next frame: the core only republishes `NowPlaying` when the
    /// identity or `known` change (see `publish_state`), and on a local file
    /// both freeze as soon as the tags are read. The symptom reported by the
    /// owner is exactly that one: nothing for ten seconds, then the cover
    /// appears **at the track change** — that is, at the only occasion that
    /// relaunched anything.
    ///
    /// Two retries and no more: beyond that, the absence is no longer a
    /// transient outage, and hammering a free third-party service for an image
    /// would be abuse. The track change remains the ultimate retry, as before.
    ///
    /// Only retries the pair **still targeted**: a retry for an album no
    /// longer being listened to is pure wasted work, and its answer would be
    /// discarded by the staleness guard anyway.
    fn reschedule_cover(&mut self, key: GenericKey) {
        let Some((rank, timeout)) = self.retry_due(&key) else {
            tracing::info!("MusicBrainz still unavailable, giving up until the track changes");
            return;
        };
        self.cover_retries = Some((key.clone(), rank + 1));
        self.start_cover_search(key, timeout);
    }

    /// The rank of the next retry for this pair and its delay, or `None` —
    /// budget exhausted, or pair that is no longer the one targeted.
    ///
    /// The rank comes out of here rather than being recomputed by the caller:
    /// both values come from the same reading, and separating them would let
    /// a counter advance on a rank another pair had set.
    ///
    /// **Separated from its application**, and that is what makes it
    /// verifiable: the retry itself sleeps then queries a third-party
    /// service, so testing it end to end would require a network and a clock.
    /// The decision, for its part, only reads two fields.
    ///
    /// The counter is carried **by the pair**: a different album starts from
    /// zero without any explicit reset having to exist, hence without a path
    /// where to forget it.
    fn retry_due(&self, key: &GenericKey) -> Option<(usize, Duration)> {
        if self.generic_key.as_ref() != Some(key) {
            return None;
        }
        let rank = match &self.cover_retries {
            Some((previous, rank)) if previous == key => *rank,
            _ => 0,
        };
        COVER_RETRIES_S.get(rank).map(|s| (rank, Duration::from_secs(*s)))
    }

    /// The flight itself, possibly preceded by a wait.
    ///
    /// **`cover_in_flight` is armed before the wait**, not after: otherwise a
    /// frame arriving during the pause would relaunch a second search for the
    /// same pair, and the two would answer each other.
    fn start_cover_search(&mut self, key: GenericKey, delay: Duration) {
        self.cover_in_flight = Some(key.clone());
        let (artist, album) = key.clone();
        let tx = self.cover_tx.clone();
        // The start of a search, dated. With the throttler, the three internal
        // attempts and their ten-second delays, the time between a track's
        // announcement and the arrival of its cover sometimes counts in tens
        // of seconds: without this line, that delay was only observable on
        // the screen, and thus not attributable.
        if delay.is_zero() {
            tracing::info!("MusicBrainz: looking for a cover for {artist} — {album}");
        } else {
            tracing::info!("MusicBrainz: retrying {artist} — {album} in {delay:?}");
        }
        tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            // Timed, and the outcome named: "found", "nothing found" and "no
            // answer" are finally told apart, with the time each one cost. The
            // throttler, the three internal attempts and their ten-second
            // delays can add up to tens of seconds — that is the hypothesis to
            // confirm or dismiss.
            let started = std::time::Instant::now();
            let answer = match musicbrainz::search_release(&artist, &album).await {
                Ok(url) => {
                    let outcome = if url.is_some() { "cover found" } else { "no cover" };
                    tracing::info!(
                        "MusicBrainz: {outcome} for {artist} — {album} after {:?}",
                        started.elapsed()
                    );
                    Answer::Known(url)
                }
                Err(e) => {
                    tracing::info!(
                        "MusicBrainz release search unavailable after {:?}: {e}",
                        started.elapsed()
                    );
                    Answer::Unavailable
                }
            };
            let _ = tx.send((key, answer)).await;
        });
    }

    /// Launches the query of an unknown disc, once only.
    fn search(&mut self, toc: String) {
        if self.in_flight.as_deref() == Some(toc.as_str()) {
            return;
        }
        if let Some((known_toc, _)) = &self.known {
            if known_toc == &toc {
                return; // already queried, result memorized (found or not)
            }
        }
        let param = match musicbrainz::mb_toc_param(&toc) {
            Ok(p) => p,
            Err(e) => {
                // Dubious TOC: we do not call a third-party service for nothing.
                tracing::info!("unusable TOC, no call made: {e}");
                return;
            }
        };
        // The first field of the TOC **is** the number of tracks, and
        // `mb_toc_param` just checked that it agrees with the offsets.
        let ntracks = toc.split_whitespace().next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        self.in_flight = Some(toc.clone());
        let tx = self.found_tx.clone();
        tokio::spawn(async move {
            let answer = match musicbrainz::lookup(&param, ntracks).await {
                Ok(info) => Answer::Known(info),
                Err(e) => {
                    tracing::info!("MusicBrainz lookup unavailable: {e}");
                    Answer::Unavailable
                }
            };
            let _ = tx.send((toc, answer)).await;
        });
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for MusicBrainzPlugin {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Any announcement makes the prepared enrichment stale: it carried the
        // previous identity, and the core would throw it away anyway.
        self.ready = None;
        let disc = np.identity.as_ref().and_then(disc_of);
        match disc {
            Some(disc) => {
                self.identity = np.identity;
                // The disc path is exclusive: on a disc, nothing for the
                // generic relay to complete.
                self.generic_identity = None;
                self.generic_key = None;
                let toc = disc.toc.clone();
                self.disc = Some(disc);
                self.search(toc);
                self.prepare();
            }
            None => {
                // Neither a disc nor a stop: a file or radio stream identity,
                // for instance. The disc path stays silent — that is another
                // plugin's business — but the generic relay may have enough to
                // search for a cover.
                self.identity = None;
                self.disc = None;
                // Captured before the generic handling below moves
                // `np.identity`: the ICY path needs them afterwards.
                let icy_url = np.identity.as_ref().and_then(stream_url);
                let stream_title = np.known.stream_title.clone();
                // Cloned here, with its neighbours, because the `match` below
                // moves `np.identity`: this very value is what will be echoed
                // back, never a reconstruction. See `IcyOutcome::identity`.
                let stream_identity = np.identity.clone();
                match np.identity {
                    Some(identity) if should_search(&np.known) => {
                        let key = (
                            np.known.artist.expect("checked by should_search"),
                            np.known.album.expect("checked by should_search"),
                        );
                        self.generic_identity = Some(identity);
                        self.generic_key = Some(key.clone());
                        self.search_cover(key);
                        self.prepare_generic();
                    }
                    _ => {
                        self.generic_identity = None;
                        self.generic_key = None;
                    }
                }

                // --- ICY path: after the generic handling above, without
                // touching it --------------------------------------------
                //
                // Triggered on a change of `stream_title`, not on every frame:
                // Icecast repeats the same header throughout a track, and
                // re-handling it every time would be a request for nothing.
                if let Some(url) = icy_url {
                    if stream_title != self.icy_seen {
                        self.icy_seen = stream_title.clone();
                        if let Some(raw) = stream_title {
                            // `icy_in_flight` prevents launching a second
                            // handling for the same URL while a first one is
                            // still in flight; the staleness guard in
                            // `next_enrichment` filters an answer that became
                            // off-topic during the flight.
                            if self.icy_in_flight.as_deref() != Some(url.as_str()) {
                                self.icy_in_flight = Some(url.clone());
                                // **A station with a manual pattern is never
                                // reprobed.** The store did refuse to rewrite
                                // the entry (`Store::learn`), but nothing
                                // prevented the probe from starting — and then
                                // it was *its* split that got displayed, not
                                // the operator's. The documentation was thus
                                // true of the file and false of the screen.
                                // Consulting the origin here closes the gap at
                                // the source: if the operator decided, we apply
                                // what they set, even when MusicBrainz does not
                                // want it.
                                let is_manual = self
                                    .store
                                    .read()
                                    .await
                                    .entry(&url)
                                    .map(|e| e.origin == patterns::Origin::Manual)
                                    .unwrap_or(false);
                                let reprobe = !is_manual && should_reprobe(&self.failures, &url);
                                if reprobe {
                                    // **The reprobe consumes the counter.**
                                    // Without that it stayed above the
                                    // threshold for the life of the process: a
                                    // station that never validates — a
                                    // mojibake stream, for instance — went
                                    // back into a full probe at *every* title,
                                    // which contradicted the documentation and
                                    // made this limit a guaranteed request
                                    // storm. A reprobe buys three titles, it
                                    // does not stay armed permanently.
                                    self.failures.remove(&url);
                                }
                                let store = self.store.clone();
                                let tx = self.icy_tx.clone();
                                let task_url = url.clone();
                                // `stream_url` already recognized the identity,
                                // so it is there: the `unwrap_or` is only type
                                // totality, not a use case.
                                let identity = stream_identity.clone().unwrap_or(Value::Null);
                                tokio::spawn(async move {
                                    let known = store.read().await.entry(&task_url).map(|e| e.pattern.clone());
                                    let outcome = handle_icy(task_url, raw, identity, known, reprobe).await;
                                    let _ = tx.send(outcome).await;
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            if let Some(e) = self.ready.take() {
                return e;
            }
            // `select!` over two `recv` stays cancellable without loss: if a
            // `NowPlaying` arrives first, the runner drops this future and no
            // result is lost — each branch only mutates `self` once its
            // message is received, never before (the durable state lives in
            // `self`, not in this future's local variables).
            tokio::select! {
                r = self.found_rx.recv() => match r {
                    Some((toc, answer)) => {
                        if self.in_flight.as_deref() == Some(toc.as_str()) {
                            self.in_flight = None;
                        }
                        // A transient outage is not memorized: `in_flight` was
                        // just released, so the next track change will relaunch
                        // the query of this disc.
                        let Answer::Known(info) = answer else { continue };
                        // A result is only kept if it describes the tracked
                        // disc: two lookups can cross during a quick swap of
                        // discs (A in flight, B inserted, B's answer then A's),
                        // and keeping the latecomer overwrote the current
                        // disc's cache — `prepare()` protected the display, but
                        // the next track change relaunched a MusicBrainz
                        // request for nothing.
                        if self.disc.as_ref().is_some_and(|d| d.toc == toc) {
                            self.known = Some((toc, info));
                            self.prepare();
                        }
                    }
                    // Impossible in practice (the plugin keeps a Sender): do
                    // not yield rather than loop on empty.
                    None => std::future::pending().await,
                },
                r = self.cover_rx.recv() => match r {
                    Some((key, answer)) => {
                        if self.cover_in_flight.as_ref() == Some(&key) {
                            self.cover_in_flight = None;
                        }
                        // A transient outage is not memorized: a MusicBrainz
                        // 503 must not become "this album has no cover" for
                        // the whole duration of the album.
                        //
                        // **And it is rescheduled**, which was missing: nothing
                        // relaunched the search as long as the track did not
                        // change, for want of a new frame to wait for (see
                        // `reschedule_cover`).
                        let Answer::Known(cover_url) = answer else {
                            self.reschedule_cover(key);
                            continue;
                        };
                        // Same guard as on the disc side: only keep the result
                        // if it describes the (artist, album) pair still
                        // targeted — a track change may have made the in-flight
                        // search obsolete while it was flying.
                        if self.generic_key.as_ref() == Some(&key) {
                            self.known_cover = Some((key, cover_url));
                            self.prepare_generic();
                        }
                    }
                    None => std::future::pending().await,
                },
                r = self.icy_rx.recv() => match r {
                    Some(outcome) => {
                        if self.icy_in_flight.as_deref() == Some(outcome.url.as_str()) {
                            self.icy_in_flight = None;
                        }
                        // **The pattern is kept before the staleness guard**,
                        // and the order is the fix: a pattern describes the
                        // **station**, not the track. A probe outcome that
                        // became stale during its flight — the station changed
                        // title, which takes a few seconds, and the probe takes
                        // four — still carries a valid lesson, verified against
                        // MusicBrainz.
                        //
                        // Throwing away the whole outcome before this line, as
                        // the previous version did, could make a station
                        // **never learn anything**: every probe was invalidated
                        // by the title change that had partly caused it.
                        if let Some(m) = outcome.pattern {
                            let mut store = self.store.write().await;
                            store.learn(&outcome.url, m);
                            if let Err(e) = store.save(&self.state_path) {
                                tracing::warn!("could not save ICY patterns: {e}");
                            }
                        }
                        // Staleness guard, like the two other paths, but it
                        // now only protects what describes **the track**: the
                        // pair and the cover.
                        if self.icy_seen.as_deref() != Some(outcome.raw.as_str()) {
                            continue;
                        }
                        match outcome.validated {
                            Some((artist, title, cover_url)) => {
                                {
                                    let mut store = self.store.write().await;
                                    store.record_success(&outcome.url);
                                    if let Err(e) = store.save(&self.state_path) {
                                        tracing::warn!("could not save ICY patterns: {e}");
                                    }
                                }
                                self.failures.remove(&outcome.url);
                                self.ready = Some(Enrichment {
                                    // The **received** identity, carried over
                                    // as is. See `IcyOutcome::identity`, which
                                    // says why it travels with the work.
                                    identity: outcome.identity,
                                    // **The station remains the source.** This
                                    // plugin split its string and verified the
                                    // split, it taught the track to nobody:
                                    // claiming the title would erase whoever
                                    // announced it. The core notes separately
                                    // who reworked it.
                                    derived_from: Some(SOURCE_ICY.to_string()),
                                    artist: Some(artist),
                                    title: Some(title),
                                    // URL already resolved by `first_recording`.
                                    cover: cover_url.map(|url| CoverRef::Url { url }),
                                    // This path **replaces** the raw ICY
                                    // string, which is precisely what is being
                                    // corrected — unlike the neighbouring
                                    // generic relay (`fill_only: true`), which
                                    // only completes because it knows nothing
                                    // beyond what it was given. Here we
                                    // overwrite, and only what MusicBrainz just
                                    // confirmed.
                                    fill_only: false,
                                    ..Default::default()
                                });
                            }
                            None => {
                                *self.failures.entry(outcome.url.clone()).or_default() += 1;
                                // **Emit anyway.** Emitting nothing let the
                                // enrichment of the *previous* track win the
                                // arbitration, a radio's identity not changing
                                // from one track to the next: the screen
                                // announced the previous artist, title and
                                // cover for the whole duration of the next
                                // one. The worst of the three states, and the
                                // one my spec described without seeing it when
                                // writing "emit nothing for this track".
                                //
                                // What is emitted depends on what is known:
                                //
                                // * the local pair, when the pattern applies.
                                //   MusicBrainz does not know this track, which
                                //   says nothing against a split already
                                //   confirmed on this station. No cover, for
                                //   want of a release to cite.
                                // * otherwise the cleaned string as the title:
                                //   the pattern no longer applies (the station
                                //   changed shape) or there is none. No split
                                //   is asserted then — just what the stream
                                //   announces, stripped of its advertising.
                                let (artist, title) = match outcome.pair {
                                    Some((a, t)) => (Some(a), Some(t)),
                                    None => (None, Some(icy::clean(&outcome.raw))),
                                };
                                self.ready = Some(Enrichment {
                                    identity: outcome.identity,
                                    // Even truer here than above: this path
                                    // carries **only** the local split,
                                    // MusicBrainz having validated nothing at
                                    // all. The title comes from the station,
                                    // word for word.
                                    derived_from: Some(SOURCE_ICY.to_string()),
                                    artist,
                                    title,
                                    fill_only: false,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    None => std::future::pending().await,
                },
            }
        }
    }
}

/// Should the station be reprobed?
///
/// Extracted as a pure function for the same reason as [`best_accepted`]: the
/// network is not reachable in tests, so it is the **decision** that must be
/// tested, not the probe it triggers. The threshold is in **consecutive**
/// failures: see [`FAILURES_BEFORE_REPROBE`].
fn should_reprobe(failures: &HashMap<String, u32>, url: &str) -> bool {
    failures.get(url).copied().unwrap_or(0) >= FAILURES_BEFORE_REPROBE
}

/// Diagnoses a dubious encoding, without repairing it.
///
/// A mojibake title will **never** validate against MusicBrainz, and would
/// otherwise look like a bad split while the split was right: without this
/// distinct diagnosis, the defect would be looked for on the wrong side.
fn warn_dubious_encoding(raw: &str) {
    // `U+FFFD`: the replacement character that a UTF-8 decoding forced on
    // bytes that are not UTF-8 leaves behind.
    if raw.contains('\u{FFFD}') {
        tracing::warn!("ICY stream title looks mis-decoded (replacement character present): {raw:?}");
        return;
    }
    // Characteristic sequence of text re-read in the wrong character set: the
    // two bytes of a UTF-8 accented character (lead 0xC2/0xC3, then a
    // continuation byte 0x80-0xBF) re-read elsewhere as "Â"/"Ã" followed by a
    // Latin-1 Supplement symbol — "Ã©" for an "é", for instance.
    let dubious =
        raw.chars().zip(raw.chars().skip(1)).any(|(a, b)| matches!(a, 'Â' | 'Ã') && ('\u{80}'..='\u{BF}').contains(&b));
    if dubious {
        tracing::warn!("ICY stream title looks mis-decoded (latin-1/UTF-8 mismatch): {raw:?}");
    }
}

/// Is a candidate validated by this answer?
///
/// Both conditions matter: the score alone is too generous, the MusicBrainz
/// search almost always returning something plausible. The normalized title
/// equality is the guard that carries everything.
fn validated(candidate_title: &str, e: &musicbrainz::Recording) -> bool {
    e.score >= musicbrainz::RECORDING_THRESHOLD && musicbrainz::normalize(&e.title) == musicbrainz::normalize(candidate_title)
}

/// Picks the best accepted candidate among answers already obtained.
///
/// Deliberately separated from the network: it is the decision, and that is
/// what must be tested. The pairs are `(candidate, answer)`, in attempt order.
fn best_accepted(attempts: &[(icy::Candidate, Option<musicbrainz::Recording>)]) -> Option<&icy::Candidate> {
    attempts
        .iter()
        .filter_map(|(c, answer)| answer.as_ref().filter(|e| validated(&c.title, e)).map(|e| (c, e.score)))
        .max_by_key(|(_, score)| *score)
        .map(|(c, _)| c)
}

/// Validates a pair already split locally, through a recording search.
///
/// This is the continuous validation of the steady state (see the module
/// doc): it also serves to find the cover, which a radio never announces
/// otherwise.
async fn validated_by_search(artist: &str, title: &str) -> Option<(String, String, Option<String>)> {
    let answer = musicbrainz::search_recording(artist, title)
        .await
        .unwrap_or_else(|e| {
            tracing::info!("MusicBrainz recording search: {e}");
            None
        })?;
    if validated(title, &answer) {
        Some((artist.to_string(), title.to_string(), answer.cover_url))
    } else {
        None
    }
}

/// Handles an ICY string: applies the known pattern, or probes the station.
///
/// Detached in a task, like the two other paths: a station can cost four
/// requests spaced one second apart, and the plugin loop must not wait.
async fn handle_icy(
    url: String,
    raw: String,
    identity: Value,
    known: Option<patterns::Pattern>,
    reprobe: bool,
) -> IcyOutcome {
    warn_dubious_encoding(&raw);
    let cleaned = icy::clean(&raw);

    if !reprobe {
        match &known {
            Some(patterns::Pattern::DoNotSplit) => {
                // The talk station: zero cost, no request.
                return IcyOutcome { url, raw, identity, pattern: None, validated: None, pair: None };
            }
            Some(m @ patterns::Pattern::Split { .. }) => {
                // Steady state: local split, one single request that counts
                // both as continuous validation and as cover search.
                //
                // The local pair is reported **even if validation fails**: it
                // is our best knowledge of the track, and the pattern that
                // produced it has already been confirmed on this station. See
                // `IcyOutcome::pair`.
                let pair = icy::apply(m, &cleaned);
                let validated = match &pair {
                    Some((artist, title)) => validated_by_search(artist, title).await,
                    None => None,
                };
                return IcyOutcome { url, raw, identity, pattern: None, validated, pair };
            }
            None => {} // Station never probed: falls through to the probe below.
        }
    }

    // Probe: unknown station, or reprobe triggered by three failures in a row.
    let candidates = icy::candidates(&cleaned);
    let mut attempts = Vec::with_capacity(candidates.len());
    for c in candidates {
        let answer = musicbrainz::search_recording(&c.artist, &c.title).await.unwrap_or_else(|e| {
            tracing::info!("MusicBrainz recording search: {e}");
            None
        });
        attempts.push((c, answer));
    }
    let tried_count = attempts.len();
    // A silent cap reads as "everything was tried": say so when the number of
    // probed candidates hits the cap of icy::candidates.
    if tried_count >= icy::MAX_CANDIDATES {
        tracing::info!(
            "ICY probe for {url}: hit the {}-candidate cap, some derivable candidates may not have been tried",
            icy::MAX_CANDIDATES
        );
    }
    match best_accepted(&attempts).cloned() {
        Some(winner) => {
            let score = attempts.iter().find(|(c, _)| *c == winner).and_then(|(_, r)| r.as_ref()).map(|e| e.score);
            let cover_url =
                attempts.iter().find(|(c, _)| *c == winner).and_then(|(_, r)| r.as_ref()).and_then(|e| e.cover_url.clone());
            tracing::info!(
                "ICY probe for {url}: tried {tried_count} candidate(s), kept \"{}\" / \"{}\" (score {:?})",
                winner.artist,
                winner.title,
                score
            );
            IcyOutcome {
                url,
                raw,
                identity,
                pattern: Some(patterns::Pattern::from_candidate(&winner)),
                validated: Some((winner.artist.clone(), winner.title.clone(), cover_url)),
                pair: Some((winner.artist, winner.title)),
            }
        }
        None => {
            tracing::info!("ICY probe for {url}: tried {tried_count} candidate(s), none accepted");
            IcyOutcome {
                url,
                raw,
                identity,
                pattern: Some(patterns::Pattern::DoNotSplit),
                validated: None,
                pair: None,
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let state_path = PathBuf::from(
        std::env::var("RITORNELLO_MUSICBRAINZ_STATE")
            .unwrap_or_else(|_| "/var/lib/ritornello/plugin-musicbrainz.json".to_string()),
    );
    let store = Arc::new(RwLock::new(patterns::Store::load(&state_path)));

    // A `metadata` plugin receives no `SetLocale` frame (it only exists for
    // `SourcePlugin`): the admin page's language thus comes from the
    // environment at launch, as in generic-input and mpd — a change of the
    // device's language only shows there after a plugin restart (see the doc
    // of `admin::MusicBrainzAdmin`).
    let locales_root = PathBuf::from(
        std::env::var("RITORNELLO_LOCALES").unwrap_or_else(|_| "/etc/ritornello/locales".to_string()),
    );
    let locale = std::env::var("RITORNELLO_LOCALE").unwrap_or_else(|_| "en".to_string());
    let catalog = Arc::new(std::sync::RwLock::new(Catalog::load(
        "musicbrainz",
        &locale,
        &locales_root,
        MUSICBRAINZ_EN,
    )));

    Runtime::from_args()?
        .metadata(MusicBrainzPlugin::new(store.clone(), state_path.clone()))?
        .admin(admin::MusicBrainzAdmin::new(store, state_path, catalog))?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn embedded_musicbrainz_en_is_not_empty() {
        assert!(!ritornello_i18n::try_parse(MUSICBRAINZ_EN).unwrap().is_empty());
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/mb_discid.json");
    const TOC: &str = "3 150 22767 41887 63000";

    fn disc_identity(track: u64) -> Value {
        json!({ "kind": "disc", "toc": TOC, "tracks": 3, "track": track })
    }

    fn file_identity(path: &str) -> Value {
        json!({ "kind": "file", "path": path })
    }

    /// A fresh plugin, empty in-memory store and disposable state path.
    ///
    /// The path is unique per call (atomic counter + PID): several tests run
    /// in parallel, and a shared file would be stolen by another test writing
    /// at the same instant.
    fn test_plugin() -> MusicBrainzPlugin {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ritornello-mb-test-{}-{n}.json", std::process::id()));
        MusicBrainzPlugin::new(Arc::new(RwLock::new(patterns::Store::default())), path)
    }

    /// Plugin whose disc is already known: avoids any network call in the
    /// tests, **none of them touches the network**.
    fn plugin_with_known_disc() -> MusicBrainzPlugin {
        let mut p = test_plugin();
        p.known = Some((TOC.to_string(), musicbrainz::parse_lookup(FIXTURE, 3)));
        p
    }

    #[test]
    fn a_disc_identity_is_recognized() {
        let d = disc_of(&disc_identity(2)).unwrap();
        assert_eq!(d.toc, TOC);
        assert_eq!(d.track, 2);
    }

    #[test]
    fn an_identity_that_is_not_a_disc_is_ignored() {
        // The plugin must stay silent on a radio stream, without inspecting anything more.
        assert!(disc_of(&json!({"kind": "stream", "url": "http://fip"})).is_none());
        assert!(disc_of(&json!({"kind": "disc"})).is_none(), "without TOC");
        assert!(disc_of(&json!({"kind": "disc", "toc": "  "})).is_none(), "empty TOC");
        assert!(disc_of(&json!({"kind": "disc", "toc": TOC})).is_none(), "without track index");
        assert!(disc_of(&json!("not an object")).is_none());
        assert!(disc_of(&Value::Null).is_none());
    }

    #[tokio::test]
    async fn emits_the_announced_track_title_with_identity_echo() {
        let mut p = plugin_with_known_disc();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(1)), ..Default::default() }).await;
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, disc_identity(1), "the identity must be echoed back");
        assert_eq!(e.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(e.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(e.title.as_deref(), Some("Freddie Freeloader"));
        // The MBID was already carried by the TOC lookup: the cover goes out
        // without one more request, and this path overwrites (it knows what
        // is playing).
        assert_eq!(
            e.cover,
            Some(CoverRef::Url { url: musicbrainz::url_caa("e32a3f0b-1c19-3170-bb1c-650893774744") })
        );
        assert!(!e.fill_only, "the disc path knows the TOC, it overwrites");
    }

    #[tokio::test]
    async fn a_track_change_re_emits_from_the_cache() {
        let mut p = plugin_with_known_disc();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(0)), ..Default::default() }).await;
        assert_eq!(p.next_enrichment().await.title.as_deref(), Some("So What"));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(2)), ..Default::default() }).await;
        let e = p.next_enrichment().await;
        assert_eq!(e.title.as_deref(), Some("Blue in Green"));
        assert_eq!(e.identity, disc_identity(2));
        assert!(p.in_flight.is_none(), "no new query for the same disc");
    }

    #[tokio::test]
    async fn a_stop_clears_the_prepared_enrichment() {
        let mut p = plugin_with_known_disc();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(0)), ..Default::default() }).await;
        p.now_playing(NowPlaying { source: "cd".into(), identity: None, ..Default::default() }).await;
        assert!(p.ready.is_none(), "a stale enrichment must not go out after the stop");
        assert!(p.identity.is_none());
    }

    #[tokio::test]
    async fn a_radio_stream_triggers_nothing() {
        let mut p = plugin_with_known_disc();
        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(json!({"kind": "stream", "url": "http://fip"})),
            ..Default::default()
        })
        .await;
        assert!(p.ready.is_none());
        assert!(p.in_flight.is_none(), "no network call for a stream identity");
    }

    #[tokio::test]
    async fn an_out_of_bounds_track_produces_nothing() {
        // Disc recognized with 3 tracks, but the identity announces track 7:
        // staying silent beats announcing another track's title.
        let mut p = plugin_with_known_disc();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(7)), ..Default::default() }).await;
        assert!(p.ready.is_none());
    }

    #[tokio::test]
    async fn an_unknown_disc_produces_nothing_and_is_queried_only_once() {
        // Result memorized as "queried, nothing found": the following track
        // changes must not relaunch a request.
        let mut p = test_plugin();
        p.known = Some((TOC.to_string(), None));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(0)), ..Default::default() }).await;
        assert!(p.ready.is_none());
        assert!(p.in_flight.is_none());
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(1)), ..Default::default() }).await;
        assert!(p.in_flight.is_none(), "an already queried disc must not be queried again");
    }

    #[tokio::test]
    async fn an_unusable_toc_triggers_no_call() {
        let mut p = test_plugin();
        p.now_playing(NowPlaying {
            source: "cd".into(),
            identity: Some(json!({"kind": "disc", "toc": "whatever", "track": 0})),
            ..Default::default()
        })
        .await;
        assert!(p.in_flight.is_none());
        assert!(p.ready.is_none());
    }

    #[test]
    fn the_generic_relay_requires_an_artist_and_an_album_and_stays_silent_if_the_cover_is_held() {
        use ritornello_proto::Known;
        // Never on a lone ICY title: it is raw text, not split, and OUI FM
        // emits "Titre - ARTISTE" in the reverse of the usual order.
        assert!(!should_search(&Known { title: Some("X - Y".into()), ..Default::default() }));
        assert!(!should_search(&Known { artist: Some("A".into()), ..Default::default() }));
        assert!(!should_search(&Known { album: Some("B".into()), ..Default::default() }));
        assert!(should_search(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            ..Default::default()
        }));
        // A cover already held: the call would be thrown away.
        assert!(!should_search(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            cover: true,
            ..Default::default()
        }));
    }

    #[tokio::test]
    async fn a_result_for_another_disc_produces_nothing() {
        // The disc was swapped while the request was in flight: the result
        // arrives for a TOC that is no longer the one in the tray.
        let mut p = test_plugin();
        // Query declared "in flight": `search` will thus launch no network
        // request, and the result is injected by hand below.
        p.in_flight = Some(TOC.to_string());
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(0)), ..Default::default() }).await;
        p.found_tx
            .send(("42 1 2 3".to_string(), Answer::Known(musicbrainz::parse_lookup(FIXTURE, 3))))
            .await
            .unwrap();
        // `next_enrichment` consumes the stale result then goes back to
        // waiting: we check that it returns nothing within a bounded delay.
        let r = tokio::time::timeout(std::time::Duration::from_millis(200), p.next_enrichment()).await;
        assert!(r.is_err(), "no enrichment must come out of an off-topic result");
    }

    // `..Default::default()` behind an already complete literal: clippy calls
    // it ineffective (`needless_update`), and it is right **today**. This is
    // not redundancy but forward compatibility — a literal ending this way
    // survives the addition of a field to the struct, one that enumerates them
    // all breaks. The repository paid for this lesson: a field added to a
    // public struct broke 44 literals elsewhere, which a `cargo test -p` never
    // compiles. When clippy and forward compatibility contradict each other
    // here, the latter wins, and the rule gets an `allow`.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn the_generic_relay_emits_a_lone_cover_as_completion() {
        // The search is pre-memorized so as to exercise no network call: it is
        // `search_cover` that decides not to relaunch, exactly as
        // `plugin_with_known_disc` does on the disc side.
        let mut p = test_plugin();
        let key = ("Miles Davis".to_string(), "Kind of Blue".to_string());
        // An already resolved URL, like what `search_release` memorizes: it is
        // the module that decides the level (album or pressing), never this
        // path. Here a group's, the common case of a search.
        let cover = musicbrainz::caa_group_url("8e8a594f-2175-38c7-a871-abb68ec363e7");
        p.known_cover = Some((key, Some(cover.clone())));
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(file_identity("/music/a.flac")),
            known: ritornello_proto::Known {
                artist: Some("Miles Davis".into()),
                album: Some("Kind of Blue".into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, file_identity("/music/a.flac"), "the identity must be echoed back");
        assert_eq!(e.cover, Some(CoverRef::Url { url: cover }));
        assert!(e.fill_only, "this path knows nothing beyond what it was given, it completes");
        assert!(
            e.artist.is_none() && e.title.is_none() && e.album.is_none(),
            "no text field: it knows nothing beyond what it was given"
        );
    }

    // See `the_generic_relay_emits_a_lone_cover_as_completion`: the
    // `..Default::default()` is forward compatibility, not redundancy.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn an_already_searched_artist_album_pair_is_not_queried_again() {
        // Memorized as "searched, nothing found": must not relaunch a request
        // for the same frame nor for a following frame of the same album.
        let mut p = test_plugin();
        let key = ("A".to_string(), "B".to_string());
        p.known_cover = Some((key, None));
        let known = ritornello_proto::Known { artist: Some("A".into()), album: Some("B".into()), ..Default::default() };
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(file_identity("/x")),
            known: known.clone(),
            ..Default::default()
        })
        .await;
        // **It prepares an admission, not a cover**: "searched, nothing found"
        // is an answer, and it is what lets the screen tell MusicBrainz
        // queried without success from MusicBrainz never queried. It brings
        // nothing else — no field, no image — so it enters no arbitration.
        let admission = p.ready.as_ref().expect("an unsuccessful search must declare itself");
        assert!(admission.searched);
        assert!(admission.artist.is_none() && admission.title.is_none() && admission.album.is_none());
        assert!(admission.cover.is_none() && admission.year.is_none() && admission.links.is_empty());
        assert!(p.cover_in_flight.is_none());
        p.now_playing(NowPlaying { source: "files".into(), identity: Some(file_identity("/x")), known, ..Default::default() })
            .await;
        assert!(p.cover_in_flight.is_none(), "an already searched pair must not be searched again");
    }

    // See `the_generic_relay_emits_a_lone_cover_as_completion`: the
    // `..Default::default()` is forward compatibility, not redundancy.
    #[allow(clippy::needless_update)]
    #[tokio::test(start_paused = true)]
    async fn a_transient_musicbrainz_outage_is_not_memorized() {
        // The defect reported by the owner, as a test. A MusicBrainz 503 froze
        // into "this album has no cover" for the whole duration of the album:
        // only a plugin restart unblocked it. Here we force the `Unavailable`
        // answer and check that nothing is memorized and that the next frame
        // does restart.
        let mut p = test_plugin();
        let key = ("Rhapsody Of Fire".to_string(), "Triumph Or Agony".to_string());
        let known = ritornello_proto::Known {
            artist: Some("Rhapsody Of Fire".into()),
            album: Some("Triumph Or Agony".into()),
            ..Default::default()
        };
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(file_identity("/x")),
            known: known.clone(),
            ..Default::default()
        })
        .await;
        assert_eq!(p.cover_in_flight.as_ref(), Some(&key), "precondition: a search went out");

        // The task answers "no answer".
        p.cover_tx.send((key.clone(), Answer::Unavailable)).await.unwrap();
        // Virtual clock (`start_paused`): this `timeout` waits no real
        // duration. It lets the loop dequeue the message — ready as soon as
        // it is queued — then yields for want of an enrichment to produce. The
        // timeout is thus not an assumption about execution speed, it is
        // virtual time advancing on its own once nothing is ready anymore.
        let nothing = tokio::time::timeout(std::time::Duration::from_secs(1), p.next_enrichment()).await;
        assert!(nothing.is_err(), "a transient outage must produce no enrichment");

        assert!(
            p.known_cover.is_none(),
            "nothing must be memorized: that is what froze the absence until the restart"
        );
        // **A retry is armed, and it is the one holding the marker.** The
        // previous version released `cover_in_flight`, counting on "the next
        // frame" to retry -- but there is none on a local file, where identity
        // and `known` freeze as soon as the tags are read. The marker thus
        // stays armed during the wait: that is what forbids a frame arriving
        // in the meantime from launching a second search for the same pair.
        assert_eq!(p.cover_in_flight.as_ref(), Some(&key), "the retry holds the marker");
        assert_eq!(p.cover_retries, Some((key.clone(), 1)), "one retry must be consumed");

        // One more frame restarts nothing: the already armed retry takes care
        // of it, and two concurrent searches for the same pair would answer
        // each other.
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(file_identity("/x")),
            known,
            ..Default::default()
        })
        .await;
        assert_eq!(p.cover_retries, Some((key, 1)), "no further retry must go out");
    }

    #[test]
    fn the_retry_budget_is_bounded_and_carried_by_the_pair() {
        // The decision alone, without clock or network: that is why it is
        // separated from its application (see `retry_due`).
        let mut p = test_plugin();
        let key = ("A".to_string(), "Disc".to_string());
        p.generic_key = Some(key.clone());

        // Three retries, increasingly spaced, then nothing more: beyond that,
        // the absence is no longer a transient outage and the track change
        // remains the ultimate retry. The third was added on evidence — six
        // 503s out of nine requests in one minute, observed on the device.
        assert_eq!(p.retry_due(&key), Some((0, Duration::from_secs(20))));
        p.cover_retries = Some((key.clone(), 1));
        assert_eq!(p.retry_due(&key), Some((1, Duration::from_secs(60))));
        p.cover_retries = Some((key.clone(), 2));
        assert_eq!(p.retry_due(&key), Some((2, Duration::from_secs(180))));
        p.cover_retries = Some((key.clone(), 3));
        assert_eq!(p.retry_due(&key), None, "the budget must be bounded");

        // The counter is carried by the pair: another album starts from zero
        // without any explicit reset having to exist.
        let other = ("A".to_string(), "Other".to_string());
        p.generic_key = Some(other.clone());
        assert_eq!(p.retry_due(&other), Some((0, Duration::from_secs(20))));

        // And nothing is retried for a pair no longer targeted: it would be
        // pure wasted work, its answer being discarded anyway.
        assert_eq!(p.retry_due(&key), None, "an abandoned pair is not retried");
    }

    // See `the_generic_relay_emits_a_lone_cover_as_completion`: the
    // `..Default::default()` is forward compatibility, not redundancy.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn an_album_change_does_not_reuse_the_old_cover() {
        // Memorization is keyed by (artist, album): a new album must change
        // the key and never redisplay the old one's cover.
        let mut p = test_plugin();
        p.known_cover =
            Some((("A".to_string(), "Old".to_string()), Some("11111111-1111-1111-1111-111111111111".into())));
        // Search of the new album declared "in flight": avoids any network
        // call in this test, without changing what is observed (`in_flight`
        // stops `search_cover` before the `tokio::spawn`).
        p.cover_in_flight = Some(("A".to_string(), "New".to_string()));
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(file_identity("/x")),
            known: ritornello_proto::Known { artist: Some("A".into()), album: Some("New".into()), ..Default::default() },
            ..Default::default()
        })
        .await;
        assert!(p.ready.is_none(), "the old album's cover must not apply to the new one");
        assert_eq!(p.generic_key, Some(("A".to_string(), "New".to_string())), "the key follows the new album");
    }

    #[tokio::test]
    async fn a_disc_identity_clears_the_generic_state() {
        // The two paths are exclusive: an inserted disc must leave nothing of
        // the generic relay in place.
        let mut p = test_plugin();
        p.in_flight = Some(TOC.to_string()); // avoids any network call in this test
        p.generic_identity = Some(file_identity("/x"));
        p.generic_key = Some(("A".to_string(), "B".to_string()));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(disc_identity(0)), ..Default::default() }).await;
        assert!(p.generic_identity.is_none());
        assert!(p.generic_key.is_none());
    }

    // --- ICY path (radio) -----------------------------------------------

    fn candidate(artist: &str, title: &str, artist_first: bool) -> icy::Candidate {
        icy::Candidate {
            artist: artist.to_string(),
            title: title.to_string(),
            separator: " - ",
            artist_first,
            title_in_middle: false,
        }
    }

    fn recording(score: u64, title: &str) -> musicbrainz::Recording {
        musicbrainz::Recording { score, title: title.to_string(), cover_url: None }
    }

    #[test]
    fn the_best_score_wins_and_not_the_first_accepted() {
        // The winner is **second** in attempt order: otherwise, the test would
        // also pass with "take the first accepted".
        let attempts = vec![
            // The reversed order validates anyway (score above the threshold,
            // but lower): this is the real case that makes "take the first
            // accepted" dangerous.
            (candidate("So What", "Miles Davis", false), Some(recording(91, "Miles Davis"))),
            (candidate("Miles Davis", "So What", true), Some(recording(99, "So What"))),
        ];
        let winner = best_accepted(&attempts).expect("a candidate must be kept");
        assert_eq!((winner.artist.as_str(), winner.title.as_str()), ("Miles Davis", "So What"));
        assert!(winner.artist_first);
    }

    #[test]
    fn a_title_that_does_not_match_is_rejected_despite_a_good_score() {
        // The guard that carries everything: the score alone is too generous,
        // the search almost always returning something plausible.
        let attempts =
            vec![(candidate("So What", "Miles Davis", false), Some(recording(95, "A Completely Different Recording")))];
        assert!(best_accepted(&attempts).is_none(), "high score but mismatching title: must be rejected");
    }

    #[test]
    fn no_accepted_candidate_yields_do_not_split() {
        // No attempt (string without separator, cf. `icy::candidates`) or none
        // accepted: the probe kept nothing, which `handle_icy` translates into
        // `Pattern::DoNotSplit` (not replayed here, the network not being
        // reachable in tests — `best_accepted` carries the decision).
        assert!(best_accepted(&[]).is_none(), "no attempt, hence none accepted");
        let attempts = vec![
            (candidate("A", "B", true), None), // offline / nothing found
            (candidate("B", "A", false), Some(recording(50, "A"))), // below the threshold
        ];
        assert!(best_accepted(&attempts).is_none());
    }

    #[tokio::test]
    async fn a_station_classified_do_not_split_triggers_no_request() {
        // `handle_icy` with `known = DoNotSplit` and `reprobe = false` must
        // return its outcome **without** touching the network. Proven by the
        // fact that the test passes while no network is reachable here: an
        // attempted request would fail or drag on, and the timeout below
        // would make it fail.
        let r = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            handle_icy(
                "http://f".to_string(),
                "Miles Davis - So What".to_string(),
                json!({"kind": "stream", "url": "http://f"}),
                Some(patterns::Pattern::DoNotSplit),
                false,
            ),
        )
        .await;
        let outcome = r.expect("no network request must be attempted, hence no timeout");
        assert_eq!(outcome.pattern, None);
        assert_eq!(outcome.validated, None);
    }

    /// Sends a failure outcome (validation missed) for `url`/`raw`, and
    /// consumes the resulting loop turn.
    ///
    /// **A failure now produces an enrichment**, and that is a review fix:
    /// emitting nothing let the *previous* track's enrichment win the
    /// arbitration, a radio's identity not changing from one track to the
    /// next. The earlier assertion — "no enrichment" — thus pinned the defect
    /// instead of the property.
    ///
    /// With `pair: None`, what goes out is the cleaned string as the title,
    /// without artist: no split is asserted, we show what the stream
    /// announces. And the wait is **exact** (we await what must come) instead
    /// of relying on a time margin.
    async fn send_failure(p: &mut MusicBrainzPlugin, url: &str, raw: &str) {
        p.icy_tx
            .send(IcyOutcome {
                url: url.to_string(),
                raw: raw.to_string(),
                identity: json!({"kind": "stream", "url": url}),
                pattern: None,
                validated: None,
                pair: None,
            })
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.artist, None, "a failure asserts no artist");
        assert_eq!(
            e.title.as_deref(),
            Some(icy::clean(raw).as_str()),
            "it shows what the stream announces, cleaned"
        );
        assert!(e.cover.is_none(), "and no cover, for want of a release to cite");
    }

    #[tokio::test]
    async fn an_isolated_failure_does_not_reprobe_and_three_in_a_row_do() {
        // Both halves. Without the first, "always reprobe" would pass; without
        // the second, "never reprobe" would pass.
        //
        // The counter and the decision are exercised through the real code
        // path (the outcome goes through `icy_tx`/`next_enrichment`, as
        // `a_result_for_another_disc_produces_nothing` already does on the
        // disc side): this is not a hard-coded re-simulation of the arithmetic.
        let mut p = test_plugin();
        let url = "http://f";
        p.icy_seen = Some("raw".to_string());

        for n in 1..=2u32 {
            send_failure(&mut p, url, "raw").await;
            assert_eq!(p.failures.get(url), Some(&n));
            assert!(!should_reprobe(&p.failures, url), "failure number {n}: must not reprobe yet");
        }

        send_failure(&mut p, url, "raw").await;
        assert_eq!(p.failures.get(url), Some(&3));
        assert!(should_reprobe(&p.failures, url), "three failures in a row must reprobe");
    }

    /// The reprobe **consumes** the failure counter.
    ///
    /// Without that it stayed above the threshold for the life of the
    /// process, and a station that never validates — a mojibake stream, for
    /// instance — went back into a full probe at *every* title. The
    /// documentation promises the opposite, and the limit it describes became
    /// a guaranteed request storm. Finding of the final cross review.
    ///
    /// Tested on `now_playing` and not on `should_reprobe` alone: the reset
    /// lives at the launch site, and it is the link between the two that this
    /// test must hold. The detached task that follows cannot reach the
    /// network, which does not matter — the reset is synchronous and precedes
    /// the `spawn`.
    #[tokio::test]
    async fn a_reprobe_consumes_the_counter() {
        let mut p = test_plugin();
        let url = "http://example/stream.mp3";
        let identity = json!({"kind": "stream", "url": url});
        p.failures.insert(url.to_string(), FAILURES_BEFORE_REPROBE);
        assert!(should_reprobe(&p.failures, url), "three failures do arm the reprobe");

        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(identity),
            known: ritornello_proto::Known {
                stream_title: Some("Miles Davis - So What".into()),
                ..Default::default()
            },
        })
        .await;

        assert_eq!(
            p.failures.get(url),
            None,
            "launching the reprobe must have consumed the counter"
        );
        assert!(
            !should_reprobe(&p.failures, url),
            "and the next title must not reprobe in turn"
        );
    }

    #[tokio::test]
    async fn a_success_resets_the_counter() {
        // Two failures, one success, two failures: no reprobe. This is the
        // only assertion that tells a consecutive counter from a cumulative
        // one — and cumulative is the natural default.
        let mut p = test_plugin();
        let url = "http://f";
        p.icy_seen = Some("raw".to_string());

        send_failure(&mut p, url, "raw").await;
        send_failure(&mut p, url, "raw").await;
        assert_eq!(p.failures.get(url), Some(&2));

        p.icy_tx
            .send(IcyOutcome {
                url: url.to_string(),
                raw: "raw".to_string(),
                // The identity the core would have sent: it is the one that
                // must be echoed back, identically.
                identity: json!({"kind": "stream", "url": url}),
                pattern: None,
                validated: Some(("Artist".to_string(), "Title".to_string(), None)),
                pair: Some(("Artist".to_string(), "Title".to_string())),
            })
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.artist.as_deref(), Some("Artist"));
        assert!(!p.failures.contains_key(url), "the success must reset the counter");

        send_failure(&mut p, url, "raw").await;
        send_failure(&mut p, url, "raw").await;
        assert!(!should_reprobe(&p.failures, url), "consecutive counter (2), not cumulative (4): must not reprobe");
    }

    #[test]
    fn an_identity_that_is_not_a_stream_is_not_handled() {
        assert!(stream_url(&json!({"kind":"disc","toc":"1 2 3"})).is_none());
        assert!(stream_url(&json!({"kind":"stream"})).is_none());
        assert!(stream_url(&json!({"kind":"stream","url":""})).is_none());
        assert_eq!(stream_url(&json!({"kind":"stream","url":"http://f"})).as_deref(), Some("http://f"));
    }
}
