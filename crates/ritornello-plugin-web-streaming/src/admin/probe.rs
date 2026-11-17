//! Reading the links the operator pastes, and adding what they name.
//!
//! The lines of a send are judged at once (pure) and appended to the lines
//! shown; a refused one is an error line from the start. The others wait
//! their turn: **one** spawned worker probes them, a line after the other,
//! each under a deadline, and settles each as soon as it answers. A send that
//! arrives while the worker runs is queued behind the lines already there.
//!
//! Settling never waits for the operator when there is nothing to choose: a
//! link that names one thing is added to the table at once (through
//! [`Store::change`]) and its line goes. Only a link naming a video and its
//! playlist stays, `found`, until the operator chooses (`choose`) or drops it
//! (`dismiss`). A link whose every choice is already in the table, a full
//! table and a failed write are error lines. A line dismissed while it is
//! probed adds nothing when its answer comes.
//!
//! The videos of every playlist found are kept in memory until the change
//! that adds it caches them, so a playlist is playable the first time it is
//! selected.

use super::store::Store;
use super::{keyed, Text, MAX_LINKS};
use ritornello_plugin_web_streaming::entries::{Entries, Entry};
use ritornello_plugin_web_streaming::link::{parse_lines, Link, LinkError};
use ritornello_plugin_web_streaming::ytdlp::{Kind, PROBE_DEADLINE, ToolError, Video, Ytdlp};
use serde::Serialize;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Playlists kept until the change that adds them: the oldest go first.
const KEPT_LISTS: usize = 20;

/// What the page shows of a failure. The code names it (the page translates
/// it); the detail is the tool's own last line, shown as it is.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Fault {
    pub code: Cow<'static, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Fault {
    pub fn of_link(e: &LinkError) -> Self {
        match e {
            LinkError::NotAUrl => Fault { code: "not_a_url".into(), detail: None },
            LinkError::ForeignHost(host) => Fault { code: "foreign_host".into(), detail: Some(host.clone()) },
            LinkError::NoMedia => Fault { code: "no_media".into(), detail: None },
            LinkError::LiveStream => Fault { code: "live_stream".into(), detail: None },
            LinkError::LocalNetwork(host) => Fault { code: "local_network".into(), detail: Some(host.clone()) },
        }
    }

    pub fn of_tool(e: &ToolError) -> Self {
        match e {
            ToolError::Missing => Fault { code: "tool_missing".into(), detail: None },
            ToolError::Timeout => Fault { code: "timeout".into(), detail: None },
            // What the plugin itself wrote (an exit with no word, an answer
            // that cannot be read, the guard's refusal) by its code; yt-dlp's
            // own words as `failed`, behind "yt-dlp: ".
            ToolError::Failed(detail) => {
                let c = ritornello_plugin_web_streaming::reason::coded(detail);
                Fault { code: c.code.into(), detail: c.detail }
            }
            ToolError::LiveStream => Fault { code: "live_stream".into(), detail: None },
        }
    }
}

/// What a link can become: a preset to add (without its number).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Found {
    pub url: String,
    pub kind: Kind,
    pub title: String,
    pub channel: Option<String>,
    /// Videos in a playlist.
    pub count: Option<u32>,
    /// The probe's extractor: the site, shown next to the title.
    pub site: Option<String>,
    /// A video of another site than YouTube: what it is known by
    /// (`extractor:id`), kept with the preset.
    pub identity: Option<String>,
}

/// A choice offered to the operator, and whether the table already held it
/// when the line was settled.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Choice {
    #[serde(flatten)]
    pub found: Found,
    pub present: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    /// Waiting its turn.
    Pending,
    /// yt-dlp is running on it.
    Probing,
    /// A link naming a video and its playlist: the operator decides.
    Found { choices: Vec<Choice> },
    Error(Fault),
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Line {
    /// Never reused while the plugin runs: the page names a line by it.
    pub id: u64,
    pub line: String,
    #[serde(flatten)]
    pub outcome: Outcome,
    /// What the worker probes; none for a line refused at once.
    #[serde(skip)]
    link: Option<Link>,
}

#[derive(Default)]
pub struct ProbeState {
    /// A worker is alive: it exits once no line is pending.
    running: bool,
    next_id: u64,
    /// The lines still shown, in the order sent.
    results: Vec<Line>,
    /// The videos of the playlists seen, by cache key, oldest first.
    lists: Vec<(String, Vec<Video>)>,
}

impl ProbeState {
    pub fn view(&self) -> serde_json::Value {
        serde_json::json!({ "running": self.running, "results": self.results })
    }

    fn remember(&mut self, id: &str, videos: Vec<Video>) {
        self.lists.retain(|(known, _)| known != id);
        self.lists.push((id.to_string(), videos));
        if self.lists.len() > KEPT_LISTS {
            self.lists.remove(0);
        }
    }

    /// The remembered videos of the playlists a table holds.
    pub fn lists_for(&self, table: &Entries) -> HashMap<String, Vec<Video>> {
        let wanted: Vec<String> = playlist_ids(table);
        self.lists.iter().filter(|(id, _)| wanted.contains(id)).cloned().collect()
    }

    /// Once saved they are on disk.
    pub fn forget_lists_of(&mut self, table: &Entries) {
        let saved = playlist_ids(table);
        self.lists.retain(|(id, _)| !saved.contains(id));
    }

    fn has(&self, id: u64) -> bool {
        self.results.iter().any(|l| l.id == id)
    }

    fn line_mut(&mut self, id: u64) -> Option<&mut Line> {
        self.results.iter_mut().find(|l| l.id == id)
    }

    /// The choice `url` of the `found` line `id`.
    pub fn choice(&self, id: u64, url: &str) -> Option<Found> {
        let line = self.results.iter().find(|l| l.id == id)?;
        let Outcome::Found { choices } = &line.outcome else { return None };
        choices.iter().find(|c| c.found.url == url).map(|c| c.found.clone())
    }

    /// Forgets a line, whatever its state; one already gone is no error.
    pub fn dismiss(&mut self, id: u64) {
        self.results.retain(|l| l.id != id);
    }
}

fn playlist_ids(table: &Entries) -> Vec<String> {
    table
        .entry
        .iter()
        .filter(|e| e.kind == Kind::Playlist)
        .filter_map(|e| ritornello_plugin_web_streaming::link::parse_link(&e.url).ok()?.cache_key())
        .collect()
}

fn lock(probe: &Mutex<ProbeState>) -> std::sync::MutexGuard<'_, ProbeState> {
    probe.lock().unwrap_or_else(|p| p.into_inner())
}

/// The most presets a table holds (`entries` numbers them 1 to 99).
const MAX_PRESETS: usize = 99;

/// Adds `found` at the end of the table, judged under the store's edit lock:
/// `already_present` when its address is there, `table_full` at 99 presets
/// (before the renumbering would run past 99).
fn push(rows: &mut Vec<Entry>, found: &Found) -> Result<(), Text> {
    if rows.iter().any(|e| e.url == found.url) {
        return Err(keyed("already_present"));
    }
    if rows.len() >= MAX_PRESETS {
        return Err(keyed("table_full"));
    }
    rows.push(Entry {
        // Numbered by position by the store.
        preset: 0,
        url: found.url.clone(),
        kind: found.kind,
        title: found.title.clone(),
        channel: found.channel.clone(),
        count: found.count,
        site: found.site.clone(),
        identity: found.identity.clone(),
    });
    Ok(())
}

/// Adds `found` for line `id` as long as that line is still shown when the
/// change runs (a dismissed line adds nothing: `stale`); the line then goes.
/// A refusal or a failed write leaves the line as it was.
pub async fn add(probe: &Mutex<ProbeState>, store: &Store, id: u64, found: Found) -> Result<(), Text> {
    store
        .change(probe, |rows| {
            if !lock(probe).has(id) {
                return Err(keyed("stale"));
            }
            push(rows, &found)
        })
        .await?;
    lock(probe).dismiss(id);
    Ok(())
}

/// Clears `running` if the worker dies on its way (a panic). Disarmed when
/// it exits normally: that exit cleared it under the lock already, and a
/// `start` may have set it again for a new worker since.
struct Running {
    shared: Arc<super::Shared>,
    armed: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.armed {
            lock(&self.shared.probe).running = false;
        }
    }
}

/// Judges the lines and appends them; starts a worker unless one runs (that
/// one takes the new lines in turn). Returns at once.
pub fn start(shared: &Arc<super::Shared>, ytdlp: &Arc<Ytdlp>, store: &Arc<Store>, text: &str) -> Result<(), Text> {
    let lines = parse_lines(text);
    if lines.is_empty() {
        return Err(keyed("no_links"));
    }
    if lines.len() > MAX_LINKS {
        return Err(Text::Keyed {
            key: "too_many_links".into(),
            params: HashMap::from([("max".to_string(), MAX_LINKS.to_string())]),
        });
    }
    let mut state = lock(&shared.probe);
    for (line, link) in lines {
        let id = state.next_id;
        state.next_id += 1;
        let (outcome, link) = match link {
            Ok(link) => (Outcome::Pending, Some(link)),
            Err(e) => (Outcome::Error(Fault::of_link(&e)), None),
        };
        state.results.push(Line { id, line, outcome, link });
    }
    let pending = state.results.iter().any(|l| l.outcome == Outcome::Pending);
    if state.running || !pending {
        return Ok(());
    }
    state.running = true;
    drop(state);
    let (shared, ytdlp, store) = (shared.clone(), ytdlp.clone(), store.clone());
    tokio::spawn(async move {
        let mut running = Running { shared: shared.clone(), armed: true };
        loop {
            // Decided under the lock: a `start` that comes after this exit
            // sees `running` false and spawns a worker of its own.
            let next = {
                let mut state = lock(&shared.probe);
                match state.results.iter_mut().find(|l| l.outcome == Outcome::Pending) {
                    Some(line) => {
                        line.outcome = Outcome::Probing;
                        line.link.clone().map(|link| (line.id, link))
                    }
                    None => {
                        state.running = false;
                        running.armed = false;
                        break;
                    }
                }
            };
            // A pending line always has its link.
            let Some((id, link)) = next else { continue };
            let outcome = probe_link(&shared, &ytdlp, &link).await;
            settle(&shared.probe, &store, id, outcome).await;
        }
    });
    Ok(())
}

/// Puts a probe's answer on its line, and adds what it found when there is
/// nothing to choose. The probe's lock is never held across an await: the
/// store takes it itself.
async fn settle(probe: &Mutex<ProbeState>, store: &Store, id: u64, outcome: Result<Vec<Found>, Fault>) {
    let set = |outcome: Outcome| {
        // A line dismissed meanwhile is gone: nothing to show.
        if let Some(line) = lock(probe).line_mut(id) {
            line.outcome = outcome;
        }
    };
    let mut choices = match outcome {
        Ok(choices) => choices,
        Err(fault) => return set(Outcome::Error(fault)),
    };
    if choices.len() == 1 {
        // Whether it is there already is judged by the change itself, under
        // the store's lock: two lines naming the same link add it once.
        return match add(probe, store, id, choices.remove(0)).await {
            Ok(()) => {}
            Err(Text::Keyed { key, .. }) => set(Outcome::Error(Fault { code: key.into(), detail: None })),
            Err(other) => {
                tracing::warn!("probe line not settled: {other:?}");
                set(Outcome::Error(Fault { code: "save_failed".into(), detail: None }));
            }
        };
    }
    let urls: HashSet<String> = store.urls().await;
    let choices: Vec<Choice> =
        choices.into_iter().map(|found| Choice { present: urls.contains(&found.url), found }).collect();
    set(if choices.iter().all(|c| c.present) {
        Outcome::Error(Fault { code: "already_present".into(), detail: None })
    } else {
        Outcome::Found { choices }
    });
}

/// A YouTube video link gives one choice, a playlist link one, a link naming
/// both gives both. When only one of two answers, the other is not offered;
/// when none does, the first failure is the line's. Another site's link is
/// probed once, and is whatever yt-dlp says it is.
async fn probe_link(shared: &super::Shared, ytdlp: &Ytdlp, link: &Link) -> Result<Vec<Found>, Fault> {
    let wanted: Vec<(Option<Kind>, Option<String>)> = match link {
        Link::Web { url } => vec![(None, Some(url.clone()))],
        _ => vec![(Some(Kind::Video), link.watch_url()), (Some(Kind::Playlist), link.list_url())],
    };
    let mut choices = Vec::new();
    let mut first_fault = None;
    for (wanted_kind, url) in wanted {
        let Some(url) = url else { continue };
        match ytdlp.probe(&url, PROBE_DEADLINE).await {
            Ok(p) if wanted_kind.is_none_or(|k| k == p.kind) => {
                let kind = p.kind;
                if kind == Kind::Playlist
                    && let Some(key) = link.cache_key()
                {
                    lock(&shared.probe).remember(&key, p.entries.clone());
                }
                let identity = match (link, kind, p.entries.first()) {
                    (Link::Web { .. }, Kind::Video, Some(v)) => Some(v.identity()),
                    _ => None,
                };
                choices.push(Found {
                    url,
                    kind,
                    title: p.title,
                    channel: p.channel,
                    count: (kind == Kind::Playlist).then(|| u32::try_from(p.entries.len()).unwrap_or(u32::MAX)),
                    site: (!p.extractor.is_empty()).then_some(p.extractor),
                    identity,
                });
            }
            Ok(_) => {
                first_fault.get_or_insert(Fault { code: "wrong_kind".into(), detail: None });
            }
            Err(e) => {
                first_fault.get_or_insert(Fault::of_tool(&e));
            }
        }
    }
    match (choices.is_empty(), first_fault) {
        (false, _) => Ok(choices),
        (true, Some(fault)) => Err(fault),
        (true, None) => Err(Fault { code: "no_media".into(), detail: None }),
    }
}
