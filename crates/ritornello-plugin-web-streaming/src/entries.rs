//! The presets, and the playlists' cached contents, on disk.
//!
//! `entries.toml` holds the numbered presets in the same contract as the
//! radio's `stations.toml`: numbers 1..=99, unique, a missing file meaning a
//! first run (empty), an unreadable one an error the caller logs before
//! starting empty. `lists/<key>.json` holds the videos of each playlist as
//! the last successful probe saw them, under its `link::Link::cache_key`.

use crate::link::WEB_KEY_PREFIX;
use crate::ytdlp::{Kind, Video};
use anyhow::{Context, Result};
use ritornello_proto::Preset;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub preset: u8,
    /// The normalised address: on YouTube a `watch` URL for a video, a
    /// `playlist` URL for a playlist; on any other site the link itself.
    pub url: String,
    pub kind: Kind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Videos in the playlist, as last probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// The site, as yt-dlp names its extractor (`Soundcloud`,
    /// `BandcampAlbum`...): shown next to the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// What a video of another site than YouTube is known by
    /// (`extractor:id`, `ytdlp::Video::identity`), as its probe said: the
    /// link alone does not tell it. YouTube's comes from the link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entries {
    #[serde(default)]
    pub entry: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    PresetOutOfRange { preset: u8, title: String },
    DuplicatePreset { preset: u8 },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::PresetOutOfRange { preset, title } => {
                write!(f, "preset {preset} out of range 1-99 ({title})")
            }
            ValidationError::DuplicatePreset { preset } => write!(f, "duplicate preset {preset}"),
        }
    }
}

impl std::error::Error for ValidationError {}

impl Entries {
    /// A missing file is a first run: empty. An unreadable or invalid one is
    /// an error, for the caller to log.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let entries: Entries = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        entries.validate()?;
        Ok(entries)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        write_atomically(path, toml::to_string_pretty(self)?.as_bytes())
    }

    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        let mut seen = std::collections::HashSet::new();
        for e in &self.entry {
            if !(1..=99).contains(&e.preset) {
                return Err(ValidationError::PresetOutOfRange { preset: e.preset, title: e.title.clone() });
            }
            if !seen.insert(e.preset) {
                return Err(ValidationError::DuplicatePreset { preset: e.preset });
            }
        }
        Ok(())
    }

    pub fn by_preset(&self, preset: u8) -> Option<&Entry> {
        self.entry.iter().find(|e| e.preset == preset)
    }

    pub fn next_preset(&self, from: u8) -> Option<u8> {
        let mut p: Vec<u8> = self.entry.iter().map(|e| e.preset).collect();
        p.sort_unstable();
        p.iter().copied().find(|x| *x > from).or_else(|| p.first().copied())
    }

    pub fn prev_preset(&self, from: u8) -> Option<u8> {
        let mut p: Vec<u8> = self.entry.iter().map(|e| e.preset).collect();
        p.sort_unstable();
        p.iter().rev().copied().find(|x| *x < from).or_else(|| p.last().copied())
    }

    /// Highest preset number, what the web grid shows (sparse tables expose
    /// a few empty numbers, answered by the "empty preset" transient).
    pub fn preset_count(&self) -> u8 {
        self.entry.iter().map(|e| e.preset).max().unwrap_or(0)
    }

    /// The named presets, sorted by number.
    pub fn presets(&self) -> Vec<Preset> {
        let mut v: Vec<Preset> =
            self.entry.iter().map(|e| Preset { index: e.preset, name: e.title.clone() }).collect();
        v.sort_by_key(|p| p.index);
        v
    }
}

/// Writes to a sibling temporary file, then renames it over `path`, so a
/// reader never sees half a file.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    // Unique to this process and to this call: two writers of the same file
    // (a refresh and a select, two refreshes) must never share a temporary
    // name, or one steals the file from under the other.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp_name = path.file_name().context("no file name")?.to_os_string();
    tmp_name.push(format!(".tmp.{}.{n}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming onto {}", path.display()));
    }
    Ok(())
}

/// `lists/<key>.json` under the data directory.
pub struct ListCache {
    dir: PathBuf,
}

/// A key is a file name: what a YouTube list id is made of, or `web.` and
/// exactly 64 lowercase hex digits.
fn is_cache_key(key: &str) -> bool {
    let list_id = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let web = key
        .strip_prefix(WEB_KEY_PREFIX)
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    list_id || web
}

impl ListCache {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("lists") }
    }

    fn path(&self, id: &str) -> Option<PathBuf> {
        is_cache_key(id).then(|| self.dir.join(format!("{id}.json")))
    }

    /// None when nothing is cached or the file is unreadable.
    pub fn load(&self, id: &str) -> Option<Vec<Video>> {
        let text = std::fs::read_to_string(self.path(id)?).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn store(&self, id: &str, videos: &[Video]) -> Result<()> {
        let path = self.path(id).with_context(|| format!("not a playlist id: {id:?}"))?;
        write_atomically(&path, serde_json::to_string(videos)?.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(preset: u8, title: &str, kind: Kind) -> Entry {
        let url = match kind {
            Kind::Video => "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            Kind::Playlist => "https://www.youtube.com/playlist?list=PLx",
        };
        Entry { preset, url: url.into(), kind, title: title.into(), channel: None, count: None, site: None, identity: None }
    }

    fn sample() -> Entries {
        Entries {
            entry: vec![
                Entry { channel: Some("Rick".into()), ..entry(1, "Never gonna", Kind::Video) },
                Entry { count: Some(12), ..entry(3, "A list", Kind::Playlist) },
            ],
        }
    }

    #[test]
    fn entries_round_trip_and_validate_like_stations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        sample().save(&path).unwrap();
        assert_eq!(Entries::load(&path).unwrap(), sample());
        assert!(!dir.path().join("entries.toml.tmp").exists(), "the temporary file is renamed away");

        let mut s = sample();
        s.entry[1].preset = 1;
        assert_eq!(s.validate(), Err(ValidationError::DuplicatePreset { preset: 1 }));
        assert!(s.save(&path).is_err());
        assert_eq!(Entries::load(&path).unwrap(), sample(), "a refused save leaves the file alone");

        for bad in [0u8, 100] {
            let mut s = sample();
            s.entry[0].preset = bad;
            assert!(matches!(s.validate(), Err(ValidationError::PresetOutOfRange { .. })), "{bad}");
        }
        let mut s = sample();
        s.entry[0].preset = 99;
        assert!(s.validate().is_ok());

        assert_eq!(sample().preset_count(), 3);
        assert_eq!(Entries::default().preset_count(), 0);
        assert_eq!(
            sample().presets(),
            vec![Preset { index: 1, name: "Never gonna".into() }, Preset { index: 3, name: "A list".into() }]
        );
        assert_eq!(sample().by_preset(3).unwrap().title, "A list");
        assert!(sample().by_preset(2).is_none());
        assert_eq!(sample().next_preset(1), Some(3));
        assert_eq!(sample().next_preset(3), Some(1));
        assert_eq!(sample().prev_preset(1), Some(3));
        assert_eq!(sample().prev_preset(3), Some(1));
    }

    #[test]
    fn a_web_entry_keeps_its_site_and_identity_and_a_youtube_one_writes_neither() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        let web = Entry {
            preset: 2,
            url: "https://soundcloud.com/forss/flickermood".into(),
            kind: Kind::Video,
            title: "Flickermood".into(),
            channel: None,
            count: None,
            site: Some("Soundcloud".into()),
            identity: Some("Soundcloud:293".into()),
        };
        let table = Entries { entry: vec![entry(1, "Never gonna", Kind::Video), web] };
        table.save(&path).unwrap();
        assert_eq!(Entries::load(&path).unwrap(), table);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("site =").count(), 1, "{text}");
        assert_eq!(text.matches("identity =").count(), 1, "{text}");
    }

    #[test]
    fn a_missing_file_is_empty_and_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        assert_eq!(Entries::load(&path).unwrap(), Entries::default());
        std::fs::write(&path, "this is [not toml").unwrap();
        assert!(Entries::load(&path).is_err());
        // Parsable but invalid is refused as well, rather than half-loaded.
        std::fs::write(&path, "[[entry]]\npreset = 100\nurl = \"u\"\nkind = \"video\"\ntitle = \"t\"\n").unwrap();
        assert!(Entries::load(&path).is_err());
    }

    #[test]
    fn concurrent_writers_of_one_file_never_share_a_temporary_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lists/PLx.json");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for j in 0..50 {
                        write_atomically(&path, format!("[{i},{j}]").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(std::fs::read_to_string(&path).unwrap().starts_with('['));
        let leftovers = std::fs::read_dir(dir.path().join("lists")).unwrap().count();
        assert_eq!(leftovers, 1, "no temporary file is left behind");
    }

    #[test]
    fn the_list_cache_round_trips_and_refuses_ids_that_are_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ListCache::new(dir.path());
        let v = vec![Video {
            extractor: "Youtube".into(),
            id: "a".into(),
            title: "A".into(),
            duration: Some(3),
            available: true,
            thumbnail_jpg: None,
            url: None,
        }];
        assert_eq!(cache.load("PLx"), None);
        cache.store("PLx", &v).unwrap();
        assert_eq!(cache.load("PLx"), Some(v.clone()));
        assert!(dir.path().join("lists/PLx.json").is_file());
        // A generic list's key.
        let web = format!("web.{}", "0123456789abcdef".repeat(4));
        cache.store(&web, &v).unwrap();
        assert_eq!(cache.load(&web), Some(v.clone()));
        let hex = "0123456789abcdef".repeat(4);
        for bad in [
            "".to_string(),
            "../x".into(),
            "a/b".into(),
            "a.b".into(),
            "web.".into(),
            format!("web.{}", &hex[..63]),
            format!("web.{hex}0"),
            format!("web.{}", hex.to_uppercase()),
            format!("web.{}g", &hex[..63]),
            format!("web.{}/", &hex[..63]),
            format!("Web.{hex}"),
            format!("x.{hex}"),
        ] {
            let bad = bad.as_str();
            assert!(cache.store(bad, &v).is_err(), "{bad}");
            assert_eq!(cache.load(bad), None, "{bad}");
        }
        std::fs::write(dir.path().join("lists/PLy.json"), "garbage").unwrap();
        assert_eq!(cache.load("PLy"), None);
    }
}
