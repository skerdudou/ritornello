//! Persisted state: the current playlist and the track being played.
//!
//! Same pattern as `crates/ritornello-plugin-radio/src/state.rs`, including the
//! `update` that preserves the fields it does not touch — the Admin half will
//! write into this same file, and a `save` rebuilt by the Source half would
//! erase it.

use ritornello_plugin_files::m3u::Entry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub playlist: Vec<StoredEntry>,
    #[serde(default)]
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEntry {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
}

impl From<&StoredEntry> for Entry {
    fn from(s: &StoredEntry) -> Self {
        Entry { path: s.path.clone(), title: s.title.clone(), duration_s: s.duration_s }
    }
}

impl From<&Entry> for StoredEntry {
    fn from(e: &Entry) -> Self {
        StoredEntry { path: e.path.clone(), title: e.title.clone(), duration_s: e.duration_s }
    }
}

/// A missing or unreadable file yields an empty state, **without panicking**: a
/// first installation, or an erased `/var/lib`, must let the plugin start and
/// not refuse to run.
pub fn load(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Atomic write: `.tmp` then `rename`, so that a power cut never leaves a
/// truncated file that the next startup would silently discard.
pub fn save(path: &Path, state: &State) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Re-reads, modifies, rewrites — and therefore preserves what the caller does not touch.
pub fn update(path: &Path, f: impl FnOnce(&mut State)) -> anyhow::Result<()> {
    let mut state = load(path);
    f(&mut state);
    save(path, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry() -> StoredEntry {
        StoredEntry {
            path: "/mnt/ritornello/nas/Album/01.mp3".into(),
            title: Some("So What".into()),
            duration_s: Some(245),
        }
    }

    #[test]
    fn a_missing_or_unreadable_state_gives_an_empty_state_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("absent.json")).index, 0);
        let damaged = dir.path().join("damaged.json");
        std::fs::write(&damaged, b"{ this is not json").unwrap();
        assert!(load(&damaged).playlist.is_empty());
    }

    #[test]
    fn the_playlist_and_the_index_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-files.json");
        save(&f, &State { playlist: vec![test_entry()], index: 0 }).unwrap();
        let reread = load(&f);
        assert_eq!(reread.index, 0);
        assert_eq!(reread.playlist, vec![test_entry()]);
    }

    #[test]
    fn update_does_not_lose_the_fields_it_does_not_touch() {
        // The Admin half writes the playlist into this same file; a `save`
        // rebuilt by the Source half would erase it.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-files.json");
        save(&f, &State { playlist: vec![test_entry()], index: 0 }).unwrap();
        update(&f, |s| s.index = 1).unwrap();
        let reread = load(&f);
        assert_eq!(reread.index, 1);
        assert_eq!(reread.playlist.len(), 1, "the playlist was erased by the update");
    }

    #[test]
    fn the_conversion_with_the_m3u_entry_round_trips() {
        let e = Entry {
            path: "/musique/01.mp3".into(),
            title: Some("So What".into()),
            duration_s: Some(245),
        };
        let stored = StoredEntry::from(&e);
        assert_eq!(Entry::from(&stored), e);
    }
}
