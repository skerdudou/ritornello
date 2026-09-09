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

/// **This report is the rollback's own dated marker**, and not only a trace
/// for the page.
///
/// `rollback` consumes the install marker before it restarts the core it has
/// just restored — it must, or a second `OnFailure=` would roll back twice —
/// so the restored core boots with nothing to tell it that this is not an
/// ordinary start. Without a second signal it reads the Startup setting, whose
/// default is *on*, and wakes a device that was in standby: the
/// three-in-the-morning failure the marker exists to prevent, happening on the
/// one path no test ever ran.
///
/// This file is that signal, and it needed no new writing: it is already
/// written immediately after the marker is cleared, already left in place for
/// ever, and already read by the core at boot. Only the reading was missing.
/// Same window as the marker (`marker::within_window`), so the two halves of
/// one event are judged by one rule; and stale for every boot after, so a
/// rollback last March does not silently override the Startup setting in
/// September.
///
/// **One asymmetry with the marker is worth stating, because this file is
/// never consumed and that one is.** `within_window` calls a clock that has
/// gone *backwards* fresh — the safe answer for a device with no
/// battery-backed clock, since NTP corrects minutes after boot and the
/// alternative would wake a device mid-rollback. For the marker that leniency
/// expires with the file; here it does not. So a device whose clock reads
/// before the epoch-ish at boot, and on which a rollback happened at any point
/// in the past, resumes what it was doing instead of consulting Startup — for
/// as long as the clock stays wrong. The outcome is the benign direction (a
/// device that was asleep stays asleep, one that was playing plays), it is the
/// same direction the marker already chose, and the alternative trades it for
/// the failure this whole pair exists to prevent.
pub fn is_fresh(report: &Report, now_unix_s: u64) -> bool {
    crate::marker::within_window(report.at_unix_s, now_unix_s)
}

/// `Ok(None)` when nothing authorises a rollback — no marker, a stale one, or
/// one left by an apply that did not replace the core. That is the ordinary
/// case for a crash loop unrelated to an update, and it is not an error: the
/// unit exits 0 and systemd is left to give up.
///
/// **`core_replaced` is checked here as well as in `marker::arm`**, and the
/// duplication is deliberate rather than belt and braces. This binary is the
/// one thing an update cannot replace — `target.rs` cannot form its path — so
/// a device routinely runs a new core beside an installer from months ago. An
/// older installer still arms this net for a plugin gesture, and the refusal
/// has to live on the reading side to cover that device at all.
pub fn rollback(prefix: &Path, now_unix_s: u64) -> std::io::Result<Option<Report>> {
    let Some(marker) = marker::read(prefix) else { return Ok(None) };
    if !marker::is_fresh(&marker, now_unix_s) {
        return Ok(None);
    }
    if !marker.core_replaced {
        // A plugin runs in a process of its own and cannot crash-loop the
        // core, so this marker attributes nothing: acting on it would undo a
        // plugin gesture for an unrelated failure and leave the real cause
        // alone. The marker is left where it is — it is not this unit's to
        // consume, and the next apply overwrites it.
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

    /// A pass that installed a plugin the device did not have **and** replaced
    /// the core, which is now the only shape in which a plugin's first install
    /// is ever undone: only a core replacement arms this net (`marker::arm`),
    /// and only a core replacement is accepted where it is read.
    #[test]
    fn undoing_a_first_install_deletes_rather_than_restores() {
        let (_d, prefix, staging) = fake_root();
        fs::write(prefix.join("usr/local/bin/ritornello-core"), b"the core that worked").unwrap();
        fs::write(staging.join("staged-mpd"), b"fresh").unwrap();
        fs::write(staging.join("staged-core"), b"the core that does not start").unwrap();
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![
                Action::PlacePlugin {
                    file: "ritornello-plugin-mpd".to_string(),
                    staged: "staged-mpd".to_string(),
                },
                Action::PlaceCore { staged: "staged-core".to_string() },
            ],
        };
        let applied = apply(&prefix, &staging, &req).unwrap();
        crate::marker::arm(&prefix, &applied, 1_000).unwrap();

        rollback(&prefix, 1_005).unwrap().expect("a fresh marker authorises it");
        // There was nothing before, so putting "what was there" back means
        // leaving nothing. Restoring an empty file instead would leave a
        // plugin the core declares and cannot execute.
        assert!(!prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-mpd").exists());
        // And the core it came with went back, which is what authorised any of
        // this in the first place.
        assert_eq!(
            fs::read(prefix.join("usr/local/bin/ritornello-core")).unwrap(),
            b"the core that worked"
        );
    }

    /// **A plugin gesture arms nothing.** `marker::arm` writes no marker, and
    /// — for the device whose installer predates this rule, which is every
    /// device that has not been redeployed by hand — `rollback` refuses one
    /// written anyway.
    ///
    /// Without this, an unrelated core crash loop within ten minutes of a
    /// plugin gesture deleted the binary just installed, or put back the one
    /// just uninstalled, and left the actual cause of the crash untouched.
    #[test]
    fn a_plugin_only_apply_never_authorises_a_rollback() {
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
        let installed = prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-mpd");
        assert!(installed.exists(), "the fixture must really have installed it");

        crate::marker::arm(&prefix, &applied, 1_000).unwrap();
        assert!(crate::marker::read(&prefix).is_none(), "arm writes nothing for a plugin");
        assert!(rollback(&prefix, 1_005).unwrap().is_none(), "and there is nothing to authorise");

        // The reading-side guard, for an installer that predates `arm`.
        crate::marker::write(&prefix, &applied, 1_000).unwrap();
        assert!(
            rollback(&prefix, 1_005).unwrap().is_none(),
            "a marker saying the core was not replaced attributes nothing"
        );
        assert!(installed.exists(), "the operator's plugin survives an unrelated crash loop");
    }

    /// **`arm` clears a marker an earlier core update left behind.** Every
    /// apply rewrites the backup manifest, so a still-fresh marker standing
    /// over a plugin gesture's manifest would have a crash loop undo the plugin
    /// gesture and leave the core exactly where it was — the worst of both
    /// outcomes.
    #[test]
    fn a_plugin_gesture_disarms_the_net_an_earlier_core_update_armed() {
        let (_d, prefix, staging) = fake_root();
        fs::write(prefix.join("usr/local/bin/ritornello-core"), b"old").unwrap();
        fs::write(staging.join("staged-core"), b"new").unwrap();
        let core_apply = apply(
            &prefix,
            &staging,
            &Request {
                format: REQUEST_FORMAT,
                actions: vec![Action::PlaceCore { staged: "staged-core".to_string() }],
            },
        )
        .unwrap();
        crate::marker::arm(&prefix, &core_apply, 1_000).unwrap();
        assert!(crate::marker::read(&prefix).is_some(), "the core update armed it");

        fs::write(staging.join("staged-mpd"), b"fresh").unwrap();
        let plugin_apply = apply(
            &prefix,
            &staging,
            &Request {
                format: REQUEST_FORMAT,
                actions: vec![Action::PlacePlugin {
                    file: "ritornello-plugin-mpd".to_string(),
                    staged: "staged-mpd".to_string(),
                }],
            },
        )
        .unwrap();
        crate::marker::arm(&prefix, &plugin_apply, 1_060).unwrap();

        assert!(
            crate::marker::read(&prefix).is_none(),
            "the manifest now describes the plugin gesture, so the authorisation over it must go"
        );
        assert!(rollback(&prefix, 1_090).unwrap().is_none());
        assert!(
            prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-mpd").exists(),
            "and the plugin is not undone by a crash loop the core update caused"
        );
    }

    /// The report's own window, and the asymmetry `is_fresh`'s doc names: it
    /// expires (unlike the marker, nothing consumes this file), and a clock
    /// that went backwards reads as fresh (like the marker, and for the same
    /// reason — but here that leniency has no expiry, which is the
    /// consequence written down beside it).
    #[test]
    fn the_reports_window_expires_and_a_backwards_clock_still_reads_as_fresh() {
        let r = Report {
            at_unix_s: 1_000,
            restored: vec!["core".into()],
            failed: vec![],
            core_restored: true,
        };
        assert!(is_fresh(&r, 1_000), "the instant of the rollback");
        assert!(is_fresh(&r, 1_000 + crate::marker::MARKER_WINDOW_S), "the last second");
        assert!(
            !is_fresh(&r, 1_000 + crate::marker::MARKER_WINDOW_S + 1),
            "one second later the restored core is an ordinary boot again"
        );
        assert!(!is_fresh(&r, 1_000 + 86_400 * 180), "and a rollback last March is not this boot");
        assert!(is_fresh(&r, 0), "a clock that has not been set yet keeps the device as it was");
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
