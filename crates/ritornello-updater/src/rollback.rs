//! Putting back what was there, and saying so.
//!
//! Reached only by `OnFailure=` on `ritornello.service`, which systemd fires
//! when the unit has exhausted its start limit. **It receives no polkit grant
//! and must never be given one**: the core has no business asking for a
//! rollback, and a second unit name in `52-ritornello-update.rules` would hand
//! the web UI the right to trigger one.

use crate::apply::{backup_dir, write_atomic, BackedUp, BACKUP_MANIFEST};
use crate::marker;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// **Written by a binary that is never updated, read by one that is** — the
/// same asymmetry as `marker::Marker`, and the same rule follows from it:
/// every field added here must carry `#[serde(default)]`.
///
/// The core deserializes this file at startup and puts it straight into
/// `UpdateState.last_rollback`, so its field names are also the wire contract
/// of `GET /api/update`. A field added without a default makes an old
/// installer's report unreadable, and the page then shows nothing at all where
/// a nocturnal rollback happened — the one trace it leaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub at_unix_s: u64,
    /// Keys put back, or deleted when there was nothing to put back.
    pub restored: Vec<String>,
    /// Keys that could not be restored, with the reason. A partial rollback is
    /// still worth reporting: it tells the operator exactly what to fix by
    /// hand.
    pub failed: Vec<String>,
    /// Was the core binary among them?
    pub core_restored: bool,
}

/// `<prefix>/var/lib/ritornello-update/last-rollback.json`.
pub fn report_path(prefix: &Path) -> PathBuf {
    prefix.join("var/lib/ritornello-update/last-rollback.json")
}

/// `Ok(None)` when nothing authorises a rollback — no marker, or a stale one.
/// That is the ordinary case for a crash loop unrelated to an update, and it
/// is not an error: the unit exits 0 and systemd is left to give up.
pub fn rollback(prefix: &Path, now_unix_s: u64) -> std::io::Result<Option<Report>> {
    let Some(marker) = marker::read(prefix) else { return Ok(None) };
    if !marker::is_fresh(&marker, now_unix_s) {
        return Ok(None);
    }

    let mut report = Report {
        at_unix_s: now_unix_s,
        restored: Vec::new(),
        failed: Vec::new(),
        core_restored: false,
    };

    let manifest_path = backup_dir(prefix).join(BACKUP_MANIFEST);
    let entries: Vec<BackedUp> = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(entries) => entries,
            Err(e) => {
                // A manifest truncated by a power cut must not read as
                // "nothing to restore": the binaries have already been
                // replaced by `apply`, so treating this as an empty manifest
                // would be a silent no-op rollback — the exact failure this
                // whole safety net exists to prevent.
                report.failed.push(format!("backup manifest at {}: {e}", manifest_path.display()));
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            report.failed.push(format!("backup manifest at {}: {e}", manifest_path.display()));
            Vec::new()
        }
    };

    for entry in entries {
        let outcome = if entry.existed {
            let kept = backup_dir(prefix).join(&entry.key);
            copy_back(&kept, &entry.target)
        } else {
            // "What was there" was nothing. Restoring an empty file instead
            // would leave a plugin the core declares and cannot execute.
            match std::fs::remove_file(&entry.target) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        };
        match outcome {
            Ok(()) => {
                if entry.key == "core" {
                    report.core_restored = true;
                }
                report.restored.push(entry.key);
            }
            Err(e) => report.failed.push(format!("{}: {e}", entry.key)),
        }
    }

    // Consumed BEFORE the report is written: if writing the report fails, the
    // one thing that must not survive is the authorisation to do this again.
    marker::clear(prefix)?;
    let path = report_path(prefix);
    write_atomic(&path, serde_json::to_string(&report).expect("a Report serializes").as_bytes())?;
    Ok(Some(report))
}

/// Same copy-then-rename as `apply::place`, and for the same reasons.
fn copy_back(kept: &Path, target: &Path) -> std::io::Result<()> {
    let dir = target.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.back",
        target.file_name().and_then(|n| n.to_str()).unwrap_or("target")
    ));
    std::fs::copy(kept, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Err(e) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::{apply, backup_dir};
    use crate::request::{Action, Request, REQUEST_FORMAT};
    use std::fs;

    fn fake_root() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let prefix = dir.path().join("root");
        let staging = dir.path().join("staging");
        fs::create_dir_all(prefix.join("usr/local/bin")).unwrap();
        fs::create_dir_all(prefix.join("usr/local/lib/ritornello/plugins")).unwrap();
        fs::create_dir_all(&staging).unwrap();
        (dir, prefix, staging)
    }

    #[test]
    fn a_replaced_core_is_put_back_and_the_marker_is_consumed() {
        let (_d, prefix, staging) = fake_root();
        let core = prefix.join("usr/local/bin/ritornello-core");
        fs::write(&core, b"the core that worked").unwrap();
        fs::write(staging.join("staged-core"), b"the core that does not start").unwrap();

        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::PlaceCore { staged: "staged-core".to_string() }],
        };
        let applied = apply(&prefix, &staging, &req).unwrap();
        crate::marker::write(&prefix, &applied, 1_000).unwrap();
        assert_eq!(fs::read(&core).unwrap(), b"the core that does not start");

        let report = rollback(&prefix, 1_010).unwrap().expect("a fresh marker authorises it");
        assert_eq!(fs::read(&core).unwrap(), b"the core that worked");
        assert_eq!(report.restored, vec!["core".to_string()]);
        // Consumed, so a second OnFailure does nothing and systemd is allowed
        // to give up instead of looping between two broken states.
        assert!(crate::marker::read(&prefix).is_none());
    }

    #[test]
    fn undoing_a_first_install_deletes_rather_than_restores() {
        let (_d, prefix, staging) = fake_root();
        fs::write(staging.join("staged-mpd"), b"fresh").unwrap();
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::PlacePlugin {
                file: "ritornello-plugin-mpd".to_string(),
                staged: "staged-mpd".to_string(),
            }],
        };
        let applied = apply(&prefix, &staging, &req).unwrap();
        crate::marker::write(&prefix, &applied, 1_000).unwrap();

        rollback(&prefix, 1_005).unwrap().expect("a fresh marker authorises it");
        // There was nothing before, so putting "what was there" back means
        // leaving nothing. Restoring an empty file instead would leave a
        // plugin the core declares and cannot execute.
        assert!(!prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-mpd").exists());
    }

    #[test]
    fn a_stale_marker_authorises_nothing() {
        let (_d, prefix, staging) = fake_root();
        let core = prefix.join("usr/local/bin/ritornello-core");
        fs::write(&core, b"old").unwrap();
        fs::write(staging.join("staged-core"), b"new").unwrap();
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::PlaceCore { staged: "staged-core".to_string() }],
        };
        let applied = apply(&prefix, &staging, &req).unwrap();
        crate::marker::write(&prefix, &applied, 1_000).unwrap();

        let outcome = rollback(&prefix, 1_000 + crate::marker::MARKER_WINDOW_S + 1).unwrap();
        assert!(outcome.is_none(), "an unrelated crash loop must not undo an old update");
        assert_eq!(fs::read(&core).unwrap(), b"new");
    }

    #[test]
    fn no_marker_at_all_authorises_nothing() {
        let (_d, prefix, _staging) = fake_root();
        assert!(rollback(&prefix, 42).unwrap().is_none());
    }

    #[test]
    fn the_report_is_left_where_the_core_can_read_it() {
        let (_d, prefix, staging) = fake_root();
        let core = prefix.join("usr/local/bin/ritornello-core");
        fs::write(&core, b"old").unwrap();
        fs::write(staging.join("staged-core"), b"new").unwrap();
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::PlaceCore { staged: "staged-core".to_string() }],
        };
        let applied = apply(&prefix, &staging, &req).unwrap();
        crate::marker::write(&prefix, &applied, 1_000).unwrap();
        rollback(&prefix, 1_001).unwrap().unwrap();

        // Without this file the only trace of a 3 a.m. rollback would be a
        // version number that did not move.
        let text = fs::read_to_string(report_path(&prefix)).expect("a report is written");
        assert!(text.contains("\"restored\":[\"core\"]"), "{text}");
    }

    #[test]
    fn a_corrupt_manifest_is_reported_rather_than_read_as_empty() {
        let (_d, prefix, staging) = fake_root();
        let core = prefix.join("usr/local/bin/ritornello-core");
        fs::write(&core, b"old").unwrap();
        fs::write(staging.join("staged-core"), b"new").unwrap();
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::PlaceCore { staged: "staged-core".to_string() }],
        };
        let applied = apply(&prefix, &staging, &req).unwrap();
        crate::marker::write(&prefix, &applied, 1_000).unwrap();

        // A power cut mid-write, simulated directly: the manifest on disk is
        // not JSON at all.
        fs::write(backup_dir(&prefix).join(crate::apply::BACKUP_MANIFEST), b"{not json").unwrap();

        let report = rollback(&prefix, 1_001).unwrap().expect("a fresh marker authorises it");
        assert!(report.restored.is_empty(), "nothing could be read back, so nothing was restored");
        assert!(
            report.failed.iter().any(|f| f.contains("manifest")),
            "the corrupt manifest must be named in `failed`: {:?}",
            report.failed
        );
        // The binaries were already replaced by `apply`; a corrupt manifest
        // must not be mistaken, on the page, for a rollback that succeeded.
        assert_eq!(fs::read(&core).unwrap(), b"new");
    }
}
