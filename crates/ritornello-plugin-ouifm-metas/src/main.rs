//! `metadata` plugin: titles of the OUI FM webradios, from their own stream.
//!
//! Why this plugin exists: out of five streams measured, only one delivers a
//! usable ICY header, and it is a foreign webradio. The common French
//! stations announce nothing, or a filler text — OUI FM literally emits
//! « Now Playing info goes here ». It does, however, expose a first-hand
//! `text/event-stream`, without authentication, with artist and title
//! **already split**.
//!
//! This endpoint is **private and undocumented**: it can change, require
//! authentication or disappear without notice. Hence three rules held here:
//! the retrieval lives in its own process and never delays playback, its
//! failure is silent on screen, and reconnection is done with a progressive
//! backoff — an unattended device must not hammer a third party's server.
//! Nothing is cached on disk.

mod stream;
mod table;

use anyhow::Result;
use stream::Meta;
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

struct OuiFmMetas {
    table: Table,
    /// Current identity, echoed back in every enrichment.
    identity: Option<Value>,
    /// Tracked webradio: its identifier, and the task that holds the
    /// connection.
    ///
    /// The connection lives in a task and not in the `next_enrichment`
    /// future: that future is dropped as soon as a `NowPlaying` arrives,
    /// which would cut the HTTP stream at every core state change.
    tracked: Option<(String, tokio::task::JoinHandle<()>)>,
    metas_tx: mpsc::Sender<(String, Meta)>,
    metas_rx: mpsc::Receiver<(String, Meta)>,
}

impl OuiFmMetas {
    fn new(table: Table) -> Self {
        let (metas_tx, metas_rx) = mpsc::channel(8);
        Self { table, identity: None, tracked: None, metas_tx, metas_rx }
    }

    /// Stops the current tracking, if there is one.
    fn stop(&mut self) {
        if let Some((id, task)) = self.tracked.take() {
            tracing::debug!("stopping tracking of webradio {id}");
            task.abort();
        }
    }

    /// Follows this webradio, unless it is already the one being followed —
    /// in which case the open connection is kept. That is the case of every
    /// track change on the same station: reopening would lose the frame the
    /// server pushes as soon as the connection opens, and would hit a third
    /// party for nothing.
    fn follows(&mut self, id: &str) {
        if self.tracked.as_ref().is_some_and(|(current, _)| current == id) {
            return;
        }
        self.stop();
        let tx = self.metas_tx.clone();
        let task_id = id.to_string();
        let task = tokio::spawn(stream::follows(task_id, tx));
        self.tracked = Some((id.to_string(), task));
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for OuiFmMetas {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Recognition then mutation: the identifiers are copied before
        // touching `self`, the table being borrowed from `self`.
        let recognized = np
            .identity
            .as_ref()
            .and_then(stream_url)
            .and_then(|url| self.table.metas_for(url))
            .map(|w| (w.metas.clone(), w.label.clone()));
        match recognized {
            Some((metas, label)) => {
                tracing::debug!("webradio recognized: {label} (metas {metas})");
                self.identity = np.identity;
                self.follows(&metas);
            }
            None => {
                // Stop, disc, or station unknown to the table: we stay quiet,
                // and above all we close the connection — a stream left open
                // would keep hitting a third party for a track that no longer
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
            // first, the runner drops this future without any received frame
            // being lost. Everything that follows its resolution is
            // synchronous, hence out of reach of a cancellation — which is
            // what allows returning directly, with no intermediate holding
            // field.
            let Some((id, meta)) = self.metas_rx.recv().await else {
                // Impossible in practice (the plugin keeps a Sender).
                std::future::pending().await
            };
            // Frame from a station we no longer follow: it was waiting in the
            // queue at the moment of the change. Same principle as the
            // staleness rule on the core side.
            let still_followed = self.tracked.as_ref().is_some_and(|(current, _)| current == &id);
            if !still_followed {
                continue;
            }
            if let Some(identity) = &self.identity {
                return Enrichment {
                    identity: identity.clone(),
                    artist: meta.artist,
                    title: meta.title,
                    // The stream gives no album (these are webradios), nor a
                    // year: measured, the frame carries no date field at all.
                    album: None,
                    year: None,
                    links: meta.links.clone(),
                    duration_s: meta.duration_s,
                    // This plugin does not know where playback stands: it
                    // answers about a track's identity, not its progress.
                    position_s: None,
                    cover: meta.cover.as_deref().map(|u| CoverRef::Url { url: u.to_string() }),
                    // This plugin reads the station's official feed: it knows
                    // better than ICY, by construction. It overwrites, so
                    // `fill_only` stays false.
                    fill_only: false,
                    ..Default::default()
                };
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let table_path =
        PathBuf::from(env_or("RITORNELLO_OUIFM_METAS", "/etc/ritornello/ouifm-metas.toml"));
    let table = Table::load(&table_path);
    tracing::info!(
        "{} known webradio(s) (embedded table + {})",
        table.webradios.len(),
        table_path.display()
    );
    Runtime::from_args()?.metadata(OuiFmMetas::new(table))?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Real stream URL of OUI FM Classic Rock, signed token included.
    const URL: &str = "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/11d46a.NND%2BFTMcarOrumMD%2FJU7lENzKQUNWno%2FSz7wPrtsPIw%3D?format=hd";
    /// Matching metadata identifier, collected from the same source.
    const METAS: &str = "3134161803443976427";

    fn stream_identity(url: &str) -> Value {
        json!({ "kind": "stream", "url": url })
    }

    /// Plugin whose tracking is already declared: no network task is spawned
    /// in the tests. **No test touches the network.**
    fn following_plugin(id: &str) -> OuiFmMetas {
        let mut p = OuiFmMetas::new(Table::embedded());
        // An inert task stands in for the HTTP connection.
        let task = tokio::spawn(std::future::pending::<()>());
        p.tracked = Some((id.to_string(), task));
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
    async fn a_frame_becomes_an_enrichment_echoing_the_identity() {
        let mut p = following_plugin(METAS);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((
                METAS.into(),
                Meta {
                    artist: Some("Shaka Ponk".into()),
                    title: Some("Wanna Get Free".into()),
                    duration_s: Some(214),
                    cover: None,
                    // Non-default value: this test checks that the links
                    // composed from the frame travel all the way to the
                    // enrichment.
                    links: vec![ritornello_proto::Link::Deezer {
                        url: "https://www.deezer.com/track/9956167".into(),
                    }],
                },
            ))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, stream_identity(URL), "the identity must be echoed back");
        assert_eq!(e.artist.as_deref(), Some("Shaka Ponk"));
        assert_eq!(e.title.as_deref(), Some("Wanna Get Free"));
        assert_eq!(e.duration_s, Some(214));
        assert_eq!(e.album, None, "a webradio has no album");
    }

    #[tokio::test]
    async fn the_already_composed_cover_travels_to_the_enrichment() {
        let mut p = following_plugin(METAS);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((
                METAS.into(),
                Meta { title: Some("t".into()), cover: Some("https://www.lesindesradios.fr/x.jpg".into()), ..Default::default() },
            ))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.cover, Some(CoverRef::Url { url: "https://www.lesindesradios.fr/x.jpg".into() }));
        assert!(!e.fill_only, "this plugin knows better than ICY, it must overwrite");
    }

    #[tokio::test]
    async fn a_station_unknown_to_the_table_closes_the_tracking() {
        let mut p = following_plugin(METAS);
        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(stream_identity("http://icecast.radiofrance.fr/fip-midfi.mp3")),
            ..Default::default()
        })
        .await;
        assert!(p.tracked.is_none(), "a stream left open would hit a third party for nothing");
        assert!(p.identity.is_none());
    }

    #[tokio::test]
    async fn stopping_playback_closes_the_tracking() {
        let mut p = following_plugin(METAS);
        p.now_playing(NowPlaying { source: "radio".into(), identity: None, ..Default::default() }).await;
        assert!(p.tracked.is_none());
    }

    #[tokio::test]
    async fn a_disc_identity_closes_the_tracking() {
        let mut p = following_plugin(METAS);
        p.now_playing(NowPlaying {
            source: "cd".into(),
            identity: Some(json!({"kind": "disc", "toc": "3 150 22767 41887 63000", "track": 0})),
            ..Default::default()
        })
        .await;
        assert!(p.tracked.is_none(), "this plugin does not handle discs");
    }

    #[tokio::test]
    async fn staying_on_the_same_station_keeps_the_connection() {
        // A track change gives a new identity but the same station: reopening
        // the stream would lose the frame the server pushes as soon as the
        // connection opens.
        //
        // This test **also** proves the end-to-end mapping, from the embedded
        // table: the tracking in place carries the Classic Rock metadata
        // identifier, and if the real URL did not resolve exactly onto it,
        // `follows` would drop this task to spawn another — which the
        // assertion below refuses. No network connection is opened, for that
        // very reason.
        let mut p = following_plugin(METAS);
        let before = p.tracked.as_ref().map(|(id, t)| (id.clone(), t.id()));
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        let after = p.tracked.as_ref().map(|(id, t)| (id.clone(), t.id()));
        assert_eq!(before, after, "the same task must carry on");
    }

    #[tokio::test]
    async fn a_frame_from_a_station_no_longer_followed_is_discarded() {
        let mut p = following_plugin(METAS);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        // Frame queued at the moment of the station change.
        p.metas_tx
            .send(("99".into(), Meta { title: Some("ancien".into()), ..Default::default() }))
            .await
            .unwrap();
        let r = tokio::time::timeout(std::time::Duration::from_millis(200), p.next_enrichment()).await;
        assert!(r.is_err(), "an off-topic frame must produce no enrichment");
    }

    #[tokio::test]
    async fn an_empty_table_never_follows_anything() {
        // Degenerate case, reachable if the embedded table became empty: the
        // plugin must stay silent, never guess an identifier.
        let mut p = OuiFmMetas::new(Table::default());
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(stream_identity(URL)), ..Default::default() }).await;
        assert!(p.tracked.is_none());
    }

}
