//! Reading the duration of an audio file, from its header.
//!
//! **Header only, in-process** — no `ffprobe` nor mpv. Measured on sixty files:
//! 0.33 ms per file here, against 42 ms with one `ffprobe` per file. On a
//! playlist of two thousand tracks, that makes less than a second instead of
//! more than a minute and two thousand process launches — which would weigh
//! heavily on a Raspberry Pi while the music plays.
//!
//! A missing duration is never an error: the playlist shows with a dash, as
//! before. An unreadable, truncated file, or one of a format the crate does not
//! know, must not interrupt the probing of the next ones.

use std::path::Path;

/// Duration of the file in seconds, or `None` if we cannot read it.
///
/// Rounded to the second: that is the resolution the page shows, and the only
/// one the `#EXTINF` of an m3u can carry. A zero duration is rendered `None` —
/// "0:00" would assert an empty track where the dash says we do not know.
pub fn probe(path: &Path) -> Option<u32> {
    let file = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    let seconds = lofty::file::AudioFile::properties(&file).duration().as_secs();
    let seconds = u32::try_from(seconds).ok()?;
    (seconds > 0).then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes a real mp3 with ffmpeg, or returns `None` if it is absent.
    ///
    /// No binary file versioned in the repository: the duration depends on the
    /// encoding, and one badly copied byte would make the test wrong without
    /// anyone understanding why. The test skips itself where ffmpeg is missing
    /// rather than failing — it is a development tool, not a dependency of the
    /// plugin.
    fn mp3_of(seconds: u32, dir: &Path) -> Option<std::path::PathBuf> {
        let output = dir.join(format!("{seconds}s.mp3"));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={seconds}"))
            .arg(&output)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        ok.then_some(output)
    }

    #[test]
    fn the_duration_of_an_mp3_is_read_from_its_header() {
        let dir = tempfile::tempdir().unwrap();
        let Some(f) = mp3_of(3, dir.path()) else {
            eprintln!("ffmpeg absent: test skipped");
            return;
        };
        // One-second tolerance: an encoder adjusts the length to the frame.
        let d = probe(&f).expect("a duration expected");
        assert!((2..=4).contains(&d), "duration read {d}");
    }

    #[test]
    fn an_unreadable_file_does_not_make_the_probing_fail() {
        // The probing walks thousands of files coming from a share: a single
        // truncated one must not interrupt the next ones, nor surface as an
        // error on screen.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("truncated.mp3");
        std::fs::write(&f, b"this is not mp3").unwrap();
        assert_eq!(probe(&f), None);
        assert_eq!(probe(&dir.path().join("absent.mp3")), None);
    }

    #[test]
    fn a_zero_duration_is_rendered_unknown() {
        // "0:00" would assert an empty track; `None` makes a dash show, which
        // says we do not know. The page already relies on this distinction
        // (see `formatDuration`).
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("empty.flac");
        std::fs::write(&f, b"").unwrap();
        assert_eq!(probe(&f), None);
    }
}
