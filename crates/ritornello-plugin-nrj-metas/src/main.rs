//! `metadata` plugin: titles of the NRJ group's webradios, from their own
//! on-air endpoint.
//!
//! Why this plugin exists: the group's streams do emit ICY, but what they put
//! in it is an internal cart code, duplicated on both sides of the separator —
//! `DD25-19 - DD25-19` on Rire & Chansons, `5612 - 5612` on NRJ. There is
//! nothing to split. Worse than an empty ICY: it looks like an artist/title
//! pair, so it gets displayed as one. Each brand site does, however, expose
//! its stations' on-air data, without authentication, with artist and title
//! **already split**.
//!
//! This endpoint is **private and undocumented**: it can change, require
//! authentication or disappear without notice. Hence three rules held here:
//! the querying lives in its own process and never delays playback, its
//! failure is silent on screen, and the rhythm is driven by the stream itself
//! with a progressive backoff on failure — an unattended device must not
//! hammer a third party's server. Nothing is cached on disk.
//!
//! **The rhythm is the interesting part, and it is measured.** The cart code
//! changes ahead of the JSON — 33 seconds ahead, twice — and the core already
//! hands us that code in `Known.stream_title`. So the code says *when*, and
//! the JSON says *what*. The deadline the server announces is only a safety
//! net, because the server is itself 40 to 70 seconds late on it.

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

/// Whether a fresh cart code should poke the tracking task.
///
/// Extracted as a pure function so the created-task guard is provable
/// without ever spawning `live::follows`: `just_created` is true only the
/// very first time a task is created for a station, and a task in that state
/// already queries on its own, almost immediately (`live::follows`'s
/// `INITIAL_WAIT`) — poking it at that same instant would only push that
/// first query further out, reintroducing the very blank the near-immediate
/// query exists to remove. Otherwise: only a genuine **change** is worth a
/// poke, since Icecast repeats the same cart code throughout an item.
fn should_poke(just_created: bool, code: &Option<String>, last_code: &Option<String>) -> bool {
    !just_created && code.is_some() && code != last_code
}

/// Whether `tracked` already designates this exact station.
///
/// Extracted as a pure function so the guard against re-spawning a task —
/// and the staleness filter in `next_enrichment` — are both provable
/// without ever spawning `live::follows`: comparing `id` alone, as an
/// earlier version did, is only correct as long as no two stations ever
/// share an id, an invariant the embedded table's own test enforces but
/// which the operator's override file can break at will (see
/// `Table::load`). A collision would keep the old task alive across a
/// switch and query the *wrong host* for as long as the session lasts,
/// silently — the same wrong-station failure `parse_station`'s own id
/// check already prevents, one layer up.
fn same_station(tracked: Option<(&str, u32)>, brand: &str, id: u32) -> bool {
    tracked == Some((brand, id))
}

/// The station being followed, and the two handles onto its task.
struct Tracked {
    brand: String,
    id: u32,
    task: tokio::task::JoinHandle<()>,
    /// Poked when the stream's cart code changes. Bounded and non-blocking:
    /// a full channel means a poke is already pending, which says the same
    /// thing.
    poke: mpsc::Sender<()>,
}

struct NrjMetas {
    table: Table,
    /// Current identity, echoed back in every enrichment.
    identity: Option<Value>,
    /// Tracked station.
    ///
    /// The querying lives in a task and not in the `next_enrichment` future:
    /// that future is dropped as soon as a `NowPlaying` arrives, which would
    /// lose the "last seen" that keeps the same item from being re-emitted.
    tracked: Option<Tracked>,
    /// Last cart code seen on this station, so that only a **change** pokes.
    last_code: Option<String>,
    /// Tagged `(brand, id, meta)`: see `same_station` for why the id alone
    /// is not enough to tell a reading belongs to the tracked station.
    metas_tx: mpsc::Sender<(String, u32, Meta)>,
    metas_rx: mpsc::Receiver<(String, u32, Meta)>,
}

impl NrjMetas {
    fn new(table: Table) -> Self {
        let (metas_tx, metas_rx) = mpsc::channel(8);
        Self { table, identity: None, tracked: None, last_code: None, metas_tx, metas_rx }
    }

    /// Stops the current tracking, if there is one.
    fn stop(&mut self) {
        if let Some(t) = self.tracked.take() {
            tracing::debug!("stopped following station {} ({})", t.id, t.brand);
            t.task.abort();
        }
        self.last_code = None;
    }

    /// Follows this station, unless it is already the one being followed — in
    /// which case the running task is kept. That is the case of every item
    /// change on the same station.
    ///
    /// Returns whether this call **created** the task — see `should_poke`
    /// for why the caller must not poke a task it just created.
    fn follows(&mut self, brand: String, id: u32) -> bool {
        if same_station(self.tracked.as_ref().map(|t| (t.brand.as_str(), t.id)), &brand, id) {
            return false;
        }
        self.stop();
        let (poke, poke_rx) = mpsc::channel(1);
        let tx = self.metas_tx.clone();
        let task = tokio::spawn(live::follows(brand.clone(), id, poke_rx, tx));
        self.tracked = Some(Tracked { brand, id, task, poke });
        true
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for NrjMetas {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Recognition then mutation: the values are copied before touching
        // `self`, the table being borrowed from `self`.
        let recognized = np
            .identity
            .as_ref()
            .and_then(stream_url)
            .and_then(|url| self.table.station_for(url))
            .map(|s| (s.id, s.label.clone(), s.brand.clone()));
        match recognized {
            Some((id, label, brand)) => {
                tracing::debug!("station recognized: {label} ({brand}, id {id})");
                self.identity = np.identity;
                let just_created = self.follows(brand, id);
                // The cart code says *when*: see `should_poke` for the guard
                // against the very first recognition of a station.
                let code = np.known.stream_title;
                if should_poke(just_created, &code, &self.last_code) {
                    self.last_code = code;
                    if let Some(t) = &self.tracked {
                        // `try_send`: a full channel already carries a poke,
                        // which says the same thing. Blocking here would
                        // delay the core.
                        let _ = t.poke.try_send(());
                    }
                } else if just_created {
                    // Record without poking (see `should_poke`): the code
                    // still needs to be recorded so the next call can tell a
                    // genuine change from a repeat.
                    self.last_code = code;
                }
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
    // This is not redundancy but forward compatibility — a literal ending like
    // this survives the addition of a field to the struct, one that enumerates
    // them all breaks. The repo paid for that lesson: a field added to a
    // public struct broke 44 literals elsewhere, none of which `cargo test -p`
    // ever compiles.
    #[allow(clippy::needless_update)]
    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            // `recv` is cancellable without loss: if a `NowPlaying` arrives
            // first, the runner drops this future without any received reading
            // being lost.
            let Some((brand, id, meta)) = self.metas_rx.recv().await else {
                // Impossible in practice (the plugin keeps a Sender).
                std::future::pending().await
            };
            // Reading from a station we no longer follow: it was waiting in
            // the queue at the moment of the change. Checked on the pair,
            // not `id` alone — see `same_station`.
            if !same_station(self.tracked.as_ref().map(|t| (t.brand.as_str(), t.id)), &brand, id) {
                continue;
            }
            if let Some(identity) = &self.identity {
                return Enrichment {
                    identity: identity.clone(),
                    artist: meta.artist,
                    title: meta.title,
                    // NRJ gives none of these. Leaving them empty is not a
                    // renunciation but the condition for `musicbrainz` to fill
                    // them in behind us: `composed_text` completes the year
                    // and the links from *every* enrichment, and the album
                    // from those marked `fill_only`.
                    album: None,
                    duration_s: None,
                    year: meta.year,
                    cover: meta.cover.map(|url| CoverRef::Url { url }),
                    cover_thumb: meta.cover_thumb.map(|url| CoverRef::Url { url }),
                    // This plugin reads the station's official feed: it knows
                    // better than the ICY, by construction. It overwrites, so
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
        PathBuf::from(env_or("RITORNELLO_NRJ_METAS", "/etc/ritornello/nrj-metas.toml"));
    let table = Table::load(&table_path);
    tracing::info!(
        "{} station(s) known (bundled table + {})",
        table.stations.len(),
        table_path.display()
    );
    Runtime::from_args()?.metadata(NrjMetas::new(table))?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Real stream URL of Rire & Chansons, as the site publishes it.
    const URL: &str = "https://streaming.nrjaudio.fm/ou8o8xgk7oiu?origine=fluxradios";
    const ID: u32 = 200;
    const BRAND: &str = "rireetchansons";

    fn stream_identity(url: &str) -> Value {
        json!({ "kind": "stream", "url": url })
    }

    /// Plugin whose tracking is already declared: no network task is spawned
    /// in the tests. **No test touches the network.**
    fn following_plugin(id: u32) -> NrjMetas {
        let mut p = NrjMetas::new(Table::embedded());
        let task = tokio::spawn(std::future::pending::<()>());
        let (poke_tx, _poke_rx) = mpsc::channel(1);
        p.tracked = Some(Tracked { brand: BRAND.into(), id, task, poke: poke_tx });
        p
    }

    // See the identical `#[allow]` on `next_enrichment`: `NowPlaying` happens
    // to have exactly these three fields today, but a literal ending in
    // `..Default::default()` is what survives a fourth one being added.
    #[allow(clippy::needless_update)]
    fn now_playing_with(url: &str, stream_title: Option<&str>) -> NowPlaying {
        NowPlaying {
            source: "radio".into(),
            identity: Some(stream_identity(url)),
            known: ritornello_proto::Known {
                stream_title: stream_title.map(str::to_string),
                ..Default::default()
            },
            ..Default::default()
        }
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
    async fn an_unknown_station_stops_the_tracking() {
        // A query left running would keep hitting a third party for a station
        // that no longer plays.
        let mut p = following_plugin(ID);
        p.now_playing(now_playing_with("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3", None)).await;
        assert!(p.tracked.is_none());
        assert!(p.identity.is_none());
    }

    #[tokio::test]
    async fn a_track_change_on_the_same_station_keeps_the_task() {
        // Restarting it would lose the "last seen" that keeps the same track
        // from being re-emitted, and would query a third party for nothing.
        //
        // The assertion is on the **task's** identity (`JoinHandle::id()`),
        // not the station id: the station id is the same before and after by
        // construction (`URL` resolves to `ID` both times), so asserting on
        // it alone could not catch `follows` losing its early return and
        // respawning on every frame — this is what the sibling crate checks
        // too (`radiofrance-metas/src/main.rs`).
        let mut p = following_plugin(ID);
        p.now_playing(now_playing_with(URL, Some("DD25-19 - DD25-19"))).await;
        let before = p.tracked.as_ref().map(|t| (t.id, t.task.id()));
        p.now_playing(now_playing_with(URL, Some("NGV4-14 - NGV4-14"))).await;
        let after = p.tracked.as_ref().map(|t| (t.id, t.task.id()));
        assert_eq!(before, after, "same task kept");
    }

    #[tokio::test]
    async fn a_change_of_cart_code_pokes_the_task() {
        // The measured trigger: the ICY code changes ahead of the JSON, by up
        // to 33 seconds.
        let mut p = NrjMetas::new(Table::embedded());
        let task = tokio::spawn(std::future::pending::<()>());
        let (poke_tx, mut poke_rx) = mpsc::channel(4);
        p.tracked = Some(Tracked { brand: BRAND.into(), id: ID, task, poke: poke_tx });
        p.identity = Some(stream_identity(URL));
        p.last_code = Some("DD25-19 - DD25-19".into());
        p.now_playing(now_playing_with(URL, Some("NGV4-14 - NGV4-14"))).await;
        assert!(poke_rx.try_recv().is_ok(), "the change was relayed");
    }

    #[tokio::test]
    async fn the_same_cart_code_pokes_nothing() {
        // Icecast repeats the same header throughout an item; re-handling it
        // every time would be a request for nothing.
        let mut p = NrjMetas::new(Table::embedded());
        let task = tokio::spawn(std::future::pending::<()>());
        let (poke_tx, mut poke_rx) = mpsc::channel(4);
        p.tracked = Some(Tracked { brand: BRAND.into(), id: ID, task, poke: poke_tx });
        p.identity = Some(stream_identity(URL));
        p.last_code = Some("NGV4-14 - NGV4-14".into());
        p.now_playing(now_playing_with(URL, Some("NGV4-14 - NGV4-14"))).await;
        assert!(poke_rx.try_recv().is_err(), "nothing to relay");
    }

    // `should_poke` is tested directly and in isolation rather than through
    // `now_playing`: driving `just_created = true` through `now_playing`
    // would require `self.tracked` to start `None`, which makes
    // `NrjMetas::follows` spawn the *real* `live::follows` — a live HTTP call
    // to a third party from CI. No test in this module may reach
    // `live::follows`.
    #[test]
    fn a_fresh_task_is_never_poked_regardless_of_the_code() {
        // See `should_poke`'s own doc comment for why a task just created is
        // never poked.
        assert!(!should_poke(true, &Some("DD25-19 - DD25-19".into()), &None));
        assert!(!should_poke(true, &Some("DD25-19 - DD25-19".into()), &Some("other".into())));
        assert!(!should_poke(true, &None, &None));
    }

    #[test]
    fn an_existing_tasks_change_of_cart_code_pokes() {
        assert!(should_poke(
            false,
            &Some("NGV4-14 - NGV4-14".into()),
            &Some("DD25-19 - DD25-19".into())
        ));
        // Also true starting from an unknown last code.
        assert!(should_poke(false, &Some("NGV4-14 - NGV4-14".into()), &None));
    }

    #[test]
    fn an_existing_tasks_repeated_cart_code_does_not_poke() {
        let code = Some("NGV4-14 - NGV4-14".into());
        assert!(!should_poke(false, &code, &code));
    }

    #[test]
    fn an_absent_cart_code_never_pokes() {
        assert!(!should_poke(false, &None, &None));
        assert!(!should_poke(false, &None, &Some("DD25-19 - DD25-19".into())));
    }

    #[tokio::test]
    async fn a_reading_becomes_an_enrichment_echoing_the_identity() {
        let mut p = following_plugin(ID);
        p.now_playing(now_playing_with(URL, None)).await;
        p.metas_tx
            .send((
                BRAND.into(),
                ID,
                Meta {
                    artist: Some("TOM VILLA".into()),
                    title: Some("On se juge".into()),
                    year: Some(1986),
                    cover: Some("https://x/img/600x/a.JPG".into()),
                    cover_thumb: Some("https://x/img/200x/a.JPG".into()),
                    ends_at: Some(1788704410.735),
                },
            ))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, stream_identity(URL), "the identity must be echoed back");
        assert_eq!(e.artist.as_deref(), Some("TOM VILLA"));
        assert_eq!(e.title.as_deref(), Some("On se juge"));
        assert_eq!(e.year, Some(1986));
        assert_eq!(e.cover, Some(CoverRef::Url { url: "https://x/img/600x/a.JPG".into() }));
        assert_eq!(
            e.cover_thumb,
            Some(CoverRef::Url { url: "https://x/img/200x/a.JPG".into() })
        );
        assert!(!e.fill_only, "this plugin reads the station's own feed, it overwrites");
        assert_eq!(e.album, None, "NRJ gives none: musicbrainz fills it in behind us");
    }

    #[tokio::test]
    async fn a_reading_from_a_station_no_longer_followed_is_discarded() {
        // It was waiting in the queue at the moment of the change. Same
        // principle as the staleness rule on the core side.
        let mut p = following_plugin(ID);
        p.now_playing(now_playing_with(URL, None)).await;
        p.metas_tx
            .send((BRAND.into(), 999, Meta { title: Some("stale".into()), ..Default::default() }))
            .await
            .unwrap();
        p.metas_tx
            .send((BRAND.into(), ID, Meta { title: Some("fresh".into()), ..Default::default() }))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.title.as_deref(), Some("fresh"));
    }

    #[tokio::test]
    async fn a_reading_of_the_same_id_but_a_different_brand_is_discarded() {
        // The bug fixed here: two stations of different brands can collide
        // on `id` — the embedded table's own test guards the shipped table,
        // but the operator's override file can break the invariant at will
        // (see `Table::load`). Keying the staleness filter on `id` alone, as
        // an earlier version did, would have accepted this as belonging to
        // the tracked station and displayed the wrong host's titles.
        let mut p = following_plugin(ID); // tracked under BRAND ("rireetchansons")
        p.now_playing(now_playing_with(URL, None)).await;
        p.metas_tx
            .send(("nrj".into(), ID, Meta { title: Some("wrong brand".into()), ..Default::default() }))
            .await
            .unwrap();
        p.metas_tx
            .send((BRAND.into(), ID, Meta { title: Some("right brand".into()), ..Default::default() }))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.title.as_deref(), Some("right brand"));
    }

    #[test]
    fn same_station_requires_both_the_brand_and_the_id() {
        assert!(same_station(Some(("nrj", 158)), "nrj", 158));
        // The collision this whole guard exists for: same id, another
        // brand — not the same station, and must not be read as one.
        assert!(!same_station(Some(("nrj", 158)), "nostalgie", 158));
        assert!(!same_station(Some(("nrj", 158)), "nrj", 200));
        assert!(!same_station(None, "nrj", 158));
    }
}
