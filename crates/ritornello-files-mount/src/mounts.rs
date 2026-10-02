//! The kernel's table of mounts, read and queried.
//!
//! Shared by the root helper (which enumerates what is mounted, to unmount what
//! is no longer declared) and the plugin (which asks whether a root is there),
//! so that the unescaping rule has a single implementation.

use std::path::{Path, PathBuf};

const PROC_MOUNTS: &str = "/proc/mounts";

/// Contents of `/proc/mounts`.
///
/// The path can be overridden with `RITORNELLO_FILES_PROC_MOUNTS`: that is
/// what lets the end-to-end journey describe volumes without mounting any, on
/// a machine where the test has no privileges. An unreadable table returns the
/// empty string.
pub fn read_proc_mounts() -> String {
    let path =
        std::env::var("RITORNELLO_FILES_PROC_MOUNTS").unwrap_or_else(|_| PROC_MOUNTS.to_string());
    std::fs::read_to_string(path).unwrap_or_default()
}

/// True if `point` appears as a mount point in the contents of
/// `/proc/mounts`.
///
/// Pure — it takes the text rather than reading it — so as to be testable
/// without mounting anything, which a test cannot do without privileges anyway.
///
/// The second column escapes spaces as `\040` (and tabs as `\011`): without
/// that handling, a share mounted under a name containing a space would look
/// unmounted, and the plugin would remount it in a loop.
pub fn is_mounted_in(proc_mounts: &str, point: &Path) -> bool {
    mount_points(proc_mounts).any(|p| p == point)
}

/// Every declared mount point, unescaped.
///
/// Exists so that the unescaping rule has **only one implementation**: the
/// root mount binary must also enumerate what is mounted, to unmount what is
/// no longer declared. Two copies of this rule were a divergence waiting to
/// happen — one handling `\011` and not the other, say, with a defect visible
/// only on a rare name.
pub fn mount_points(proc_mounts: &str) -> impl Iterator<Item = PathBuf> + '_ {
    proc_mounts.lines().filter_map(|line| {
        line
            .split_whitespace()
            .nth(1)
            .map(|p| PathBuf::from(p.replace("\\040", " ").replace("\\011", "\t")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two real lines: a cifs share mounted by the root binary, and a foreign
    /// mount that must have no effect on the answer.
    const PROC_MOUNTS_SAMPLE: &str =
        "//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0\n\
         /dev/sda1 /media/usb ext4 rw 0 0\n";

    #[test]
    fn a_mount_point_absent_from_proc_mounts_is_not_mounted() {
        // Parsing /proc/mounts is pure: the test needs to mount nothing at
        // all, which it could not do without privileges anyway.
        assert!(is_mounted_in(PROC_MOUNTS_SAMPLE, Path::new("/mnt/ritornello/nas")));
        assert!(!is_mounted_in(PROC_MOUNTS_SAMPLE, Path::new("/mnt/ritornello/autre")));
    }

    #[test]
    fn a_mount_point_with_an_escaped_space_is_recognized() {
        // /proc/mounts escapes spaces as \040. Without that handling, a share
        // "ma musique" would look unmounted, and the plugin would remount it
        // at every glance — a silent mount loop.
        let contents = "//nas/x /mnt/ritornello/ma\\040musique cifs ro 0 0\n";
        assert!(is_mounted_in(contents, Path::new("/mnt/ritornello/ma musique")));
    }

    #[test]
    fn an_escaped_tab_is_recognized_too() {
        // Same mechanism, other escape: \011 is the tab. Handling it halfway
        // would leave the same defect on a rarer name.
        let contents = "//nas/x /mnt/ritornello/ma\\011musique cifs ro 0 0\n";
        assert!(is_mounted_in(contents, Path::new("/mnt/ritornello/ma\tmusique")));
    }

    #[test]
    fn the_source_device_is_not_confused_with_the_mount_point() {
        // The first column is the source, the second the mount point. Looking
        // in any column would make a root look mounted when only its name
        // appears elsewhere in the line.
        let contents = "/mnt/ritornello/nas /mnt/autre none bind 0 0\n";
        assert!(!is_mounted_in(contents, Path::new("/mnt/ritornello/nas")));
    }
}
