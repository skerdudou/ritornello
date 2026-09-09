//! One dated file per restarting event — and there are **two** events, which
//! is a correction of what this module used to claim.
//!
//! While it is fresh, this marker says both:
//!
//! 1. **restart without changing what the player was doing.** An install at
//!    3 a.m. must not wake the active source and start playing, which is what
//!    the startup power setting would otherwise do;
//! 2. **a crash loop right now is attributable to this core replacement**, so
//!    the rollback unit may act. Three months later, an unrelated crash loop
//!    finds a stale marker and nothing happens.
//!
//! **The second meaning is consumed and the first is not, so they cannot both
//! live in one file.** `rollback::rollback` deletes this marker before it
//! restarts the core it has just put back — deliberately, because an
//! authorisation that survived would let a second `OnFailure=` roll back
//! twice. This module used to claim the opposite ("no race between the core
//! that crashes and the rollback that repairs — the reverted core finds the
//! instruction even if the broken one had already read it"), and that sentence
//! was false: the reverted core found nothing, read the Startup setting
//! instead, and woke a device that had been asleep — the exact
//! three-in-the-morning failure this whole mechanism exists to prevent.
//!
//! So the instruction the **restored** core needs is carried by the other
//! dated file, the one the rollback writes and never deletes:
//! `rollback::Report`, whose `at_unix_s` is the moment of the rollback.
//! `is_fresh` here and `rollback::is_fresh` share one window through
//! `within_window`, and the core's `startup_override` reads both. One file per
//! restarting event, each read by the process that event started.
//!
//! **Armed only by a core replacement.** `arm` writes this file when the apply
//! replaced the core and *clears* it otherwise. A plugin runs in a process of
//! its own and cannot crash-loop the core, so a plugin gesture must never
//! point the rollback net at its own backup manifest: any unrelated core
//! start-limit failure inside the window would otherwise delete a
//! just-installed plugin binary, or put back a just-uninstalled one, and leave
//! the real cause untouched.

use crate::apply::{write_atomic, Applied};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Ten minutes. Long enough for systemd to exhaust its start limit and for a
/// slow SD card to boot; short enough that a power cut hours later behaves
/// exactly as it does today.
pub const MARKER_WINDOW_S: u64 = 600;

/// **The writer of this file is older than its reader, always.**
///
/// This struct is written by `ritornello-update`, which an update deliberately
/// cannot replace — `target.rs` cannot form its path, so it only ever changes
/// by hand — and it is read by the core, which updates itself. So a device
/// routinely runs a new core against a marker written by an installer from
/// months ago.
///
/// The consequence is a rule, and it binds whoever adds the first field here:
/// **every field added to this struct must carry `#[serde(default)]`.**
/// Without it, `serde_json::from_str` fails on a marker the old installer
/// wrote, `marker::read` answers `None` — its documented behaviour for a
/// corrupt file — and the two things this marker exists for both quietly stop
/// happening: the restart after an install wakes the player at 3 a.m., and the
/// rollback declines to act. Nothing would fail loudly.
///
/// `REQUEST_FORMAT` guards the other direction only (a new core writing a
/// request an old installer must not misread); there is no such number here,
/// on purpose — a marker that refuses to be read is worse than one read
/// partially.
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

/// Is a dated file written at `at_unix_s` still describing the restart this
/// boot is part of?
///
/// Shared with `rollback::is_fresh` rather than copied: the two files answer
/// the same question for the two halves of the same event — an install's
/// restart and the rollback's — and a window that drifted between them would
/// make one of the two halves wake the device.
///
/// A clock that went backwards reads as fresh: see the module documentation.
pub fn within_window(at_unix_s: u64, now_unix_s: u64) -> bool {
    now_unix_s < at_unix_s || now_unix_s - at_unix_s <= MARKER_WINDOW_S
}

/// Pure, and therefore tested. See the module documentation for why a clock
/// that went backwards reads as fresh.
pub fn is_fresh(marker: &Marker, now_unix_s: u64) -> bool {
    within_window(marker.at_unix_s, now_unix_s)
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

/// Arms the core's rollback net for what this apply did — **or disarms it**.
///
/// The whole of the "a plugin gesture must not arm the core's rollback"
/// decision, in one branch, in the library rather than in the binary, so that
/// a test can drive it.
///
/// The `clear` in the first arm is not tidiness. The marker authorises rolling
/// back **the backup manifest as it now stands**, and every apply rewrites
/// that manifest from scratch. A still-fresh marker from an earlier core
/// replacement, left standing over a plugin gesture's manifest, would mean an
/// unrelated crash loop undoing the plugin gesture and *not* putting the core
/// back — the worst of both. The cost is named and accepted: a core update
/// that started successfully and is followed by a plugin gesture inside the
/// same ten minutes loses its rollback net. A core that got far enough to
/// serve that gesture has already contradicted the attribution the window
/// stands for.
pub fn arm(prefix: &Path, applied: &Applied, now_unix_s: u64) -> std::io::Result<()> {
    if !applied.core_replaced {
        return clear(prefix);
    }
    write(prefix, applied, now_unix_s)
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
