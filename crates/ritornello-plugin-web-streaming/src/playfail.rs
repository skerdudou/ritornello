//! The last playback that failed, as the wrapper recorded it.
//!
//! A playback is resolved by mpv's ytdl hook running the wrapper, out of the
//! plugin's sight: when it fails, mpv goes idle, the core turns that into a
//! Stop, and nothing else would say why. So the wrapper, on a failed run it
//! serves for mpv (not one of the plugin's own, which say so through
//! [`crate::ytdlp::OWN_RUN_ENV`]), writes [`FILE`] in the data directory: when,
//! the host of the address, a code, and yt-dlp's last line. The page shows it
//! in the yt-dlp card, and the source says it once on the Stop that follows
//! (`source.rs`).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The file, in the plugin's data directory, rewritten whole (temporary file
/// then rename) by each failed run.
pub const FILE: &str = "last-playback-failure.json";

/// The codes the wrapper writes (`tool/wrapper.rs`, `failure_code`).
pub const GUARD_REFUSED: &str = "guard_refused";
pub const LIVE_REFUSED: &str = "live_refused";
pub const NO_FORMATS: &str = "no_formats";
pub const YTDLP_FAILED: &str = "ytdlp_failed";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PlaybackFailure {
    /// Seconds since the epoch, as Python's `time.time()` gives them.
    pub time: f64,
    /// The host of the address that failed; empty when it had none.
    #[serde(default)]
    pub host: String,
    pub code: String,
    /// yt-dlp's last line, its own words: shown as raw detail.
    #[serde(default)]
    pub detail: String,
}

impl PlaybackFailure {
    /// The last failure recorded; `None` when there is none, or it cannot be
    /// read (nothing to say rather than a guess).
    pub fn load(data_dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(data_dir.join(FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The catalogue key saying this failure, one per code; a code this
    /// version does not know (a wrapper written by a newer plugin) reads as
    /// yt-dlp's own failure.
    pub fn key(&self) -> &'static str {
        match self.code.as_str() {
            GUARD_REFUSED => "play_failed_guard_refused",
            LIVE_REFUSED => "play_failed_live_refused",
            NO_FORMATS => "play_failed_no_formats",
            _ => "play_failed_ytdlp_failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: &str) -> PlaybackFailure {
        PlaybackFailure { time: 1.0, host: "h".into(), code: code.into(), detail: String::new() }
    }

    #[test]
    fn each_code_has_its_key_in_the_catalogue_and_an_unknown_one_reads_as_yt_dlp_s() {
        let en = ritornello_i18n::try_parse(crate::WEB_STREAMING_EN).unwrap();
        for (code, key) in [
            (GUARD_REFUSED, "play_failed_guard_refused"),
            (LIVE_REFUSED, "play_failed_live_refused"),
            (NO_FORMATS, "play_failed_no_formats"),
            (YTDLP_FAILED, "play_failed_ytdlp_failed"),
            ("from_a_newer_plugin", "play_failed_ytdlp_failed"),
        ] {
            assert_eq!(failure(code).key(), key);
            assert!(en.contains_key(key), "{key} is not in the English catalogue");
        }
    }

    #[test]
    fn a_record_round_trips_and_garbage_is_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(PlaybackFailure::load(dir.path()), None);
        std::fs::write(
            dir.path().join(FILE),
            r#"{"time": 1730000000.5, "host": "www.youtube.com", "code": "ytdlp_failed", "detail": "ERROR: gone"}"#,
        )
        .unwrap();
        let f = PlaybackFailure::load(dir.path()).unwrap();
        assert_eq!((f.time, f.host.as_str(), f.detail.as_str()), (1730000000.5, "www.youtube.com", "ERROR: gone"));
        std::fs::write(dir.path().join(FILE), "nope").unwrap();
        assert_eq!(PlaybackFailure::load(dir.path()), None);
    }
}
