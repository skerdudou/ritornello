//! Dialogue with systemd to mount and unmount the declared shares.
//!
//! The plugin mounts nothing itself: the service runs under
//! `NoNewPrivileges=true`, so `sudo` and every setuid path are structurally
//! out of reach. It asks systemd to run a fixed unit,
//! `ritornello-media-mount.service`, which a polkit rule lets it start — and
//! that one only (`deploy/51-ritornello-media.rules`).
//!
//! This module knows only two things: telling whether a root is mounted (by
//! reading `/proc/mounts`), and asking for reconciliation. What is actually
//! mounted, and with which options, is decided on the privileged side.

use crate::roots::{Root, RootKind};
use ritornello_files_mount::mounts::{is_mounted_in, read_proc_mounts};
use std::path::PathBuf;

/// The unit the plugin starts. Fixed: it is also the name the polkit rule
/// compares against; a parameterizable unit would be an open authorization.
pub const UNIT: &str = "ritornello-media-mount.service";

/// What the plugin knows about a root's availability.
///
/// Deliberately binary: nothing here distinguishes "not mounted yet" from
/// "mount failed", because the plugin cannot know without asking systemd, and
/// the course of action is the same — reconcile, then report what `systemctl`
/// answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountState {
    Mounted,
    NotMounted,
}

/// Mount point of a root, **imposed**: `/mnt/ritornello/<name>`.
///
/// A plain relay to `Root::mount_point`, not a second implementation: both
/// used to exist side by side, whereas rolling back a failed declaration now
/// relies on their equality — a divergence would silently undeclare a healthy
/// share.
///
/// This is not `Root::base_dir()`, which appends the declared subpath. The
/// subpath is browsed *under* the mounted point and never appears in
/// `/proc/mounts`: confusing it with the mount point would make a root with a
/// subpath look forever unmounted.
fn mount_point(root: &Root) -> PathBuf {
    root.mount_point()
}

/// Mount state of a root, as the kernel reports it.
///
/// A local root always returns `Mounted`: there is nothing to mount, and
/// returning `NotMounted` would trigger an endless reconciliation for a
/// directory the mount binary ignores anyway.
///
/// An unreadable `/proc/mounts` returns `NotMounted`: not knowing means not
/// being able to promise the share is there.
pub fn state(root: &Root) -> MountState {
    if root.kind == RootKind::Local {
        return MountState::Mounted;
    }
    // Through `volumes::read_proc_mounts` and not a hard-coded
    // `read_to_string`: it is the only reader of this table, it honors
    // `RITORNELLO_FILES_PROC_MOUNTS`, and that is what makes the rollback of a
    // failed declaration verifiable without mounting anything. An unreadable
    // table returns the empty string, hence `NotMounted`: not knowing means
    // not being able to promise the share is there.
    if is_mounted_in(&read_proc_mounts(), &mount_point(root)) {
        MountState::Mounted
    } else {
        MountState::NotMounted
    }
}

/// Formats a `systemctl` failure, standard error **verbatim**.
///
/// Verbatim because a polkit refusal is explicit and actionable there
/// ("Interactive authentication required", which points at the missing rule),
/// whereas a home-made sentence would make it opaque. The fallback on the exit
/// code only serves the case where `systemctl` fails without writing anything:
/// an empty error would be shown as a silent success.
fn failure(status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let err = String::from_utf8_lossy(stderr).trim().to_string();
    if err.is_empty() {
        format!("systemctl failed ({status})")
    } else {
        err
    }
}

/// Asks systemd to reconcile the mounts.
///
/// `systemctl` as a child process, not a D-Bus crate: that is how the System
/// tab talks to systemd and logind (`crates/ritornello-core/src/system.rs`),
/// and it avoids pulling in a whole dependency for one call.
///
/// **No capability probe beforehand.** The System tab can ask logind whether
/// it is allowed (`CanPowerOff`); systemd offers no equivalent for
/// `manage-units` — there is no "CanStartUnit". So we try, and the error
/// carries `systemctl`'s output as is, all the way to the page.
pub async fn reconcile(unit: &str) -> Result<(), String> {
    tracing::info!("asking systemd to start {unit}");
    let output = tokio::process::Command::new("systemctl")
        .arg("start")
        .arg(unit)
        .output()
        .await
        .map_err(|e| format!("systemctl unavailable: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(failure(output.status, &output.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One real line: a cifs share mounted by the root binary. The parsing of the
    /// table itself is tested where it lives, in `ritornello-files-mount`.
    const PROC_MOUNTS_SAMPLE: &str = "//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0\n";

    fn smb_root() -> Root {
        Root {
            name: "nas".into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: "musique".into(),
            subpath: None,
            user: "steven".into(),
            domain: String::new(),
            writable: false,
            archive_covers: false,
        }
    }

    #[test]
    fn a_local_root_has_nothing_to_mount() {
        // Without this case, a folder of the device would be declared
        // unmounted and the plugin would demand a reconciliation the root
        // binary ignores: a perpetual privilege request for nothing.
        let r = Root {
            name: "usb".into(),
            kind: RootKind::Local,
            path: Some("/media/usb".into()),
            ..smb_root()
        };
        assert_eq!(state(&r), MountState::Mounted);
    }

    #[test]
    fn the_subpath_does_not_enter_the_mount_point() {
        // `base_dir()` appends the browsed subpath; /proc/mounts only knows
        // the mounted point. Confusing them would make every root with a
        // subpath look unmounted, hence remounted endlessly.
        let r = Root { subpath: Some("Albums".into()), ..smb_root() };
        assert_eq!(mount_point(&r), PathBuf::from("/mnt/ritornello/nas"));
        assert!(is_mounted_in(PROC_MOUNTS_SAMPLE, &mount_point(&r)));
    }

    #[test]
    fn a_systemctl_failure_reports_its_output_word_for_word() {
        // The reason behind the "no home-made message" choice: polkit's
        // sentence is the one that says what to do. Rewording it would lose
        // the information.
        let refusal = b"Failed to start ritornello-media-mount.service: Interactive authentication required.";
        let output = std::process::Command::new("false").output().unwrap();
        assert_eq!(
            failure(output.status, refusal),
            "Failed to start ritornello-media-mount.service: Interactive authentication required."
        );
    }

    #[test]
    fn a_silent_failure_remains_a_non_empty_message() {
        // A `systemctl` that fails without writing anything would otherwise
        // yield an empty error, which the page would show as a silent success.
        let output = std::process::Command::new("false").output().unwrap();
        let m = failure(output.status, b"   \n");
        assert!(m.contains("systemctl failed"), "{m:?}");
    }
}
