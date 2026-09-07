//! Placing a file, and keeping aside what it replaces.
//!
//! Two properties are worth reading the code for.
//!
//! **The whole request is validated before the first write.** A half-applied
//! update is a state nothing knows how to describe: the core would say one
//! thing, the disk another, and the rollback unit would have to guess. So
//! every name is resolved first, and only then does anything move.
//!
//! **What is kept aside is what was there**, not "the previous release". When
//! only one plugin was replaced, that is exactly what rollback must put back.

use crate::request::{Request, Action, REQUEST_FORMAT};
use crate::target::{staged_of, target_of, TargetError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Name of the file describing what was kept aside, inside the backup
/// directory.
pub const BACKUP_MANIFEST: &str = "backup.json";

/// What went wrong. Every variant names the offending input: this runs from a
/// systemd unit, so the journal is the only place an operator will look.
#[derive(Debug)]
pub enum ApplyError {
    UnknownFormat(u32),
    Target(TargetError),
    /// The staged path is not a regular file — a symlink, a directory, or
    /// absent. Checked with `symlink_metadata`, which does **not** follow.
    NotARegularFile(PathBuf),
    Io(PathBuf, std::io::Error),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFormat(v) => write!(f, "request format {v} is not {REQUEST_FORMAT}: this installer is older or newer than the core that wrote it"),
            Self::Target(e) => write!(f, "{e}"),
            Self::NotARegularFile(p) => write!(f, "refusing {}: not a regular file", p.display()),
            Self::Io(p, e) => write!(f, "on {}: {e}", p.display()),
        }
    }
}

impl std::error::Error for ApplyError {}

impl From<TargetError> for ApplyError {
    fn from(e: TargetError) -> Self {
        Self::Target(e)
    }
}

/// One line of the backup manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackedUp {
    /// Key under which the old bytes were kept: `core`, or
    /// `plugin-<file>`.
    pub key: String,
    /// Path to restore it to.
    pub target: PathBuf,
    /// Was there anything there at all? `false` means rollback must **delete**
    /// the target rather than restore it — a first install being undone.
    pub existed: bool,
}

/// What `apply` did, reported to the journal and to the marker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub placed: Vec<String>,
    pub removed: Vec<String>,
    pub core_replaced: bool,
}

/// `<prefix>/var/lib/ritornello-update/rollback`.
///
/// Root-owned on the device, readable by the service: the core must be able to
/// see that a rollback is possible, and must not be able to fabricate one.
pub fn backup_dir(prefix: &Path) -> PathBuf {
    prefix.join("var/lib/ritornello-update/rollback")
}

fn io<T>(path: &Path, r: std::io::Result<T>) -> Result<T, ApplyError> {
    r.map_err(|e| ApplyError::Io(path.to_path_buf(), e))
}

pub fn apply(prefix: &Path, staging: &Path, request: &Request) -> Result<Applied, ApplyError> {
    if request.format != REQUEST_FORMAT {
        return Err(ApplyError::UnknownFormat(request.format));
    }
    // Pass one: resolve everything. Nothing has moved yet.
    let mut resolved: Vec<(&Action, PathBuf, Option<PathBuf>)> = Vec::new();
    for action in &request.actions {
        let target = target_of(prefix, action)?;
        let staged = staged_of(staging, action)?;
        if let Some(ref s) = staged {
            let meta = io(s, std::fs::symlink_metadata(s))?;
            if !meta.is_file() {
                return Err(ApplyError::NotARegularFile(s.clone()));
            }
        }
        resolved.push((action, target, staged));
    }

    // Pass two: keep aside, then move.
    let backups = backup_dir(prefix);
    io(&backups, std::fs::create_dir_all(&backups))?;
    let manifest_path = backups.join(BACKUP_MANIFEST);
    // A manifest from an earlier run must never be readable as this run's: if
    // this pass fails partway through, rollback must find either nothing, or
    // a manifest that names exactly the backups made so far — never a stale
    // one naming a different, no-longer-matching set of backups.
    match std::fs::remove_file(&manifest_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ApplyError::Io(manifest_path, e)),
    }
    let mut manifest: Vec<BackedUp> = Vec::new();
    let mut applied = Applied::default();

    for (action, target, staged) in resolved {
        let key = match action {
            Action::PlaceCore { .. } => "core".to_string(),
            Action::PlacePlugin { file, .. } | Action::RemovePlugin { file } => format!("plugin-{file}"),
        };
        let existed = target.exists();
        if existed {
            let kept = backups.join(&key);
            io(&kept, std::fs::copy(&target, &kept).map(|_| ()))?;
        }
        manifest.push(BackedUp { key, target: target.clone(), existed });

        // Rewritten now, before the target itself is touched: if placing or
        // removing fails right below, the manifest on disk already names
        // every backup made so far, and none that isn't there yet. Rollback
        // reading it mid-failure restores bytes that are still correct.
        let text = serde_json::to_string(&manifest).expect("a Vec<BackedUp> serializes");
        io(&manifest_path, write_atomic(&manifest_path, text.as_bytes()))?;

        match (action, staged) {
            (Action::PlaceCore { .. }, Some(staged)) => {
                place(&staged, &target)?;
                applied.core_replaced = true;
                applied.placed.push("core".to_string());
            }
            (Action::PlacePlugin { file, .. }, Some(staged)) => {
                place(&staged, &target)?;
                applied.placed.push(file.clone());
            }
            (Action::RemovePlugin { file }, _) => {
                if existed {
                    io(&target, std::fs::remove_file(&target))?;
                }
                applied.removed.push(file.clone());
            }
            // `staged_of` returns `Some` for both place variants and `None`
            // only for `RemovePlugin`, so this arm is unreachable. Written as
            // an error rather than a panic: this runs as root.
            (_, None) => return Err(ApplyError::NotARegularFile(target.clone())),
        }
    }

    Ok(applied)
}

/// Copy then rename, both inside the target's own directory.
///
/// The temporary lives beside the target and not in `/tmp`: a rename across
/// filesystems is not atomic, and `/usr/local` and `/tmp` are routinely
/// different mounts — `PrivateTmp=true` in the service unit guarantees it for
/// the core's own view.
///
/// Renaming onto a binary that is currently executing is legal on Linux: the
/// old inode stays alive for the processes that already opened it. That is
/// what lets the core replace a running plugin, and itself.
fn place(staged: &Path, target: &Path) -> Result<(), ApplyError> {
    let dir = target.parent().unwrap_or(Path::new("/"));
    io(dir, std::fs::create_dir_all(dir))?;
    let tmp = dir.join(format!(
        ".{}.new",
        target.file_name().and_then(|n| n.to_str()).unwrap_or("target")
    ));
    io(&tmp, std::fs::copy(staged, &tmp).map(|_| ()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        io(&tmp, std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)))?;
    }
    if let Err(e) = std::fs::rename(&tmp, target) {
        // Best-effort cleanup, and the rename error is the one worth
        // reporting: a failed cleanup must not mask it.
        let _ = std::fs::remove_file(&tmp);
        return Err(ApplyError::Io(target.to_path_buf(), e));
    }
    Ok(())
}

/// Writes through a temporary beside the target, then `rename`.
///
/// The same idiom, and the same reason, as `plugins::write_atomic` in the
/// core: this is a device one unplugs, and every file this crate writes is
/// read by something that must not be misled by half of it. A truncated
/// backup manifest would make a rollback restore nothing while the binaries
/// are already replaced; a truncated marker would disarm the rollback and the
/// state-preserving restart together.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // The rename error is the one worth reporting; a cleanup that fails
        // in turn must not mask it.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{Action, Request, REQUEST_FORMAT};
    use std::fs;

    /// Builds a fake root: the two target directories, plus a staging
    /// directory, and returns (prefix, staging).
    fn fake_root() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let prefix = dir.path().join("root");
        let staging = dir.path().join("staging");
        fs::create_dir_all(prefix.join("usr/local/bin")).unwrap();
        fs::create_dir_all(prefix.join("usr/local/lib/ritornello/plugins")).unwrap();
        fs::create_dir_all(&staging).unwrap();
        (dir, prefix, staging)
    }

    fn stage(staging: &std::path::Path, name: &str, content: &[u8]) {
        fs::write(staging.join(name), content).unwrap();
    }

    #[test]
    fn a_plugin_binary_is_placed_and_the_previous_one_is_kept_aside() {
        let (_d, prefix, staging) = fake_root();
        let target = prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-radio");
        fs::write(&target, b"old binary").unwrap();
        stage(&staging, "staged-radio", b"new binary");

        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::PlacePlugin {
                file: "ritornello-plugin-radio".to_string(),
                staged: "staged-radio".to_string(),
            }],
        };
        let applied = apply(&prefix, &staging, &req).expect("apply succeeds");

        assert_eq!(fs::read(&target).unwrap(), b"new binary");
        assert_eq!(applied.placed, vec!["ritornello-plugin-radio".to_string()]);
        // What was there is recoverable, which is the whole point of the
        // rollback unit existing at all.
        let kept = backup_dir(&prefix).join("plugin-ritornello-plugin-radio");
        assert_eq!(fs::read(kept).unwrap(), b"old binary");
    }

    #[test]
    fn a_first_install_records_that_there_was_nothing_to_keep() {
        let (_d, prefix, staging) = fake_root();
        stage(&staging, "staged-mpd", b"fresh binary");
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::PlacePlugin {
                file: "ritornello-plugin-mpd".to_string(),
                staged: "staged-mpd".to_string(),
            }],
        };
        apply(&prefix, &staging, &req).expect("apply succeeds");

        // The distinction rollback needs: "put the old one back" and "there
        // was no old one, so delete it" are different gestures, and a missing
        // backup file cannot tell them apart on its own.
        let manifest = fs::read_to_string(backup_dir(&prefix).join(BACKUP_MANIFEST)).unwrap();
        assert!(manifest.contains("\"existed\":false"), "{manifest}");
    }

    #[test]
    fn the_placed_binary_is_executable() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let (_d, prefix, staging) = fake_root();
            stage(&staging, "staged-cd", b"binary");
            let req = Request {
                format: REQUEST_FORMAT,
                actions: vec![Action::PlacePlugin {
                    file: "ritornello-plugin-cd".to_string(),
                    staged: "staged-cd".to_string(),
                }],
            };
            apply(&prefix, &staging, &req).unwrap();
            let target = prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-cd");
            let mode = fs::metadata(&target).unwrap().permissions().mode();
            // A plugin the core cannot execute is a plugin that ships and
            // never starts — the failure this repository has already recorded
            // making three times, in its other forms.
            assert_eq!(mode & 0o777, 0o755, "mode was {:o}", mode & 0o777);
        }
    }

    #[test]
    fn a_staged_symlink_is_refused_and_nothing_is_written() {
        #[cfg(unix)]
        {
            let (_d, prefix, staging) = fake_root();
            let secret = prefix.join("usr/local/bin/ritornello-core");
            fs::write(&secret, b"the running core").unwrap();
            std::os::unix::fs::symlink(&secret, staging.join("staged-evil")).unwrap();

            let req = Request {
                format: REQUEST_FORMAT,
                actions: vec![Action::PlacePlugin {
                    file: "ritornello-plugin-radio".to_string(),
                    staged: "staged-evil".to_string(),
                }],
            };
            let err = apply(&prefix, &staging, &req).expect_err("a symlink is refused");
            assert!(matches!(err, ApplyError::NotARegularFile(_)), "{err:?}");
            assert!(!prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-radio").exists());
        }
    }

    #[test]
    fn a_wrong_format_is_refused_before_anything_is_touched() {
        let (_d, prefix, staging) = fake_root();
        stage(&staging, "staged-radio", b"new");
        let req = Request {
            format: REQUEST_FORMAT + 1,
            actions: vec![Action::PlacePlugin {
                file: "ritornello-plugin-radio".to_string(),
                staged: "staged-radio".to_string(),
            }],
        };
        let err = apply(&prefix, &staging, &req).expect_err("an unknown format is refused");
        assert!(matches!(err, ApplyError::UnknownFormat(_)), "{err:?}");
        assert!(!prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-radio").exists());
    }

    #[test]
    fn a_hostile_name_is_refused_before_anything_is_touched() {
        let (_d, prefix, staging) = fake_root();
        stage(&staging, "staged-radio", b"new");
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![
                Action::PlacePlugin {
                    file: "ritornello-plugin-radio".to_string(),
                    staged: "staged-radio".to_string(),
                },
                Action::RemovePlugin { file: "../../bin/ritornello-core".to_string() },
            ],
        };
        let err = apply(&prefix, &staging, &req).expect_err("a hostile name is refused");
        assert!(matches!(err, ApplyError::Target(_)), "{err:?}");
        // The refusal is decided on the WHOLE request before the first write:
        // a half-applied update is the state nothing knows how to describe.
        assert!(!prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-radio").exists());
    }

    #[test]
    fn removing_a_plugin_keeps_it_aside_first() {
        let (_d, prefix, _staging) = fake_root();
        let target = prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-mpd");
        fs::write(&target, b"doomed").unwrap();
        let staging = prefix.join("unused");
        std::fs::create_dir_all(&staging).unwrap();
        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![Action::RemovePlugin { file: "ritornello-plugin-mpd".to_string() }],
        };
        let applied = apply(&prefix, &staging, &req).unwrap();
        assert!(!target.exists());
        assert_eq!(applied.removed, vec!["ritornello-plugin-mpd".to_string()]);
        assert_eq!(fs::read(backup_dir(&prefix).join("plugin-ritornello-plugin-mpd")).unwrap(), b"doomed");
    }

    #[test]
    fn a_failure_on_a_later_action_still_leaves_the_earlier_one_recorded() {
        let (_d, prefix, staging) = fake_root();
        let radio_target = prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-radio");
        fs::write(&radio_target, b"old binary").unwrap();
        stage(&staging, "staged-radio", b"new binary");

        // The second action's target is a directory, not a file:
        // `target.exists()` is true, so `apply` tries to back it up, and
        // `fs::copy` fails reading a directory as a source — a genuine I/O
        // error with no reliance on permissions or on running as root.
        let cd_target = prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-cd");
        fs::create_dir_all(&cd_target).unwrap();
        stage(&staging, "staged-cd", b"new binary");

        let req = Request {
            format: REQUEST_FORMAT,
            actions: vec![
                Action::PlacePlugin {
                    file: "ritornello-plugin-radio".to_string(),
                    staged: "staged-radio".to_string(),
                },
                Action::PlacePlugin {
                    file: "ritornello-plugin-cd".to_string(),
                    staged: "staged-cd".to_string(),
                },
            ],
        };
        let err = apply(&prefix, &staging, &req);
        assert!(err.is_err(), "backing up a directory must fail");

        // What the first action already did must still be described on disk:
        // a manifest that goes missing here is a backup rollback can no
        // longer find, exactly the half-applied state this module exists to
        // rule out.
        let manifest = fs::read_to_string(backup_dir(&prefix).join(BACKUP_MANIFEST)).unwrap();
        assert!(manifest.contains("\"key\":\"plugin-ritornello-plugin-radio\""), "{manifest}");
        assert!(manifest.contains("\"existed\":true"), "{manifest}");
        // And nothing about the action that never got backed up.
        assert!(!manifest.contains("ritornello-plugin-cd"), "{manifest}");
    }

    #[test]
    fn write_atomic_leaves_no_tmp_file_behind_on_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("thing.json");
        write_atomic(&path, b"hello").expect("write succeeds");

        assert_eq!(fs::read(&path).unwrap(), b"hello");
        // The cheap way to prove the rename actually happened rather than a
        // plain copy: nothing named after the temporary survives it.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
