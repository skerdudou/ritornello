//! One marker file, two meanings — and that is a design decision, not a
//! shortcut.
//!
//! While it is fresh, it says both:
//!
//! 1. **restart without changing what the player was doing.** An install at
//!    3 a.m. must not wake the active source and start playing, which is what
//!    the startup power setting would otherwise do;
//! 2. **a crash loop right now is attributable to this install**, so the
//!    rollback unit may act. Three months later, an unrelated crash loop finds
//!    a stale marker and nothing happens.
//!
//! Deriving both from one dated file is what removes every synchronisation
//! problem the alternative had: no file for the core to delete, no ownership
//! to hand over, and **no race between the core that crashes and the rollback
//! that repairs** — the reverted core finds the instruction even if the broken
//! one had already read it.

use crate::apply::{write_atomic, Applied};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Ten minutes. Long enough for systemd to exhaust its start limit and for a
/// slow SD card to boot; short enough that a power cut hours later behaves
/// exactly as it does today.
pub const MARKER_WINDOW_S: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub at_unix_s: u64,
    /// Was the core binary itself replaced? The page says something different
    /// in each case, and so does the log.
    pub core_replaced: bool,
    pub placed: Vec<String>,
    pub removed: Vec<String>,
}

/// `<prefix>/var/lib/ritornello-update/pending.json`.
pub fn marker_path(prefix: &Path) -> PathBuf {
    prefix.join("var/lib/ritornello-update/pending.json")
}

/// Pure, and therefore tested. See the module documentation for why a clock
/// that went backwards reads as fresh.
pub fn is_fresh(marker: &Marker, now_unix_s: u64) -> bool {
    now_unix_s < marker.at_unix_s || now_unix_s - marker.at_unix_s <= MARKER_WINDOW_S
}

/// Written through `apply::write_atomic`: a marker truncated by a power cut
/// would disarm the state-preserving restart and the rollback at once, which
/// is the exact failure this file exists to prevent.
pub fn write(prefix: &Path, applied: &Applied, now_unix_s: u64) -> std::io::Result<()> {
    let marker = Marker {
        at_unix_s: now_unix_s,
        core_replaced: applied.core_replaced,
        placed: applied.placed.clone(),
        removed: applied.removed.clone(),
    };
    let path = marker_path(prefix);
    write_atomic(&path, serde_json::to_string(&marker).expect("a Marker serializes").as_bytes())
}

/// `None` for an absent, unreadable, or **corrupt** marker.
///
/// A truncated marker reads as absent rather than as an error, and that is a
/// deliberate choice, not an oversight left from before `write_atomic`
/// existed: the only two callers are "should this restart preserve the player
/// state" and "may I roll back", and both have a correct answer without it —
/// today's behaviour, and no. `write` writes atomically, so a corrupt file on
/// disk means an earlier write never completed at all, not that it completed
/// with the wrong bytes.
pub fn read(prefix: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(marker_path(prefix)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn clear(prefix: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(marker_path(prefix)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_ten_minutes_and_the_bound_is_inclusive() {
        let m = Marker { at_unix_s: 1_000, core_replaced: true, placed: vec![], removed: vec![] };
        assert!(is_fresh(&m, 1_000), "the instant of the install is fresh");
        assert!(is_fresh(&m, 1_000 + MARKER_WINDOW_S), "the last second is still fresh");
        assert!(!is_fresh(&m, 1_000 + MARKER_WINDOW_S + 1), "one second later it is stale");
        assert!(!is_fresh(&m, 1_000 + 86_400), "a day later it is stale");
    }

    /// A device one unplugs has no battery-backed clock, and NTP corrects it
    /// minutes after boot. So "now" can legitimately be BEFORE the install.
    ///
    /// Fresh is the safe answer, and it is safe in both of the marker's two
    /// uses: a state-preserving restart is never harmful, and the rollback
    /// only ever runs when the service is already failing to start.
    #[test]
    fn a_clock_that_went_backwards_reads_as_fresh() {
        let m = Marker { at_unix_s: 10_000, core_replaced: true, placed: vec![], removed: vec![] };
        assert!(is_fresh(&m, 9_000));
        assert!(is_fresh(&m, 0));
    }
}
