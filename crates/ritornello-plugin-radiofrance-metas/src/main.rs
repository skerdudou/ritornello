//! `metadata` plugin: titles of the Radio France stations, from their live feed.
//!
//! Why this plugin exists: the Radio France streams emit **no** ICY metadata
//! at all — no `icy-metaint` whatsoever, measured on FIP as well as on its
//! webradios. Where OUI FM announces at least a filler text, a Radio France
//! station configured on the device currently displays nothing. Radio France
//! does, however, expose the live feed of each station, without
//! authentication, with title and artist **already split**.
//!
//! This endpoint is **private and undocumented** (only the station list is
//! documented): it can change, require authentication or disappear without
//! notice. Hence three rules held here: the polling lives in its own process
//! and never delays playback, its failure is silent on screen, and the rhythm
//! is the one the server announces itself, with a progressive backoff on
//! failure — an unattended device must not hammer a third party's server.
//! Nothing is cached on disk.

mod live;
mod table;

use anyhow::Result;
use live::Meta;
use ritornello_plugin_sdk::{MetadataPlugin, Runtime};
use ritornello_proto::{CoverRef, Enrichment, NowPlaying};
use serde_json::Value;
use std::path::PathBuf;
use table::Table;
use tokio::sync::mpsc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// URL of a stream identity, if it is one.
///
/// Pure function: entry point for data coming from another process, hence the
/// place where an unexpected shape must be discarded without noise.
fn stream_url(identity: &Value) -> Option<&str> {
    if identity.get("kind").and_then(Value::as_str)? != "stream" {
        return None;
    }
    let url = identity.get("url").and_then(Value::as_str)?;
    (!url.trim().is_empty()).then_some(url)
}

struct RadioFranceMetas {
    table: Table,
    /// Current identity, echoed back in every enrichment.
    identity: Option<Value>,
    /// Tracked station: its identifier, and the task that queries it.
    ///
    /// The polling lives in a task and not in the `next_enrichment` future:
    /// that future is dropped as soon as a `NowPlaying` arrives, which would
    /// reset the cycle at every core state change — and above all would lose
    /// the "last seen" that avoids re-emitting the same track on every query.
    tracked: Option<(u32, tokio::task::JoinHandle<()>)>,
    metas_tx: mpsc::Sender<(u32, Meta)>,
    metas_rx: mpsc::Receiver<(u32, Meta)>,
}

impl RadioFranceMetas {
    fn new(table: Table) -> Self {
        let (metas_tx, metas_rx) = mpsc::channel(8);
        Self { table, identity: None, tracked: None, metas_tx, metas_rx }
    }

    /// Stops the current tracking, if there is one.
    fn stop(&mut self) {
        if let Some((id, task)) = self.tracked.take() {
            tracing::debug!("stopped following station {id}");
            task.abort();
        }
    }

    /// Follows this station, unless it is already the one being followed — in
    /// which case the running task is kept. That is the case of every track
    /// change on the same station: restarting it would lose its "last seen",
    /// hence re-emit the current track, and would query a third party outside
    /// the rhythm it announced itself.
    fn follows(&mut self, id: u32, profile: String) {
        if self.tracked.as_ref().is_some_and(|(current, _)| *current == id) {
            return;
        }
        self.stop();
        let tx = self.metas_tx.clone();
        let task = tokio::spawn(live::follows(id, profile, tx));
        self.tracked = Some((id, task));
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for RadioFranceMetas {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Recognition then mutation: the values are copied before touching
        // `self`, the table being borrowed from `self`.
        let recognized = np
            .identity
            .as_ref()
            .and_then(stream_url)
            .and_then(|url| self.table.station_for(url))
            .map(|s| (s.id, s.label.clone(), s.rules.clone()));
        match recognized {
            Some((id, label, profile)) => {
                tracing::debug!("station recognized: {label} (id {id}, profile {profile})");
                self.identity = np.identity;
                self.follows(id, profile);
            }
            None => {
                // Stop, disc, or station unknown to the table: we stay quiet,
                // and above all we stop the task — a query left running would
                // keep hitting a third party for a station that no longer
                // plays.
                self.identity = None;
                self.stop();
            }
        }
    }

    // `..Default::default()` behind a literal that is nevertheless complete:
    // clippy calls it a no-op (`needless_update`), and it is right **today**.
    // This is not redundancy but forward compatibility — a literal ending
    // like this survives the addition of a field to the struct, one that
    // enumerates them all breaks. The repo paid for that lesson: a field
    // added to a public struct broke 44 literals elsewhere, none of which
    // `cargo test -p` ever compiles. When clippy and forward compatibility
    // contradict each other here, the latter wins, and the lint gets an
    // `allow`.
    #[allow(clippy::needless_update)]
    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            // `recv` is cancellable without loss: if a `NowPlaying` arrives
            // first, the runner drops this future without any received
            // reading being lost. Everything that follows its resolution is
            // synchronous, hence out of reach of a cancellation.
            let Some((id, meta)) = self.metas_rx.recv().await else {
                // Impossible in practice (the plugin keeps a Sender).
                std::future::pending().await
            };
            // Reading from a station we no longer follow: it was waiting in
            // the queue at the moment of the change. Same principle as the
            // staleness rule on the core side.
            let still_followed = self.tracked.as_ref().is_some_and(|(current, _)| *current == id);
            if !still_followed {
                continue;
            }
            if let Some(identity) = &self.identity {
                return Enrichment {
                    identity: identity.clone(),
                    artist: meta.artist,
                    title: meta.title,
                    // Absent most of the time: the live feed does not give
                    // it, it is read from the schedule, which is frequently
                    // one track behind (see `live::supplement_in_schedule`).
                    album: meta.album,
                    year: meta.year,
                    links: meta.links,
                    duration_s: meta.duration_s,
                    cover: meta.cover.as_deref().map(|u| CoverRef::Url { url: live::cover_url(u) }),
                    // This plugin reads the station's official feed: it knows
                    // better than ICY, by construction. It overwrites, so
                    // `fill_only` stays false.
                    fill_only: false,
                    // The elapsed time is computed **here**, at emission time:
                    // it is the only instant where it is exact, and the core
                    // anchors it at reception. A skewed clock or a
                    // `startTime` in the future would give a negative elapsed
                    // time: `checked_sub` turns it into "I don't know" rather
                    // than zero, which would claim to know.
                    position_s: meta.start_time.and_then(|start| {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()?
                            .as_secs();
                        now.checked_sub(start).and_then(|e| u32::try_from(e).ok())
                    }),
                    ..Default::default()
                };
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let table_path = PathBuf::from(env_or(
        "RITORNELLO_RADIOFRANCE_METAS",
        "/etc/ritornello/radiofrance-metas.toml",
    ));
    let table = Table::load(&table_path);
    tracing::info!("{} station(s) known (bundled table + {})", table.stations.len(), table_path.display());
    Runtime::from_args()?.metadata(RadioFranceMetas::new(table))?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Real stream URL of FIP Groove, as a directory publishes it.
    const URL: &str = "https://icecast.radiofrance.fr/fipgroove-midfi.mp3";
    /// Matching station identifier, collected from the documentation.
    const ID: u32 = 66;

    fn stream_identity(url: &str) -> Value {
        json!({ "kind": "stream", "url": url })
    }

    /// Plugin whose tracking is already declared: no network task is spawned
    /// in the tests. **No test touches the network.**
    fn following_plugin(id: u32) -> RadioFranceMetas {
        let mut p = RadioFranceMetas::new(Table::embedded());
        // An inert task stands in for the HTTP polling.
        let task = tokio::spawn(std::future::pending::<()>());
        p.tracked = Some((id, task));
        p
    }

    #[test]
    fn recognizes_a_stream_identity() {
        assert_eq!(stream_url(&stream_identity(URL)), Some(URL));
    }

    #[test]
    fn ignores_what_is_not_a_stream() {
        assert!(stream_url(&json!({"kind": "disc", "toc": "3 1 2 3"})).is_none());
        assert!(stream_url(&json!({"kind": "stream"})).is_none());
        assert!(stream_url(&json!({"kind": "stream", "url": "  "})).is_none());
        assert!(stream_url(&Value::Null).is_none());
    }

    #[tokio::test]
    async fn a_reading_becomes_an_enrichment_echoing_the_identity() {
        let mut p = following_plugin(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((
                ID,
                Meta {
                    artist: Some("Etta James".into()),
                    title: Some("Fire".into()),
                    album: Some("At Last!".into()),
                    // Non-default values: this test checks that the relay
                    // carries the whole schedule supplement all the way to
                    // the enrichment, not just the album.
                    year: Some(1960),
                    links: vec![ritornello_proto::Link::Youtube {
                        url: "https://www.youtube.com/watch?v=zIqlKJj9IlY".into(),
                    }],
                    duration_s: Some(197),
                    start_time: None,
                    cover: None,
                },
            ))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, stream_identity(URL), "the identity must be echoed back");
        assert_eq!(e.artist.as_deref(), Some("Etta James"));
        assert_eq!(e.title.as_deref(), Some("Fire"));
        assert_eq!(e.duration_s, Some(197));
        // The album does not come from the live feed but from the schedule,
        // and it travels all the way to the enrichment — that is what the
        // core places in `track.album`, which a display can turn into a line.
        assert_eq!(e.album.as_deref(), Some("At Last!"));
    }

    #[tokio::test]
    async fn the_cover_becomes_a_composed_url_and_overwrites() {
        let mut p = following_plugin(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((ID, Meta { title: Some("Fire".into()), cover: Some("uuid-test".into()), ..Default::default() }))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(
            e.cover,
            Some(CoverRef::Url {
                url: "https://api.radiofrance.fr/v1/services/embed/image/uuid-test?preset=400x400".into()
            })
        );
        assert!(!e.fill_only, "this plugin knows better than ICY, it must overwrite");
    }

    #[tokio::test]
    async fn a_track_without_album_is_still_a_valid_enrichment() {
        // The most common case: the schedule is one track behind. The missing
        // album must hold back nothing else.
        let mut p = following_plugin(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((ID, Meta { title: Some("Fire".into()), ..Default::default() }))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.title.as_deref(), Some("Fire"));
        assert_eq!(e.album, None);
    }

    #[tokio::test]
    async fn a_station_unknown_to_the_table_closes_the_tracking() {
        let mut p = following_plugin(ID);
        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(stream_identity("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3")),
            ..Default::default()
        })
        .await;
        assert!(p.tracked.is_none(), "a query left running would hit a third party for nothing");
        assert!(p.identity.is_none());
    }

    #[tokio::test]
    async fn stopping_playback_closes_the_tracking() {
        let mut p = following_plugin(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: None, ..Default::default() }).await;
        assert!(p.tracked.is_none());
    }

    #[tokio::test]
    async fn a_disc_identity_closes_the_tracking() {
        let mut p = following_plugin(ID);
        p.now_playing(NowPlaying {
            source: "cd".into(),
            identity: Some(json!({"kind": "disc", "toc": "3 150 22767 41887 63000", "track": 0})),
            ..Default::default()
        })
        .await;
        assert!(p.tracked.is_none(), "this plugin does not handle discs");
    }

    #[tokio::test]
    async fn staying_on_the_same_station_keeps_the_task() {
        // A track change gives a new identity but the same station:
        // restarting the task would lose its "last seen" and re-emit the
        // current track.
        //
        // This test **also** proves the end-to-end mapping, from the embedded
        // table: the tracking in place carries the FIP Groove identifier, and
        // if the real URL did not resolve exactly onto it, `follows` would
        // drop this task to spawn another — which the assertion below
        // refuses. No request is emitted, for that very reason.
        let mut p = following_plugin(ID);
        let before = p.tracked.as_ref().map(|(id, t)| (*id, t.id()));
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        let after = p.tracked.as_ref().map(|(id, t)| (*id, t.id()));
        assert_eq!(before, after, "the same task must carry on");
    }

    #[tokio::test]
    async fn a_reading_from_a_station_no_longer_followed_is_discarded() {
        let mut p = following_plugin(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        // Reading queued at the moment of the station change.
        p.metas_tx
            .send((99, Meta { title: Some("ancien".into()), ..Default::default() }))
            .await
            .unwrap();
        let r = tokio::time::timeout(std::time::Duration::from_millis(200), p.next_enrichment()).await;
        assert!(r.is_err(), "an off-topic reading must produce no enrichment");
    }

    #[tokio::test]
    async fn an_empty_table_never_follows_anything() {
        // Degenerate case, reachable if the embedded table became empty: the
        // plugin must stay silent, never guess an identifier.
        let mut p = RadioFranceMetas::new(Table::default());
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        assert!(p.tracked.is_none());
    }
}
