//! The source half: numbered presets of videos and playlists, played through
//! mpv's ytdl hook.
//!
//! Shape copied from the radio (an armed preset, a `playing` state, a table
//! shared with the admin half), adapted to what a video and a playlist need:
//!
//! - A **video** preset plays its `watch` URL (YouTube) or its link (any
//!   other site); mpv asks the tool (the path is handed along in the `Play`)
//!   to resolve it.
//! - A **playlist** preset writes the cached, *playable* videos (available,
//!   with an address: `ytdlp::Video::play_url`, a YouTube id rebuilt into its
//!   `watch` URL, another site's address judged like an operator's link) to
//!   `playlist.m3u`, one address per line, and plays that. An entry without
//!   one is left out before anything is written, so the m3u and the list
//!   kept in memory are the same. mpv walks its own list, so next/prev while a playlist plays
//!   are `PlayerNext` / `PlayerPrev`, and where it lands comes back through
//!   `player_track(i)`. `i` is an index into the m3u, which is why the
//!   playing list is kept in memory exactly as written: a refresh of the
//!   cache never moves it.
//! - A playlist's cache is refreshed in a background task (one at a time per
//!   playlist); it never delays the select, and a failed refresh keeps the
//!   old cache.
//! - A preset that cannot play (not cached, nothing playable, bad link...) is
//!   refused without moving anything, and next/prev step over it.

use anyhow::Result;
use ritornello_plugin_sdk::{Notification, SourceOutcome, SourcePlugin};
use ritornello_plugin_web_streaming::entries::{write_atomically, Entries, ListCache};
use ritornello_plugin_web_streaming::entries::Entry;
use ritornello_plugin_web_streaming::link::{parse_link, Link};
use ritornello_plugin_web_streaming::playfail::PlaybackFailure;
use ritornello_plugin_web_streaming::state;
use ritornello_plugin_web_streaming::ytdlp::{Kind, Video, Ytdlp};
use ritornello_proto::{Preset, SourceAction, Text};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock as AsyncRwLock;

/// How long a background refresh may take before the tool is killed.
const REFRESH_DEADLINE: Duration = Duration::from_secs(120);

fn keyed(key: &str) -> Text {
    Text::Keyed { key: key.into(), params: HashMap::new() }
}

/// A video's title as the status line. A key with a parameter rather than
/// `Text::Verbatim`, which nothing outside the files plugin may produce: the
/// catalogue's `video_title` is just `{title}`.
fn video_title(title: &str) -> Text {
    Text::Keyed { key: "video_title".into(), params: HashMap::from([("title".to_string(), title.to_string())]) }
}

/// The status line of the video at index `i` of a list: its
/// `Video::display_title`, or "track {n}" when it has none to show.
fn list_title(v: &Video, i: usize) -> Text {
    match v.display_title() {
        Some(title) => video_title(&title),
        None => Text::Keyed {
            key: "untitled_track".into(),
            params: HashMap::from([("n".to_string(), (i + 1).to_string())]),
        },
    }
}

/// What mpv is handed for an address: always `ytdl://<address>`.
///
/// Measured on mpv 0.37 (task 13, fix round 1): a plain `http(s)://` address
/// is opened by mpv itself **before** its ytdl hook runs (a listener on
/// 127.0.0.1 saw mpv's GET), so a host name of the local network
/// (`router.lan`) would be reached outside the wrapper's guard; a
/// `ytdl://` one is handed to the hook (the wrapper received the full
/// address) and mpv opened nothing itself, as a line of an m3u read by
/// `loadlist` too. One rule for every site, YouTube included: its address
/// gains nothing from being opened by mpv first.
fn for_mpv(url: &str) -> String {
    format!("ytdl://{url}")
}

/// Now, on the clock the wrapper writes a failure's time with (`time.time()`).
fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The identity of a video: opaque to the core, what a `metadata` plugin
/// recognises it by.
fn video_identity(id: &str) -> serde_json::Value {
    serde_json::json!({ "kind": "video", "id": id })
}

/// What a list can play: available, and with an address fit to be written
/// down.
fn playable(videos: Vec<Video>) -> Vec<Video> {
    videos.into_iter().filter(|v| v.available && v.play_url().is_some()).collect()
}

/// A video preset's identity and address. YouTube's both come from the link;
/// another site's address is the link, and its identity what the probe said
/// (`extractor:id`), or the link itself for an entry saved without one.
fn single(entry: &Entry, link: &Link) -> Option<(String, String)> {
    let url = link.watch_url()?;
    let identity = match link {
        Link::Web { url } => entry.identity.clone().unwrap_or_else(|| url.clone()),
        _ => link.watch_id()?.to_string(),
    };
    Some((identity, url))
}

/// A playlist preset's cache key and address.
fn list_of(link: &Link) -> Option<(String, String)> {
    Some((link.cache_key()?, link.list_url()?))
}

/// What plays right now.
struct Playing {
    /// The videos written to the m3u, in order: mpv's index into them.
    /// `None` for a single video.
    list: Option<Vec<Video>>,
    /// When the address mpv is resolving now was handed over (the `Play`,
    /// then each move of mpv in its list): a failure the wrapper recorded
    /// after this is this playback's.
    since: f64,
}

/// Why a preset cannot play, and whether looking again at its playlist could
/// change that.
struct Refused {
    key: &'static str,
    /// The playlist whose cache is worth refreshing: its key and address.
    refresh: Option<(String, String)>,
}

impl Refused {
    fn plain(key: &'static str) -> Self {
        Self { key, refresh: None }
    }
}

/// What a preset is known by across a save of the table, which may give it
/// another number: its saved link and kind (the number is a position).
type EntryKey = (String, Kind);

fn key_of(entry: &Entry) -> EntryKey {
    (entry.url.clone(), entry.kind)
}

/// A preset that can play, with everything needed to start it.
enum Plan {
    Video { title: String, identity: String, url: String, bin: String, key: EntryKey },
    List { title: String, list: (String, String), videos: Vec<Video>, m3u: String, bin: String, key: EntryKey },
}

type InFlight = Arc<Mutex<HashSet<String>>>;

pub struct WebStreamingSource {
    data: PathBuf,
    entries: Arc<AsyncRwLock<Entries>>,
    tool: Ytdlp,
    /// The armed preset while stopped, the playing one otherwise.
    preset: u8,
    /// The entry at `preset` when it was played or armed: what a save that
    /// reorders the table is followed by (`poll_notification`).
    followed: Option<EntryKey>,
    playing: Option<Playing>,
    /// The playlists being refreshed right now: a second refresh of the same
    /// one is not started.
    refreshing: InFlight,
    /// Ticks after the admin half saved the table. `None` only in tests that
    /// have no admin half: `poll_notification` then waits forever, since
    /// `None` returned from it is terminal for the SDK.
    saved_rx: Option<tokio::sync::watch::Receiver<()>>,
}

impl WebStreamingSource {
    pub fn new(
        data: PathBuf,
        entries: Arc<AsyncRwLock<Entries>>,
        saved_rx: Option<tokio::sync::watch::Receiver<()>>,
    ) -> Self {
        let tool = Ytdlp::new(&data);
        let preset = state::load(&data).unwrap_or(1);
        // Built before anything else can hold the lock; a contended one only
        // leaves the armed preset unfollowed until it is played or armed.
        let followed = entries.try_read().ok().and_then(|e| e.by_preset(preset).map(key_of));
        Self { data, entries, tool, preset, followed, playing: None, refreshing: InFlight::default(), saved_rx }
    }

    /// A refusal: nothing was launched, so what plays carries on. Ephemeral,
    /// and no identity declaration (`plays_nothing()` would be false).
    fn refusal(key: &str, count: u8) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop).transient().status_text(keyed(key)).preset_count(count)
    }

    /// Makes `n` the current preset, and remembers it across restarts.
    fn set_preset(&mut self, n: u8) {
        if n != self.preset
            && let Err(e) = state::save(&self.data, n)
        {
            tracing::warn!("failed to persist preset: {e:#}");
        }
        self.preset = n;
    }

    fn start_refresh(&self, (key, url): &(String, String)) {
        start_refresh(&self.data, &self.refreshing, key, url);
    }

    /// Whether preset `n` can play right now, without doing anything.
    fn plan(&self, entries: &Entries, n: u8) -> Result<Plan, Refused> {
        let entry = entries.by_preset(n).ok_or(Refused::plain("empty_preset"))?;
        if !self.tool.installed() {
            return Err(Refused::plain("tool_missing"));
        }
        let bin = self.tool.bin().to_str().ok_or(Refused::plain("tool_path_invalid"))?.to_string();
        let link = parse_link(&entry.url).map_err(|_| Refused::plain("bad_entry"))?;
        match entry.kind {
            Kind::Video => {
                let (identity, url) = single(entry, &link).ok_or(Refused::plain("bad_entry"))?;
                Ok(Plan::Video { title: entry.title.clone(), identity, url, bin, key: key_of(entry) })
            }
            Kind::Playlist => {
                let list = list_of(&link).ok_or(Refused::plain("bad_entry"))?;
                let refresh = Some(list.clone());
                let cached = ListCache::new(&self.data)
                    .load(&list.0)
                    .ok_or(Refused { key: "list_not_cached", refresh: refresh.clone() })?;
                let videos = playable(cached);
                if videos.is_empty() {
                    return Err(Refused { key: "list_empty", refresh });
                }
                // Checked before anything is written.
                let m3u = self.data.join("playlist.m3u").to_str().ok_or(Refused::plain("tool_path_invalid"))?.to_string();
                Ok(Plan::List { title: entry.title.clone(), list, videos, m3u, bin, key: key_of(entry) })
            }
        }
    }

    /// What Play would start at the armed preset, for the screen. A playlist
    /// is armed by the video that would play first, in the shape the playing
    /// identity has; one that is not cached arms nothing.
    fn armed_identity(&self, entries: &Entries) -> Option<serde_json::Value> {
        let entry = entries.by_preset(self.preset)?;
        let link = parse_link(&entry.url).ok()?;
        match entry.kind {
            Kind::Video => Some(video_identity(&single(entry, &link)?.0)),
            Kind::Playlist => {
                let first = playable(ListCache::new(&self.data).load(&link.cache_key()?)?).into_iter().next()?;
                Some(video_identity(&first.identity()))
            }
        }
    }

    /// What the source says while nothing plays: the preset Play would start
    /// stays armed, with its number and name on screen.
    async fn armed_outcome(&mut self) -> SourceOutcome {
        let entries = self.entries.read().await;
        let count = entries.preset_count();
        self.followed = entries.by_preset(self.preset).map(key_of);
        match (self.armed_identity(&entries), entries.by_preset(self.preset)) {
            (Some(identity), Some(entry)) => SourceOutcome::new(SourceAction::Noop)
                .armed(identity)
                .preset(self.preset)
                .preset_name(entry.title.clone())
                .preset_count(count),
            _ => SourceOutcome::new(SourceAction::Noop).plays_nothing().preset_count(count),
        }
    }

    async fn arm_preset(&mut self, n: u8) -> SourceOutcome {
        self.set_preset(n);
        self.armed_outcome().await
    }

    async fn play_preset(&mut self, n: u8) -> SourceOutcome {
        let snapshot = self.entries.read().await.clone();
        let count = snapshot.preset_count();
        match self.plan(&snapshot, n) {
            Ok(plan) => self.commit(plan, n, count),
            Err(r) => {
                if let Some(list) = &r.refresh {
                    self.start_refresh(list);
                }
                Self::refusal(r.key, count)
            }
        }
    }

    /// Starts a plan that could play. Nothing about the source's state moves
    /// before this point, so a refusal leaves it exactly as it was.
    fn commit(&mut self, plan: Plan, n: u8, count: u8) -> SourceOutcome {
        match plan {
            Plan::Video { identity, url, bin, title: name, key } => {
                self.set_preset(n);
                self.followed = Some(key);
                self.playing = Some(Playing { list: None, since: now() });
                SourceOutcome::new(SourceAction::play(for_mpv(&url)).finite().ytdl_path(bin))
                    .plays_at(video_identity(&identity), url)
                    .preset(n)
                    .preset_name(name.clone())
                    .preset_count(count)
                    .status_text(video_title(&name))
            }
            Plan::List { list, videos, m3u, bin, title: name, key } => {
                // `playable` kept only entries that have an address: one line
                // per video, so mpv's index is the list's.
                let urls: Vec<String> =
                    videos.iter().map(|v| v.play_url().expect("the list holds playable videos only")).collect();
                let body: String = urls.iter().map(|u| format!("{}\n", for_mpv(u))).collect();
                if let Err(e) = write_atomically(Path::new(&m3u), body.as_bytes()) {
                    tracing::warn!("cannot write the playlist file: {e:#}");
                    return Self::refusal("playlist_write_failed", count);
                }
                let first = videos[0].clone();
                let outcome = SourceOutcome::new(SourceAction::play(m3u).playlist().starting_at(0).finite().ytdl_path(bin))
                    .plays_at(video_identity(&first.identity()), urls[0].clone())
                    .preset(n)
                    .preset_name(name)
                    .preset_count(count)
                    .status_text(list_title(&first, 0));
                self.set_preset(n);
                self.followed = Some(key);
                self.playing = Some(Playing { list: Some(videos), since: now() });
                self.start_refresh(&list);
                outcome
            }
        }
    }

    fn is_playing(&self) -> bool {
        self.playing.is_some()
    }

    fn plays_a_list(&self) -> bool {
        matches!(&self.playing, Some(Playing { list: Some(_), .. }))
    }

    /// Next or previous preset, as the radio does: one preset means nothing
    /// to do (replaying would restart the same video, audibly); stopped
    /// moves the armed preset without playing. A preset that cannot play is
    /// stepped over, in the same direction, at most once round the table; if
    /// none can play, nothing moves and the first refusal is said.
    async fn step(&mut self, forward: bool) -> SourceOutcome {
        if self.plays_a_list() {
            let action = if forward { SourceAction::PlayerNext } else { SourceAction::PlayerPrev };
            return SourceOutcome::new(action);
        }
        let snapshot = self.entries.read().await.clone();
        let count = snapshot.preset_count();
        let mut from = self.preset;
        let mut first_refusal: Option<&'static str> = None;
        // At most one attempt per preset of the table. The bound is the
        // table's, not "back at the current preset": that preset may be absent
        // from the table (deleted on the admin page, or never there), and a
        // walk that waits to meet it would circle forever without an `await`.
        for _ in 0..snapshot.entry.len() {
            let target = if forward { snapshot.next_preset(from) } else { snapshot.prev_preset(from) };
            match target {
                // Back where we started: every other preset was refused, or
                // there is only one.
                None => break,
                Some(n) if n == self.preset => break,
                Some(n) => match self.plan(&snapshot, n) {
                    Ok(_) if !self.is_playing() => return self.arm_preset(n).await,
                    Ok(plan) => return self.commit(plan, n, count),
                    Err(r) => {
                        if let Some(list) = &r.refresh {
                            self.start_refresh(list);
                        }
                        first_refusal.get_or_insert(r.key);
                        from = n;
                    }
                },
            }
        }
        match first_refusal {
            Some(key) => Self::refusal(key, count),
            None => SourceOutcome::new(SourceAction::Noop),
        }
    }
}

/// Re-reads the playlist at `url` with the tool and replaces the cache under
/// `key`. Any failure leaves the old cache untouched.
pub async fn refresh_list(data: &Path, key: &str, url: &str, deadline: Duration) -> Result<()> {
    let probe = Ytdlp::new(data).probe(url, deadline).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    anyhow::ensure!(probe.kind == Kind::Playlist, "{url} did not answer with a playlist");
    ListCache::new(data).store(key, &probe.entries)
}

/// Starts a refresh unless one is already running for this playlist. It never
/// blocks the caller, and never touches what plays: mpv already has its m3u.
fn start_refresh(data: &Path, in_flight: &InFlight, key: &str, url: &str) {
    let (data, key_owned, url) = (data.to_path_buf(), key.to_string(), url.to_string());
    spawn_single_flight(in_flight, key, async move {
        if let Err(e) = refresh_list(&data, &key_owned, &url, REFRESH_DEADLINE).await {
            tracing::warn!("refreshing playlist {url} failed, keeping the cached list: {e:#}");
        }
    });
}

/// Frees a playlist's slot when dropped: on success, on a panic and when the
/// task is aborted alike, so one bad run never blocks that playlist's
/// refreshes until the next restart.
struct InFlightGuard {
    set: InFlight,
    id: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// Runs `work` in the background unless `id` already has one running.
fn spawn_single_flight<F>(in_flight: &InFlight, id: &str, work: F) -> Option<tokio::task::JoinHandle<()>>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if !in_flight.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string()) {
        return None;
    }
    // Made before the task exists, so a task aborted before its first poll
    // still frees the slot when its future is dropped.
    let guard = InFlightGuard { set: in_flight.clone(), id: id.to_string() };
    Some(tokio::spawn(async move {
        let _guard = guard;
        work.await;
    }))
}

#[async_trait::async_trait]
impl SourcePlugin for WebStreamingSource {
    /// The icon the web UI shows this source under.
    fn icon(&self) -> Option<&'static str> {
        Some("headphones")
    }

    async fn activate(&mut self) -> SourceOutcome {
        // The armed preset, or the first one that exists after it when the
        // table does not hold it.
        let n = {
            let entries = self.entries.read().await;
            match entries.by_preset(self.preset) {
                Some(_) => self.preset,
                None => entries.next_preset(self.preset).unwrap_or(self.preset),
            }
        };
        self.play_preset(n).await
    }

    async fn deactivate(&mut self) -> SourceOutcome {
        self.playing = None;
        SourceOutcome::new(SourceAction::Stop).plays_nothing()
    }

    /// Stopped: the preset stays on screen, armed; Play starts it again.
    ///
    /// A playback whose resolution failed ends here too (mpv goes idle, the
    /// core turns that into a Stop): when the wrapper recorded a failure
    /// since this playback's address was handed over, the status says why,
    /// once (the next Stop starts from a new playback, or from none).
    async fn stop(&mut self) -> SourceOutcome {
        let since = self.playing.take().map(|p| p.since);
        let outcome = self.armed_outcome().await;
        match since.and_then(|since| PlaybackFailure::load(&self.data).filter(|f| f.time >= since)) {
            Some(failure) => outcome.status_text(keyed(failure.key())),
            None => outcome,
        }
    }

    async fn select(&mut self, n: u8) -> SourceOutcome {
        self.play_preset(n).await
    }

    async fn next(&mut self) -> SourceOutcome {
        self.step(true).await
    }

    async fn prev(&mut self) -> SourceOutcome {
        self.step(false).await
    }

    async fn eject(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop)
    }

    /// mpv moved on its own to entry `n` of the m3u it was given: name that
    /// video. Anything else (stopped, a single video, mpv's `-1`, an index
    /// past the list) declares nothing.
    async fn player_track(&mut self, n: i64) -> SourceOutcome {
        let video = match &mut self.playing {
            Some(Playing { list: Some(list), since }) => {
                // mpv moved on: a failure of the entry it left is not this
                // one's (it played, or mpv skipped it to reach this one).
                *since = now();
                usize::try_from(n).ok().and_then(|i| list.get(i).map(|v| (i, v)))
            }
            _ => None,
        };
        match video.and_then(|(i, v)| Some((i, v, v.play_url()?))) {
            Some((i, v, url)) => SourceOutcome::new(SourceAction::Noop)
                .plays_at(video_identity(&v.identity()), url)
                .status_text(list_title(v, i)),
            None => SourceOutcome::new(SourceAction::Noop),
        }
    }

    async fn list_presets(&mut self) -> Vec<Preset> {
        self.entries.read().await.presets()
    }

    /// After the admin half saved the table: the new count and names, and the
    /// **number and name of what plays or is armed**, followed by its link
    /// across the save (the radio's fix for the same defect: the preset is a
    /// position, so a reorder made the card name another preset, Stop arm
    /// it, and a reboot play it). No status and no action, so nothing that
    /// plays is disturbed.
    ///
    /// - Found again: its number, persisted if it moved, and its name (a
    ///   rename shows at once).
    /// - Gone while playing: it keeps playing, and no number is declared
    ///   rather than one that now names another preset.
    /// - Gone while stopped: re-armed from the number, as Play would start
    ///   it — whatever now sits there, or nothing.
    async fn poll_notification(&mut self) -> Option<Notification> {
        let Some(rx) = &mut self.saved_rx else { return std::future::pending().await };
        match rx.changed().await {
            Ok(()) => {
                rx.borrow_and_update();
                let snapshot = self.entries.read().await.clone();
                let mut notice =
                    Notification::new().preset_count(snapshot.preset_count()).presets(snapshot.presets());
                let Some(key) = self.followed.clone() else {
                    // Nothing was played or armed from the table: nothing to
                    // set straight.
                    return Some(notice);
                };
                match snapshot.entry.iter().find(|e| key_of(e) == key).map(|e| (e.preset, e.title.clone())) {
                    Some((p, name)) => {
                        self.set_preset(p);
                        notice = notice.preset(p).preset_name(name);
                    }
                    None if self.is_playing() => {}
                    None => {
                        self.followed = snapshot.by_preset(self.preset).map(key_of);
                        notice = match (self.armed_identity(&snapshot), snapshot.by_preset(self.preset)) {
                            (Some(identity), Some(entry)) => {
                                notice.armed(identity).preset(self.preset).preset_name(entry.title.clone())
                            }
                            _ => notice.plays_nothing(),
                        };
                    }
                }
                Some(notice)
            }
            // The sender is gone: not a reason to end notifications, which
            // would be terminal.
            Err(_) => std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests;
