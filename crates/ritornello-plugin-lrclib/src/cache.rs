//! What LRCLIB already answered, kept on disk so a song heard every day is
//! asked about once.
//!
//! One JSON file per entry, named by a hash of its key, so an entry is
//! written, replaced or dropped alone and a torn write spoils one song, not
//! the cache. Lyrics stay on disk; memory holds only the index (key, kind,
//! dates, size), rebuilt from the directory at start.
//!
//! What is kept: lyrics found (no expiry — published lyrics do not change)
//! and absences (30 days — LRCLIB grows). Never an `Unavailable`: an outage
//! remembered as an absence would hide lyrics for a month.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ritornello_proto::{normalize_for_match, Lyrics};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::client::Query;

/// Ceiling on the cache directory: a few thousand songs' worth of lyrics,
/// small next to an SD card, large next to a living room's playlist.
pub const CACHE_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// How long an absence holds before LRCLIB is asked again.
pub const ABSENT_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

/// How stale the `used` date on disk may get. A read updates it in memory at
/// once, but writing a file at every track change would wear the SD card for
/// an eviction order that a day's error does not disturb.
const USED_WRITE_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Which song an entry is for: artist and title folded as matching folds
/// them, and the duration, since two versions of a song are two entries
/// (their lyrics differ in timing). The album is left out: it does not
/// decide the match either.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key(String);

impl Key {
    pub fn of(q: &Query) -> Self {
        // U+001F (unit separator) cannot survive normalization, so it cannot
        // occur inside a field.
        let duration = q.duration_s.map(|d| d.to_string()).unwrap_or_default();
        Self(format!(
            "{}\u{1f}{}\u{1f}{duration}",
            normalize_for_match(&q.artist),
            normalize_for_match(&q.title)
        ))
    }

    fn file_name(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}.json")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cached {
    Found(Lyrics),
    Absent { since: SystemTime },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub bytes: u64,
    pub found: u32,
    pub absent: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Found,
    Absent,
}

/// An entry as written on disk. Dates are Unix seconds.
#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lyrics: Option<Lyrics>,
    since: u64,
    used: u64,
}

/// What memory knows of an entry.
struct Slot {
    kind: Kind,
    since: u64,
    used: u64,
    /// The `used` the file carries, to know when it is a day behind.
    used_on_disk: u64,
    bytes: u64,
}

pub struct Cache {
    dir: PathBuf,
    cap: u64,
    index: HashMap<Key, Slot>,
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn from_secs(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

impl Cache {
    /// Opens the cache in `dir`, created if missing. Never fails: a cache
    /// that cannot be read is an empty cache, and the plugin works without.
    pub fn open(dir: &Path) -> Self {
        Self::open_with_cap(dir, CACHE_MAX_BYTES)
    }

    /// `open` with another ceiling, so eviction is tested on a few entries
    /// rather than twenty megabytes of them.
    fn open_with_cap(dir: &Path, cap: u64) -> Self {
        let mut cache = Self { dir: dir.to_path_buf(), cap, index: HashMap::new() };
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("lyrics cache {}: {e}", dir.display());
            return cache;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return cache };
        for entry in entries.flatten() {
            let path = entry.path();
            match cache.load(&path) {
                Some((key, slot)) => {
                    cache.index.insert(key, slot);
                }
                // Unreadable, truncated, a leftover temporary file, or a
                // name that does not match its key: this directory is ours,
                // and keeping such a file would only make it count against
                // the ceiling forever.
                None => {
                    tracing::info!("lyrics cache: dropping {}", path.display());
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        cache.evict_past_cap(None);
        cache
    }

    fn load(&self, path: &Path) -> Option<(Key, Slot)> {
        let text = std::fs::read_to_string(path).ok()?;
        let entry: Entry = serde_json::from_str(&text).ok()?;
        let key = Key(entry.key);
        if path.file_name()?.to_str()? != key.file_name() {
            return None;
        }
        if entry.kind == Kind::Found && entry.lyrics.is_none() {
            return None;
        }
        let slot = Slot {
            kind: entry.kind,
            since: entry.since,
            used: entry.used,
            used_on_disk: entry.used,
            bytes: text.len() as u64,
        };
        Some((key, slot))
    }

    pub fn get(&mut self, k: &Key, now: SystemTime) -> Option<Cached> {
        let slot = self.index.get(k)?;
        let (kind, since) = (slot.kind, slot.since);
        let lyrics = match kind {
            Kind::Absent => {
                // A clock set back makes the age zero, not an error.
                let age = now.duration_since(from_secs(since)).unwrap_or_default();
                if age > ABSENT_TTL {
                    self.remove(k);
                    return None;
                }
                None
            }
            Kind::Found => {
                // Read back from disk: the index holds no lyrics. A file
                // spoiled since the start is dropped like one spoiled before.
                let path = self.dir.join(k.file_name());
                let lyrics = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Entry>(&text).ok())
                    .filter(|e| e.key == k.0)
                    .and_then(|e| e.lyrics);
                if lyrics.is_none() {
                    tracing::info!("lyrics cache: dropping unreadable {}", path.display());
                    self.remove(k);
                    return None;
                }
                lyrics
            }
        };
        self.touch(k, kind, since, &lyrics, secs(now));
        Some(match lyrics {
            Some(lyrics) => Cached::Found(lyrics),
            None => Cached::Absent { since: from_secs(since) },
        })
    }

    /// Records a use: in memory always, on disk once the file's date is a
    /// day behind.
    fn touch(&mut self, k: &Key, kind: Kind, since: u64, lyrics: &Option<Lyrics>, now: u64) {
        let Some(slot) = self.index.get_mut(k) else { return };
        slot.used = slot.used.max(now);
        if now.saturating_sub(slot.used_on_disk) < USED_WRITE_INTERVAL.as_secs() {
            return;
        }
        let entry = Entry { key: k.0.clone(), kind, lyrics: lyrics.clone(), since, used: now };
        if let Some(bytes) = self.write(k, &entry)
            && let Some(slot) = self.index.get_mut(k)
        {
            slot.used_on_disk = now;
            slot.bytes = bytes;
        }
    }

    pub fn put(&mut self, k: &Key, v: Cached, now: SystemTime) {
        let now = secs(now);
        let (kind, lyrics, since) = match v {
            Cached::Found(lyrics) => (Kind::Found, Some(lyrics), now),
            Cached::Absent { since } => (Kind::Absent, None, secs(since)),
        };
        let entry = Entry { key: k.0.clone(), kind, lyrics, since, used: now };
        match self.write(k, &entry) {
            Some(bytes) => {
                self.index.insert(k.clone(), Slot { kind, since, used: now, used_on_disk: now, bytes });
                self.evict_past_cap(Some(k));
            }
            // The old file, if any, may or may not have been replaced: forget
            // the entry rather than describe a file we no longer know.
            None => self.remove(k),
        }
    }

    /// Empties the cache, on disk and in memory.
    pub fn clear(&mut self) -> std::io::Result<()> {
        self.index.clear();
        for entry in std::fs::read_dir(&self.dir)? {
            std::fs::remove_file(entry?.path())?;
        }
        Ok(())
    }

    pub fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for slot in self.index.values() {
            stats.bytes += slot.bytes;
            match slot.kind {
                Kind::Found => stats.found += 1,
                Kind::Absent => stats.absent += 1,
            }
        }
        stats
    }

    /// Writes `entry` to a temporary file, then renames it over the entry's
    /// file: a power cut leaves the old entry or the new one, never half of
    /// one. Returns the size written.
    fn write(&self, k: &Key, entry: &Entry) -> Option<u64> {
        let text = serde_json::to_string(entry).ok()?;
        let path = self.dir.join(k.file_name());
        let tmp = path.with_extension("json.tmp");
        let result = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)
        })();
        match result {
            Ok(()) => Some(text.len() as u64),
            Err(e) => {
                tracing::warn!("lyrics cache: cannot write {}: {e}", path.display());
                let _ = std::fs::remove_file(&tmp);
                None
            }
        }
    }

    fn remove(&mut self, k: &Key) {
        self.index.remove(k);
        let _ = std::fs::remove_file(self.dir.join(k.file_name()));
    }

    /// Drops the least recently used entries until the total is under the
    /// ceiling. `keep` is the entry just written: evicting it would make the
    /// write pointless.
    fn evict_past_cap(&mut self, keep: Option<&Key>) {
        let mut total: u64 = self.index.values().map(|s| s.bytes).sum();
        while total > self.cap {
            let oldest = self
                .index
                .iter()
                .filter(|(key, _)| Some(*key) != keep)
                .min_by_key(|(_, slot)| slot.used)
                .map(|(key, slot)| (key.clone(), slot.bytes));
            let Some((key, bytes)) = oldest else { break };
            self.remove(&key);
            total -= bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::parse_record;

    const GET_SYNCED: &str = include_str!("../tests/fixtures/get-synced.json");
    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
    }

    fn key(artist: &str) -> Key {
        Key::of(&Query { artist: artist.into(), title: "Song".into(), album: None, duration_s: Some(200) })
    }

    fn real_lyrics() -> Lyrics {
        parse_record(GET_SYNCED).unwrap().lyrics().unwrap()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_key_folds_names_and_keeps_the_duration() {
        let q = |artist: &str, title: &str, duration_s| Query {
            artist: artist.into(),
            title: title.into(),
            album: Some("ignored".into()),
            duration_s,
        };
        assert_eq!(Key::of(&q("Beyoncé", "Halo (Live)", Some(261))), Key::of(&q("BEYONCE", "Halo", Some(261))));
        assert_ne!(Key::of(&q("Daft Punk", "Get Lucky", Some(248))), Key::of(&q("Daft Punk", "Get Lucky", Some(367))));
        assert_ne!(Key::of(&q("Daft Punk", "Get Lucky", None)), Key::of(&q("Daft Punk", "Get Lucky", Some(367))));
        // The separator keeps "a b" + "c" apart from "a" + "b c".
        assert_ne!(Key::of(&q("a b", "c", None)), Key::of(&q("a", "b c", None)));
    }

    #[test]
    fn found_lyrics_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(dir.path());
        cache.put(&key("Daft Punk"), Cached::Found(real_lyrics()), at(0));
        drop(cache);
        let mut cache = Cache::open(dir.path());
        assert_eq!(cache.stats().found, 1);
        // Found lyrics never expire: a year later they are still there.
        assert_eq!(cache.get(&key("Daft Punk"), at(365 * 86_400)), Some(Cached::Found(real_lyrics())));
        assert_eq!(cache.get(&key("Someone Else"), at(0)), None);
    }

    #[test]
    fn an_absence_expires_after_thirty_days() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(dir.path());
        cache.put(&key("Nobody"), Cached::Absent { since: at(0) }, at(0));
        drop(cache);
        let mut cache = Cache::open(dir.path());
        assert_eq!(
            cache.get(&key("Nobody"), at(0) + ABSENT_TTL - DAY),
            Some(Cached::Absent { since: at(0) })
        );
        assert_eq!(cache.get(&key("Nobody"), at(0) + ABSENT_TTL + Duration::from_secs(1)), None);
        assert_eq!(cache.stats(), Stats::default(), "an expired absence is forgotten");
        assert!(files_in(dir.path()).is_empty(), "and its file removed");
    }

    #[test]
    fn the_least_recently_used_entries_go_first_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        // Measure one entry, then allow three and a half of them. The keys
        // differ by one letter, so every entry has the same size.
        let probe = tempfile::tempdir().unwrap();
        let mut sizer = Cache::open(probe.path());
        sizer.put(&key("x"), Cached::Found(real_lyrics()), at(0));
        let one = sizer.stats().bytes;
        assert!(one > 0);

        let mut cache = Cache::open_with_cap(dir.path(), one * 7 / 2);
        cache.put(&key("a"), Cached::Found(real_lyrics()), at(1));
        cache.put(&key("b"), Cached::Found(real_lyrics()), at(2));
        cache.put(&key("c"), Cached::Found(real_lyrics()), at(3));
        // "a" is the oldest written, but read since: "b" is now the least
        // recently used.
        assert!(cache.get(&key("a"), at(4)).is_some());
        cache.put(&key("d"), Cached::Found(real_lyrics()), at(5));

        assert_eq!(cache.stats().found, 3);
        assert!(cache.stats().bytes <= one * 7 / 2);
        assert_eq!(cache.get(&key("b"), at(6)), None, "the least recently used went");
        for kept in ["a", "c", "d"] {
            assert!(cache.get(&key(kept), at(6)).is_some(), "{kept} was evicted");
        }
        assert_eq!(files_in(dir.path()).len(), 3, "evicted means removed from disk");
    }

    #[test]
    fn an_unreadable_entry_is_dropped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(dir.path());
        cache.put(&key("Kept"), Cached::Found(real_lyrics()), at(0));
        cache.put(&key("Spoiled later"), Cached::Found(real_lyrics()), at(0));
        drop(cache);
        // A truncated write from before a power cut, and a stray temp file.
        std::fs::write(dir.path().join("0123abcd.json"), "{\"key\":\"x\",\"kind\":\"fou").unwrap();
        std::fs::write(dir.path().join("4567.json.tmp"), "half").unwrap();

        let mut cache = Cache::open(dir.path());
        assert_eq!(cache.stats().found, 2);
        assert_eq!(files_in(dir.path()).len(), 2, "the garbage is gone: {:?}", files_in(dir.path()));

        // Spoiled after opening: the read finds it and drops it.
        for name in files_in(dir.path()) {
            let path = dir.path().join(&name);
            if std::fs::read_to_string(&path).unwrap().contains("spoiled later") {
                std::fs::write(&path, "not json").unwrap();
            }
        }
        assert_eq!(cache.get(&key("Spoiled later"), at(1)), None);
        assert_eq!(cache.stats().found, 1);
        assert!(cache.get(&key("Kept"), at(1)).is_some());
    }

    #[test]
    fn clear_empties_the_directory_and_the_stats() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(dir.path());
        cache.put(&key("One"), Cached::Found(real_lyrics()), at(0));
        cache.put(&key("Two"), Cached::Absent { since: at(0) }, at(0));
        assert_eq!((cache.stats().found, cache.stats().absent), (1, 1));
        cache.clear().unwrap();
        assert_eq!(cache.stats(), Stats::default());
        assert!(files_in(dir.path()).is_empty());
        assert_eq!(cache.get(&key("One"), at(1)), None);
        // Still usable afterwards.
        cache.put(&key("Three"), Cached::Absent { since: at(2) }, at(2));
        assert_eq!(cache.stats().absent, 1);
    }

    #[test]
    fn a_read_is_written_back_at_most_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(dir.path());
        cache.put(&key("Daft Punk"), Cached::Found(real_lyrics()), at(0));
        let file = dir.path().join(&files_in(dir.path())[0]);
        let written = || std::fs::read_to_string(&file).unwrap();
        let before = written();
        assert!(cache.get(&key("Daft Punk"), at(3600)).is_some());
        assert_eq!(written(), before, "an hour later, the read stays in memory");
        assert!(cache.get(&key("Daft Punk"), at(0) + DAY + Duration::from_secs(1)).is_some());
        assert_ne!(written(), before, "a day later, the use reaches the disk");
    }
}
