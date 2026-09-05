//! Persisted state: what to do on arriving at the source, and the track last
//! listened to on the last disc.
//!
//! Same pattern as `crates/ritornello-plugin-files/src/state.rs`, including the
//! `update` that preserves the fields it does not touch — and here that is not
//! a precaution but a requirement: the Admin half writes `on_arrival`, the
//! Source half writes `remembered`, into this same file. A `save` rebuilt by
//! either would erase the other's field.
//!
//! One file for a setting **and** a playback position, which may look like a
//! mix of two natures. It is the same choice as the files plugin, whose file
//! holds both the playlist (configured) and the current index (state), and for
//! the same reason: they are written by the same two halves of the same
//! process, and splitting them would buy nothing but a second path to keep in
//! sync.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// What the plugin does when the source is arrived at — the source key
/// (`Activate`) and the boot or standby exit (`Wake`) alike.
///
/// **The same value governs both**, deliberately. They used to differ without
/// anyone deciding it: pressing the key started track 1, while a boot started
/// nothing, because `wake` was overridden and `activate` was not. One of the
/// two was going to surprise its user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnArrival {
    /// Play nothing; only refresh what the display shows. The default, and
    /// the owner's decision: starting a drive is a physical act — it spins
    /// up, it is audible — and it must not happen unasked.
    #[default]
    Nothing,
    /// Start the disc from its first track.
    FirstTrack,
    /// Resume the track last listened to, **on that same disc** (see
    /// `Remembered`).
    LastTrack,
}

/// The track last listened to, and the disc it belongs to.
///
/// The TOC is what makes this usable: a track number alone, applied to
/// whatever disc happens to be in the drive, would drop the listener into the
/// middle of an unrelated record — or outside its track count. The plugin
/// already reads that TOC and already uses it to tell a disc swap from a
/// flicker of the tray, so this costs nothing new.
///
/// Only **one** disc is remembered, the last one. Not a history: swapping
/// discs and coming back loses the position, which is the honest reading of
/// "the last track listened to".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    /// Raw TOC (`cd-discid` output), exactly as it goes into the identity.
    pub toc: String,
    /// **Zero-based**, like `CdSource::track` — never the 1-based number the
    /// display and the remote use. The conversion belongs to whoever builds a
    /// `cdda://` URI, in one place.
    pub track: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub on_arrival: OnArrival,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remembered: Option<Remembered>,
}

/// A missing or unreadable file yields the defaults, **without panicking**: a
/// first installation, or an erased `/var/lib`, must let the plugin start and
/// not refuse to run. The default being "play nothing", a lost file costs a
/// setting, never an unexpected start.
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

/// Re-reads, modifies, rewrites — and therefore preserves what the caller does
/// not touch. The only write the two halves are allowed to use.
pub fn update(path: &Path, f: impl FnOnce(&mut State)) -> anyhow::Result<()> {
    let mut state = load(path);
    f(&mut state);
    save(path, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_unreadable_state_gives_the_defaults_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("absent.json")), State::default());
        let damaged = dir.path().join("damaged.json");
        std::fs::write(&damaged, b"{ this is not json").unwrap();
        assert_eq!(load(&damaged), State::default());
    }

    #[test]
    fn the_default_plays_nothing() {
        // The owner's decision, and the reason this enum has a `Default`: a
        // fresh install, or a wiped `/var/lib`, must not make the drive spin
        // up on its own.
        assert_eq!(State::default().on_arrival, OnArrival::Nothing);
        assert_eq!(load(std::path::Path::new("/nonexistent")).on_arrival, OnArrival::Nothing);
    }

    #[test]
    fn the_setting_and_the_remembered_disc_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-cd.json");
        let state = State {
            on_arrival: OnArrival::LastTrack,
            remembered: Some(Remembered { toc: "abcd1234".into(), track: 4 }),
        };
        save(&f, &state).unwrap();
        assert_eq!(load(&f), state);
    }

    #[test]
    fn update_does_not_lose_the_fields_it_does_not_touch() {
        // The heart of the matter: the Admin half writes `on_arrival` and the
        // Source half writes `remembered`, into this same file. Either one
        // rebuilding the whole state would erase the other's work — the
        // setting would revert on the next track change, or the resume point
        // would vanish the next time the page is saved.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-cd.json");
        update(&f, |s| s.on_arrival = OnArrival::LastTrack).unwrap();
        update(&f, |s| s.remembered = Some(Remembered { toc: "deadbeef".into(), track: 2 })).unwrap();
        let reread = load(&f);
        assert_eq!(reread.on_arrival, OnArrival::LastTrack, "the setting survived a track change");
        assert_eq!(reread.remembered.unwrap().track, 2);
        update(&f, |s| s.on_arrival = OnArrival::FirstTrack).unwrap();
        assert_eq!(load(&f).remembered.unwrap().toc, "deadbeef", "the resume point survived a save");
    }

    #[test]
    fn the_setting_is_stored_under_a_readable_name() {
        // The file is read by a human when something looks wrong on the
        // device: `last_track` says what it does, a bare `2` would not.
        let json = serde_json::to_string(&State {
            on_arrival: OnArrival::LastTrack,
            remembered: None,
        })
        .unwrap();
        assert!(json.contains("\"last_track\""), "{json}");
        // And what nobody set is absent rather than null: the file stays
        // legible, and an added field will not have to explain a `null`.
        assert!(!json.contains("remembered"), "{json}");
    }
}
