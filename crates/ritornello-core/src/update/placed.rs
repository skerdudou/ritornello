//! What the updater put on this device, remembered across the restart.
//!
//! **The one thing the core had no memory of.** Everything else in this module
//! compares what *runs* against what a release *offers*, and that comparison
//! cannot tell "never tried" from "tried and reverted": the rollback unit only
//! ever handles paths, so `rollback::Report` names files and carries no
//! version at all. Two defects followed from the gap, and they are the same
//! defect seen twice:
//!
//! - a core release that does not start is installed, fails, is put back — and
//!   is installed again the next night, and every night after, until a newer
//!   release appears;
//! - a plugin whose new binary dies before announcing keeps an unknown
//!   installed version for ever, so the guard that (rightly) skips components
//!   with unknown versions excludes it from every automatic run, including the
//!   release that would repair it.
//!
//! What closes both is one line per component: **the version of the archive
//! that was placed**, which is not the version the component then announced.
//! When those two agree, the component is up to date and never reaches the
//! automatic list at all; when they disagree, the placement did not take, and
//! the offered version being the one already placed is exactly "tried, and it
//! did not work".
//!
//! **Beside the staging area, not in `state.json`**, and that is a decision
//! rather than an accident — see the module's own note in `update/mod.rs` and
//! `record` below. The short of it: `Core::persist` rewrites the whole of
//! `state.json` from its in-memory copy on every volume change, so a second
//! writer there loses its field at the next unrelated write. This file has one
//! writer, the worker, and the worker is serial by construction.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What one install pass put on the device for one component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedComponent {
    /// The version of the **archive that was placed**, and never the version
    /// the component afterwards announced. That distinction is the whole
    /// point: a binary that dies before announcing has no version of its own,
    /// and this is the only record that it was ever tried.
    pub version: String,
    /// The core's entry only: what its archive carried that `install_one`
    /// never places (the privileged installer, the systemd units, the polkit
    /// rules — see `archive::core_not_installed`).
    ///
    /// It lives here rather than in a file of its own so that the note and the
    /// version it describes are **one fact**: two files would be two records
    /// of "the core version the updater placed", free to disagree. And it is
    /// what lets the note be shown only while it still describes the core that
    /// is actually running — see `main::apply_core_archive_note`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_installed_files: Option<Vec<String>>,
}

/// Component name to what was placed for it. At most one entry per component:
/// the previous placement of a component says nothing useful once a newer one
/// has happened, and an unbounded history on an SD card with no janitor is a
/// file that only grows.
pub type Placed = BTreeMap<String, PlacedComponent>;

/// `<staging>/placed.json`, beside the core's archive note it replaces.
///
/// The staging directory is the unprivileged service's own — it creates it
/// before any download — which is what makes this file writable at the moment
/// it has to be written: immediately after the privileged unit has placed the
/// bytes and, for the core, before the process leaves.
pub fn path(staging: &Path) -> PathBuf {
    staging.join("placed.json")
}

/// What is remembered, or nothing.
///
/// An absent, unreadable or corrupt file answers an **empty** memory, the same
/// convention as `read_rollback_report` and for the same reason: a memory that
/// cannot be parsed says less than no memory at all. It fails **open** — the
/// component is attempted once more — which is the safe direction here: the
/// worst case is the behaviour this file exists to stop, bounded by a rollback
/// that already works, whereas failing closed would refuse an update on a
/// corrupt file and leave no way back except by hand.
pub fn read(staging: &Path) -> Placed {
    let path = path(staging);
    let Ok(text) = std::fs::read_to_string(&path) else { return Placed::new() };
    match serde_json::from_str(&text) {
        Ok(placed) => placed,
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", path.display());
            Placed::new()
        }
    }
}

/// Writes down that `component` was just placed at `version`.
///
/// **Read-modify-write, and that is sound here rather than lucky**: the worker
/// is the only writer of this file and it runs one job at a time for the life
/// of the process (`run_worker`), so there is no second writer to lose an
/// entry to. The same could not be said of `state.json`, whose every write
/// rewrites the whole document from `Core`'s in-memory copy.
///
/// Through a temporary and a `rename`, like every other file this product
/// writes: this is a device one unplugs.
pub fn record(
    staging: &Path,
    component: &str,
    version: &str,
    not_installed_files: Option<Vec<String>>,
) -> std::io::Result<()> {
    let mut placed = read(staging);
    placed.insert(
        component.to_string(),
        PlacedComponent { version: version.to_string(), not_installed_files },
    );
    let text = serde_json::to_string(&placed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::create_dir_all(staging)?;
    super::write_atomic(&path(staging), text.as_bytes())
}

/// The version last placed for this component, if any.
pub fn version_of<'a>(placed: &'a Placed, component: &str) -> Option<&'a str> {
    placed.get(component).map(|p| p.version.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The round trip that has to survive a restart: the process that writes
    /// this is not the process that reads it back.
    #[test]
    fn what_was_placed_is_read_back_by_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        record(&staging, "core", "0.3.0", Some(vec!["etc/systemd/system/ritornello.service".into()]))
            .unwrap();
        record(&staging, "mpd", "0.4.1", None).unwrap();

        let placed = read(&staging);
        assert_eq!(version_of(&placed, "core"), Some("0.3.0"));
        assert_eq!(version_of(&placed, "mpd"), Some("0.4.1"));
        assert_eq!(
            placed["core"].not_installed_files.as_deref(),
            Some(["etc/systemd/system/ritornello.service".to_string()].as_slice()),
            "the core's note travels with the version it describes"
        );
        assert_eq!(placed["mpd"].not_installed_files, None, "a plugin's archive has no such note");
        assert_eq!(version_of(&placed, "radio"), None, "a component nothing ever placed");
    }

    /// A second placement of the same component replaces the first: the
    /// previous one says nothing useful, and keeping it would grow a file
    /// nothing ever trims.
    #[test]
    fn a_later_placement_of_the_same_component_replaces_the_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        record(&staging, "core", "0.3.0", Some(vec!["a".into()])).unwrap();
        record(&staging, "core", "0.4.0", Some(vec!["b".into()])).unwrap();
        let placed = read(&staging);
        assert_eq!(placed.len(), 1);
        assert_eq!(version_of(&placed, "core"), Some("0.4.0"));
        assert_eq!(
            placed["core"].not_installed_files.as_deref(),
            Some(["b".to_string()].as_slice()),
            "the note is replaced along with the version, never merged with the old one"
        );
    }

    /// Absent, and corrupt, both answer an empty memory — and the second is
    /// not the first: a file that exists and does not parse is what a power
    /// cut mid-write leaves, and reading it as an error would refuse every
    /// automatic update until someone deleted it by hand.
    #[test]
    fn an_absent_or_corrupt_memory_remembers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        assert!(read(&staging).is_empty(), "no file at all");

        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(path(&staging), b"{\"core\": {\"vers").unwrap();
        assert!(read(&staging).is_empty(), "a file truncated by a power cut");
    }
}
