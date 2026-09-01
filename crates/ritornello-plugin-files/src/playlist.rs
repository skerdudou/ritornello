//! The current playlist: the tracks, the current track, and the m3u we give to
//! mpv.

use crate::m3u::{render, Entry};
use ritornello_proto::Preset;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playlist {
    pub entries: Vec<Entry>,
    pub index: usize,
}

impl Playlist {
    /// How many tracks carry a remote-control digit.
    ///
    /// `preset` is a `u8` with range 1–99: beyond that, the tracks remain
    /// reachable through next/prev and through the page's list, but no digit
    /// designates them. This is not worked around — it is declared.
    pub fn preset_count(&self) -> u8 {
        self.entries.len().min(99) as u8
    }

    /// The **named** presets: a number and the title of the track.
    ///
    /// The source long announced only a `preset_count`, so that the home page
    /// grid showed only bare numbers where the radio shows "1 · FIP". The name
    /// already existed though — it is the one `preset_name` publishes for the
    /// current track, and the one the m3u writes as `#EXTINF`.
    ///
    /// **Dense and capped at 99, exactly like `preset_count`**: the two
    /// describe the same thing and must stay in agreement. A list of files has
    /// no holes — the numbers follow the positions — so the index is indeed
    /// "the position plus one", which is *not* true of a sparse station table
    /// (see the MPD plugin doc, § Dense positions, sparse indices).
    pub fn presets(&self) -> Vec<Preset> {
        self.entries
            .iter()
            .take(usize::from(self.preset_count()))
            .enumerate()
            .map(|(i, e)| Preset { index: (i + 1) as u8, name: e.display_name() })
            .collect()
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.index)
    }

    /// Preset number of what plays (1-based), capped at 99 to fit in a `u8`.
    pub fn preset(&self) -> Option<u8> {
        (self.index < self.entries.len()).then(|| (self.index + 1).min(99) as u8)
    }

    /// Positions on preset `n` (1-based). Returns `false` — **without moving
    /// playback** — when it does not exist: a selection failure must not
    /// interrupt what plays.
    pub fn select(&mut self, n: u8) -> bool {
        if n == 0 || usize::from(n) > self.entries.len() {
            return false;
        }
        self.index = usize::from(n) - 1;
        true
    }

    /// Realigns the index on a track announced by the player. Returns `false`
    /// for an index outside the list — mpv says `-1` at the end of the list,
    /// and the core relays it as is.
    pub fn set_index(&mut self, n: i64) -> bool {
        let Ok(i) = usize::try_from(n) else { return false };
        if i >= self.entries.len() {
            return false;
        }
        self.index = i;
        true
    }

    /// Writes the playlist meant for mpv: **absolute** paths, so that it
    /// depends on no current directory. Atomic write — a power cut must not
    /// leave a truncated m3u that mpv would half read.
    pub fn write_for_mpv(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("m3u.tmp");
        std::fs::write(&tmp, render(&self.entries, None))?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn playlist_of(n: usize) -> Playlist {
        Playlist {
            entries: (1..=n)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/musique/{i:02}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect(),
            index: 0,
        }
    }

    #[test]
    fn the_preset_count_is_capped_at_99() {
        assert_eq!(playlist_of(150).preset_count(), 99);
        assert_eq!(playlist_of(12).preset_count(), 12);
        assert_eq!(Playlist::default().preset_count(), 0);
    }

    #[test]
    fn the_named_presets_follow_the_positions_and_the_same_cap() {
        // The name is the one `preset_name` already publishes for the current
        // track: the grid tiles and the player must say the same thing about
        // the same track.
        let p = playlist_of(3);
        assert_eq!(
            p.presets(),
            vec![
                Preset { index: 1, name: "01".into() },
                Preset { index: 2, name: "02".into() },
                Preset { index: 3, name: "03".into() },
            ]
        );
        // The same cap as `preset_count`, and it must stay so: an announced
        // preset that `Command::Select` cannot reach would make a tile that
        // plays nothing.
        let long = playlist_of(150);
        assert_eq!(long.presets().len(), usize::from(long.preset_count()));
        assert_eq!(long.presets().last().unwrap().index, 99);
        assert!(Playlist::default().presets().is_empty());
    }

    #[test]
    fn selecting_out_of_bounds_fails_without_moving_the_index() {
        let mut p = playlist_of(3);
        p.index = 1;
        assert!(!p.select(0), "zero is not a preset");
        assert!(!p.select(4));
        assert_eq!(p.index, 1, "a failure must not move playback");
        assert!(p.select(3));
        assert_eq!(p.index, 2);
    }

    #[test]
    fn a_negative_or_out_of_list_index_is_set_aside() {
        // mpv says -1 at the end of the list, and the core passes it on as is.
        let mut p = playlist_of(3);
        assert!(!p.set_index(-1));
        assert!(!p.set_index(3));
        assert_eq!(p.index, 0, "the index must not have moved");
        assert!(p.set_index(2));
        assert_eq!(p.index, 2);
    }

    #[test]
    fn the_preset_follows_the_index_and_disappears_on_an_empty_playlist() {
        let mut p = playlist_of(3);
        p.index = 2;
        assert_eq!(p.preset(), Some(3));
        assert_eq!(Playlist::default().preset(), None);
    }

    #[test]
    fn the_mpv_m3u_carries_absolute_paths() {
        // It is written in the state directory and read by another process: a
        // relative path would resolve there against mpv's current directory.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-files.m3u");
        playlist_of(2).write_for_mpv(&f).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("#EXTM3U\n"));
        assert!(text.contains("\n/musique/01.mp3\n"), "{text}");
        assert!(text.contains("\n/musique/02.mp3\n"), "{text}");
        // And nothing lingers from the temporary file.
        assert!(!dir.path().join("plugin-files.m3u.tmp").exists());
    }
}
