//! The one place the table of presets changes.
//!
//! Every edit the page makes (remove, move, rename, and the ones that add) goes
//! through [`Store::change`]: it clones the table, lets the edit act on the
//! clone, renumbers by position, validates, writes, and only then swaps the
//! memory. A refused edit or a failed write changes nothing and tells nobody.

use super::probe::ProbeState;
use super::{keyed, keyed_with, SAVE_DEADLINE};
use ritornello_plugin_web_streaming::entries::{Entries, Entry, ListCache, ValidationError};
use ritornello_plugin_web_streaming::link::parse_link;
use ritornello_plugin_web_streaming::targets;
use ritornello_plugin_web_streaming::ytdlp::Kind;
use ritornello_proto::Text;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock as AsyncRwLock;

pub(crate) struct Store {
    data: PathBuf,
    entries: Arc<AsyncRwLock<Entries>>,
    /// Ticks after each successful change: the source half announces the new
    /// table to the core.
    saved: tokio::sync::watch::Sender<()>,
    /// Serialises the changes. The write lock of `entries` is NOT held while
    /// an edit runs and the files are written (the source half and `get_data`
    /// read it): it is taken for the swap only.
    edit: tokio::sync::Mutex<()>,
}

impl Store {
    pub fn new(data: PathBuf, entries: Arc<AsyncRwLock<Entries>>, saved: tokio::sync::watch::Sender<()>) -> Self {
        Self { data, entries, saved, edit: tokio::sync::Mutex::new(()) }
    }

    /// Applies `f` to a copy of the table, renumbers it by position (sorted by
    /// the current preset number first, then `preset = index + 1`), validates
    /// it, writes the cache of the playlists just probed and `entries.toml`
    /// under [`SAVE_DEADLINE`], then swaps the table in memory, forgets the
    /// cached lists and ticks `saved`. `f` runs under the edit lock: what it
    /// judges (a stale preset) cannot move before it is applied.
    ///
    /// A refused `f` or a failed write: nothing changes, nothing ticks.
    pub async fn change<F>(&self, probe: &Mutex<ProbeState>, f: F) -> Result<(), Text>
    where
        F: FnOnce(&mut Vec<Entry>) -> Result<(), Text> + Send,
    {
        let _editing = self.edit.lock().await;
        let mut rows = self.entries.read().await.entry.clone();
        rows.sort_by_key(|e| e.preset);
        f(&mut rows)?;
        for (index, row) in rows.iter_mut().enumerate() {
            row.preset = u8::try_from(index + 1).unwrap_or(u8::MAX);
        }
        let table = Entries { entry: rows };
        table.validate().map_err(|e| match e {
            // Positions only run past 99 (or 255, saturated): one preset too many.
            ValidationError::PresetOutOfRange { .. } => keyed("too_many_entries"),
            ValidationError::DuplicatePreset { preset } => keyed_with("duplicate_preset", "preset", preset),
        })?;
        for e in &table.entry {
            // A YouTube link must name what the preset says; another site's
            // link names whatever its probe found, so it fits either kind.
            let fits = parse_link(&e.url).ok().is_some_and(|l| match e.kind {
                Kind::Video => l.watch_url().is_some(),
                Kind::Playlist => l.cache_key().is_some(),
            });
            if !fits {
                return Err(keyed("bad_entry"));
            }
        }
        // The playlists the operator just probed are cached by this very
        // change: the first `select` of a new playlist never meets an empty
        // cache (which would refuse it, and refresh in the background).
        let probed = probe.lock().unwrap_or_else(|p| p.into_inner()).lists_for(&table);
        let (data, path) = (self.data.clone(), self.data.join("entries.toml"));
        let to_write = table.clone();
        let written = targets::blocking(SAVE_DEADLINE, move || {
            let cache = ListCache::new(&data);
            for (key, videos) in &probed {
                if let Err(e) = cache.store(key, videos) {
                    // The select then refreshes the cache by itself.
                    tracing::warn!("playlist {key} not cached: {e:#}");
                }
            }
            to_write.save(&path).map_err(|e| std::io::Error::other(format!("{e:#}")))
        })
        .await;
        if let Err(e) = written {
            // The path and the cause stay in the log, not on the page.
            tracing::warn!("entries not saved: {e}");
            return Err(keyed("save_failed"));
        }
        // Stored now: their copy in memory is not needed any more.
        probe.lock().unwrap_or_else(|p| p.into_inner()).forget_lists_of(&table);
        *self.entries.write().await = table;
        // On every successful change, as the radio does: the core's own merge
        // drops what did not change.
        self.saved.send_replace(());
        Ok(())
    }
}

/// The row a page names by its number and its address, or `stale`: the table
/// changed since the page drew it.
pub(super) fn find(rows: &[Entry], preset: u8, url: &str) -> Result<usize, Text> {
    rows.iter().position(|e| e.preset == preset && e.url == url).ok_or_else(|| keyed("stale"))
}
