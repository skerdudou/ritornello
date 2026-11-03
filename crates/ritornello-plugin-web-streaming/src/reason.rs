//! The reasons a job or a video gives for not going through, as the page
//! shows them.
//!
//! The download chain writes a handful of messages itself (below), and passes
//! on, untouched, what yt-dlp or the system said. The page translates what
//! the plugin wrote and shows the rest as raw detail behind a translated
//! sentence, so the two must be told apart: the messages the plugin writes
//! are the constants here, which the producers use, and [`coded`] recognises
//! exactly them (and the texts of [`TargetError`]). Anything else is an
//! outside voice and travels as `detail`.

use crate::targets::TargetError;
use serde::Serialize;

pub const UNAVAILABLE: &str = "unavailable";
pub const INVALID_ID: &str = "invalid id";
pub const NOT_INSTALLED: &str = "yt-dlp is not installed";
pub const TOOL_TIMEOUT: &str = "yt-dlp did not answer in time";
pub const TIMED_OUT: &str = "timed out";
pub const INTERRUPTED: &str = "interrupted by a restart";
pub const JOB_LOST: &str = "job lost";
pub const NO_SUCH_PRESET: &str = "no such preset";
pub const NO_PLAYLIST: &str = "the preset has no playlist";
pub const NO_VIDEO: &str = "the preset has no video";
pub const BAD_ADDRESS: &str = "the preset's address is unreadable";
pub const LIVE_STREAM: &str = "a live stream is not downloaded";
/// What yt-dlp's failure is written with when it printed nothing: this, then
/// the system's own account of how it ended ("exit status: 3").
pub const EXIT_PREFIX: &str = "yt-dlp stopped: ";
/// yt-dlp failed and its last line was empty.
pub const SILENT: &str = "yt-dlp stopped without saying why";
/// What a `-J` answer that cannot be read (not JSON, a field missing, too
/// large) is written with: this, then what is wrong with it.
pub const UNREADABLE_PREFIX: &str = "yt-dlp's answer cannot be read: ";
/// The wrapper's own refusal of a local address (`tool/wrapper.rs`,
/// `_refused`), which yt-dlp then reports inside its own line: the address
/// that follows is the detail.
pub const LOCAL_REFUSED: &str = "local network refused: ";
/// What a failure of the share itself (not of yt-dlp) is written with: the
/// reason stays a string, as persisted in `jobs.json` (history written before
/// this still loads, its share errors reading as plain failures), and the
/// prefix is what `coded` recognises.
const IO_PREFIX: &str = "share I/O: ";

/// A target error as a reason: the named ones by their own text, an I/O
/// error of the share with its raw system text behind the prefix.
pub fn target_error(e: &TargetError) -> String {
    match e {
        TargetError::Io(detail) => format!("{IO_PREFIX}{detail}"),
        other => other.to_string(),
    }
}

/// A reason as the page receives it: a code it translates, and the raw text
/// of an outside voice when there is one.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Coded {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn coded(message: &str) -> Coded {
    let known = |code| Coded { code, detail: None };
    match message {
        UNAVAILABLE => return known("unavailable"),
        INVALID_ID => return known("invalid_id"),
        NOT_INSTALLED => return known("tool_missing"),
        TOOL_TIMEOUT => return known("timeout"),
        TIMED_OUT => return known("timed_out"),
        INTERRUPTED => return known("interrupted"),
        JOB_LOST => return known("job_lost"),
        NO_SUCH_PRESET => return known("no_such_preset"),
        NO_PLAYLIST => return known("preset_no_playlist"),
        NO_VIDEO => return known("preset_no_video"),
        BAD_ADDRESS => return known("bad_entry"),
        LIVE_STREAM => return known("live_stream"),
        SILENT => return known("failed_silent"),
        _ => {}
    }
    if let Some(detail) = message.strip_prefix(EXIT_PREFIX) {
        return Coded { code: "exited", detail: Some(detail.to_string()) };
    }
    if let Some(detail) = message.strip_prefix(IO_PREFIX) {
        return Coded { code: "io", detail: Some(detail.to_string()) };
    }
    if let Some(detail) = message.strip_prefix(UNREADABLE_PREFIX) {
        return Coded { code: "unreadable_output", detail: Some(detail.to_string()) };
    }
    if let Some((_, address)) = message.split_once(LOCAL_REFUSED) {
        return Coded { code: "local_network", detail: Some(address.trim().to_string()) };
    }
    for (error, code) in [
        (TargetError::UnknownRoot, "unknown_root"),
        (TargetError::NotWritable, "not_writable"),
        (TargetError::NotMounted, "not_mounted"),
        (TargetError::Escapes, "escapes"),
        (TargetError::Unresponsive, "unresponsive"),
    ] {
        if message == error.to_string() {
            return known(code);
        }
    }
    Coded { code: "failed", detail: Some(message.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_plugin_writes_is_a_code_and_what_others_say_is_detail() {
        assert_eq!(coded(NOT_INSTALLED).code, "tool_missing");
        assert_eq!(coded(&TargetError::NotMounted.to_string()).code, "not_mounted");
        let raw = coded("ERROR: Video unavailable");
        assert_eq!((raw.code, raw.detail.as_deref()), ("failed", Some("ERROR: Video unavailable")));
        // An OS error is an outside voice too, whatever it looks like.
        assert_eq!(coded("No such file or directory (os error 2)").code, "failed");
    }

    #[test]
    fn a_share_io_error_keeps_its_variant_and_its_system_text() {
        let message = target_error(&TargetError::Io("Permission denied (os error 13)".into()));
        let c = coded(&message);
        assert_eq!((c.code, c.detail.as_deref()), ("io", Some("Permission denied (os error 13)")));
        // The named ones keep their own text, and so their own codes.
        assert_eq!(coded(&target_error(&TargetError::NotWritable)).code, "not_writable");
    }

    #[test]
    fn a_tool_that_ended_without_a_line_says_how_it_ended() {
        let c = coded(&format!("{EXIT_PREFIX}exit status: 3"));
        assert_eq!((c.code, c.detail.as_deref()), ("exited", Some("exit status: 3")));
    }

    #[test]
    fn an_unreadable_answer_and_the_guards_refusal_have_their_codes() {
        let c = coded(&format!("{UNREADABLE_PREFIX}no id"));
        assert_eq!((c.code, c.detail.as_deref()), ("unreadable_output", Some("no id")));
        // As yt-dlp reports the wrapper's refusal: inside its own line.
        let c = coded("ERROR: [generic] Unable to download webpage: [Errno 13] local network refused: 192.168.1.1");
        assert_eq!((c.code, c.detail.as_deref()), ("local_network", Some("192.168.1.1")));
    }

    #[test]
    fn a_silent_tool_has_its_own_code() {
        assert_eq!(coded(SILENT).code, "failed_silent");
    }

    #[test]
    fn every_constant_has_a_code() {
        for m in [
            UNAVAILABLE, INVALID_ID, NOT_INSTALLED, TOOL_TIMEOUT, TIMED_OUT, INTERRUPTED, JOB_LOST, NO_SUCH_PRESET,
            NO_PLAYLIST, NO_VIDEO, BAD_ADDRESS, LIVE_STREAM, SILENT,
        ] {
            assert_ne!(coded(m).code, "failed", "{m}");
            assert!(coded(m).detail.is_none());
        }
    }
}
