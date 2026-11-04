//! Reading the links the operator pastes.
//!
//! The lines are judged at once (pure); the valid ones are probed by **one**
//! spawned task, a line after the other, each under a deadline. Its answers
//! are kept for `get_data`, and so are the videos of every playlist found:
//! `save` caches them, so a playlist the operator has just seen is playable
//! the first time it is selected.

use super::{keyed, Text, MAX_LINKS};
use ritornello_plugin_web_streaming::entries::Entries;
use ritornello_plugin_web_streaming::link::{parse_lines, Link, LinkError};
use ritornello_plugin_web_streaming::ytdlp::{Kind, PROBE_DEADLINE, ToolError, Video, Ytdlp};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// Playlists kept for the next save: the oldest go first.
const KEPT_LISTS: usize = 20;

/// What the page shows of a failure. The code names it (the page translates
/// it); the detail is the tool's own last line, shown as it is.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Fault {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Fault {
    pub fn of_link(e: &LinkError) -> Self {
        match e {
            LinkError::NotAUrl => Fault { code: "not_a_url", detail: None },
            LinkError::ForeignHost(host) => Fault { code: "foreign_host", detail: Some(host.clone()) },
            LinkError::NoMedia => Fault { code: "no_media", detail: None },
            LinkError::LiveStream => Fault { code: "live_stream", detail: None },
            LinkError::LocalNetwork(host) => Fault { code: "local_network", detail: Some(host.clone()) },
        }
    }

    pub fn of_tool(e: &ToolError) -> Self {
        match e {
            ToolError::Missing => Fault { code: "tool_missing", detail: None },
            ToolError::Timeout => Fault { code: "timeout", detail: None },
            // What the plugin itself wrote (an exit with no word, an answer
            // that cannot be read, the guard's refusal) by its code; yt-dlp's
            // own words as `failed`, behind "yt-dlp: ".
            ToolError::Failed(detail) => {
                let c = ritornello_plugin_web_streaming::reason::coded(detail);
                Fault { code: c.code, detail: c.detail }
            }
            ToolError::LiveStream => Fault { code: "live_stream", detail: None },
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

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    Pending,
    /// One choice, or two for a video link that names its playlist: the
    /// operator decides.
    Found { choices: Vec<Found> },
    Error(Fault),
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Line {
    pub line: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Default)]
pub struct ProbeState {
    running: bool,
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
}

fn playlist_ids(table: &Entries) -> Vec<String> {
    table
        .entry
        .iter()
        .filter(|e| e.kind == Kind::Playlist)
        .filter_map(|e| ritornello_plugin_web_streaming::link::parse_link(&e.url).ok()?.cache_key())
        .collect()
}

/// Clears `running` however the task ends.
struct Running(Arc<super::Shared>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.probe.lock().unwrap_or_else(|p| p.into_inner()).running = false;
    }
}

/// Judges the lines and, for the valid ones, starts the probing; returns at
/// once. Refused while a probe runs, so two never interleave their answers.
pub fn start(shared: &Arc<super::Shared>, ytdlp: &Arc<Ytdlp>, text: &str) -> Result<(), Text> {
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
    let mut state = shared.probe.lock().unwrap_or_else(|p| p.into_inner());
    if state.running {
        return Err(keyed("probe_busy"));
    }
    state.results = lines
        .iter()
        .map(|(line, link)| Line {
            line: line.clone(),
            outcome: match link {
                Ok(_) => Outcome::Pending,
                Err(e) => Outcome::Error(Fault::of_link(e)),
            },
        })
        .collect();
    let work: Vec<(usize, Link)> =
        lines.into_iter().enumerate().filter_map(|(i, (_, link))| link.ok().map(|l| (i, l))).collect();
    if work.is_empty() {
        return Ok(());
    }
    state.running = true;
    drop(state);
    let (shared, ytdlp) = (shared.clone(), ytdlp.clone());
    tokio::spawn(async move {
        let _running = Running(shared.clone());
        for (i, link) in work {
            let outcome = probe_link(&shared, &ytdlp, &link).await;
            if let Some(line) = shared.probe.lock().unwrap_or_else(|p| p.into_inner()).results.get_mut(i) {
                line.outcome = outcome;
            }
        }
    });
    Ok(())
}

/// A YouTube video link gives one choice, a playlist link one, a link naming
/// both gives both. When only one of two answers, the other is not offered;
/// when none does, the first failure is the line's. Another site's link is
/// probed once, and is whatever yt-dlp says it is.
async fn probe_link(shared: &super::Shared, ytdlp: &Ytdlp, link: &Link) -> Outcome {
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
                    shared.probe.lock().unwrap_or_else(|e| e.into_inner()).remember(&key, p.entries.clone());
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
                first_fault.get_or_insert(Fault { code: "wrong_kind", detail: None });
            }
            Err(e) => {
                first_fault.get_or_insert(Fault::of_tool(&e));
            }
        }
    }
    match (choices.is_empty(), first_fault) {
        (false, _) => Outcome::Found { choices },
        (true, Some(fault)) => Outcome::Error(fault),
        (true, None) => Outcome::Error(Fault { code: "no_media", detail: None }),
    }
}
