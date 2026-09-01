//! The device's mounted volumes: what a wizard may offer to browse, and what
//! it must refuse.
//!
//! Everything is pure and takes the text of `/proc/mounts` rather than reading
//! it: that is what allows proving the browsing guard without mounting
//! anything, which a test could not do without privileges.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// File systems that carry no user files.
///
/// **Blacklist, not whitelist — a decision revised along the way.**
///
/// The first version instead enumerated the accepted file systems. The
/// reasoning was that a blacklist would forget the kernel's next pseudo file
/// system. It was wrong, because it weighed the wrong risk: the asymmetry of
/// consequences goes the other way.
///
/// - An incomplete whitelist makes **a real disk unusable**, with no
///   workaround offered to the user. It happened: `/mnt/c` under WSL is a
///   `9p`, and a USB disk in NTFS mounted by ntfs-3g shows up as `fuseblk` —
///   two types we had not anticipated, two hard blocks.
/// - An incomplete blacklist lets **a spurious entry** through into a choice
///   list. The annoyance is visible, reversible and minor.
///
/// What the blacklist must still ensure holds: `proc` is in it, so the guard
/// still refuses `/proc/self` and its recursive tree.
///
/// `overlay` is **not** in it: on a containerized system, it is the root
/// itself. Excluding it would make everything invisible, which is exactly the
/// error just corrected. Its few spurious entries under WSL are the lesser
/// evil.
const PSEUDO_FS: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "nsfs",
    "proc",
    "pstore",
    "ramfs",
    "rootfs",
    "rpc_pipefs",
    "securityfs",
    "selinuxfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// True if this mount type can carry someone's music.
fn useful_fs(fstype: &str) -> bool {
    !PSEUDO_FS.contains(&fstype)
}

const PROC_MOUNTS: &str = "/proc/mounts";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Volume {
    pub path: PathBuf,
    pub fstype: String,
}

/// Unescapes a `/proc/mounts` field: the space is written `\040` there and
/// the tab `\011`.
fn unescape(s: &str) -> String {
    s.replace("\\040", " ").replace("\\011", "\t")
}

/// Every mount, **pseudo file systems included**.
///
/// The browsing guard needs them whole: it is by knowing the mount of `/proc`
/// that `/proc/self` can be refused.
fn all(proc_mounts: &str) -> Vec<Volume> {
    proc_mounts
        .lines()
        .filter_map(|l| {
            let mut c = l.split_whitespace();
            let _source = c.next()?;
            let point = c.next()?;
            let fstype = c.next()?;
            Some(Volume { path: PathBuf::from(unescape(point)), fstype: fstype.to_string() })
        })
        .collect()
}

/// Volumes that can be offered to the user, sorted.
pub fn volumes(proc_mounts: &str) -> Vec<Volume> {
    let mut kept: Vec<Volume> = Vec::new();
    for v in all(proc_mounts) {
        if !useful_fs(&v.fstype) {
            continue;
        }
        // The same point mounted twice appears only once, and the last mount
        // is the one that counts — as for the kernel.
        match kept.iter_mut().find(|r| r.path == v.path) {
            Some(slot) => *slot = v,
            None => kept.push(v),
        }
    }
    kept.sort_by(|a, b| a.path.cmp(&b.path));
    kept
}

/// The **owning** mount of a path: the longest mount point that prefixes it.
///
/// This is the only correct formulation. A test "the path starts with a
/// volume" would accept `/proc/self/root`, since `/proc` starts with `/`,
/// which is indeed a volume.
///
/// On equal length, `max_by_key` returns the **last** element, which is
/// exactly the semantics of overmounting: the last mounted is the one you see.
pub fn owner(proc_mounts: &str, path: &Path) -> Option<Volume> {
    all(proc_mounts)
        .into_iter()
        .filter(|v| path.starts_with(&v.path))
        .max_by_key(|v| v.path.as_os_str().len())
}

/// True if `path` can be browsed: its owning mount carries a real file system.
pub fn browsable(proc_mounts: &str, path: &Path) -> bool {
    owner(proc_mounts, path)
        .map(|v| useful_fs(&v.fstype))
        .unwrap_or(false)
}

/// Contents of `/proc/mounts`.
///
/// The path can be overridden with `RITORNELLO_FILES_PROC_MOUNTS`: that is
/// what lets the end-to-end journey describe volumes without mounting any, on
/// a machine where the test has no privileges.
pub fn read_proc_mounts() -> String {
    let path =
        std::env::var("RITORNELLO_FILES_PROC_MOUNTS").unwrap_or_else(|_| PROC_MOUNTS.to_string());
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic Raspberry Pi /proc/mounts: the root, the boot partition, a
    /// USB key, and the pseudo file systems that must stay invisible.
    const MOUNTS: &str = "\
proc /proc proc rw,relatime 0 0
sysfs /sys sysfs rw,relatime 0 0
/dev/mmcblk0p2 / ext4 rw,relatime 0 0
devtmpfs /dev devtmpfs rw 0 0
tmpfs /run tmpfs rw,nosuid 0 0
/dev/mmcblk0p1 /boot/firmware vfat rw,relatime 0 0
/dev/sda1 /media/ma\\040cle exfat rw,relatime 0 0
//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0
";

    #[test]
    fn pseudo_file_systems_are_not_offered() {
        // Blacklist, not whitelist: see PSEUDO_FS for the asymmetry of
        // consequences that made this choice get revised.
        let v: Vec<String> = volumes(MOUNTS).iter().map(|v| v.path.display().to_string()).collect();
        assert_eq!(v, vec!["/", "/boot/firmware", "/media/ma cle", "/mnt/ritornello/nas"]);
    }

    #[test]
    fn an_unknown_file_system_stays_offerable() {
        // THE defect the whitelist had caused, now pinned. Three cases met for
        // real:
        //   - `9p`: /mnt/c under WSL, hence the host machine's whole disk;
        //   - `fuseblk`: a USB disk in NTFS mounted by ntfs-3g, the most
        //     ordinary case of a key coming from Windows;
        //   - `virtiofs`: a virtual machine share.
        // None was anticipated, and each made a real disk unreachable without
        // the slightest workaround offered to the user.
        let m = "\
C:\\134 /mnt/c 9p rw,noatime 0 0
/dev/sdb1 /media/usb fuseblk rw,relatime 0 0
partage /mnt/hote virtiofs rw 0 0
";
        let v: Vec<String> = volumes(m).iter().map(|v| v.path.display().to_string()).collect();
        assert_eq!(v, vec!["/media/usb", "/mnt/c", "/mnt/hote"]);
        assert!(browsable(m, Path::new("/mnt/c/projets/musique")));
        assert!(browsable(m, Path::new("/media/usb/Albums")));
    }

    #[test]
    fn a_containerized_overlay_stays_visible() {
        // `overlay` is deliberately absent from the blacklist: on a
        // containerized system, it is the root itself, and excluding it would
        // make everything invisible — exactly the error the whitelist made.
        let m = "overlay / overlay rw 0 0\nproc /proc proc rw 0 0\n";
        assert!(browsable(m, Path::new("/srv/musique")));
        assert!(!browsable(m, Path::new("/proc/self")));
    }

    #[test]
    fn a_mount_point_with_an_escaped_space_is_unescaped() {
        // /proc/mounts escapes the space as \040. Without this handling, the
        // "ma cle" key would be offered under a name the file system does not
        // know, and browsing would fail on opening.
        assert!(volumes(MOUNTS).iter().any(|v| v.path == Path::new("/media/ma cle")));
    }

    #[test]
    fn the_owning_mount_is_the_longest_prefix() {
        // THE rule that makes the guard correct. A naive test "starts with a
        // volume" would accept /proc/self/root, since /proc starts with /,
        // which is a volume.
        let p = owner(MOUNTS, Path::new("/boot/firmware/config.txt")).unwrap();
        assert_eq!(p.path, PathBuf::from("/boot/firmware"));
        let p = owner(MOUNTS, Path::new("/home/pi/musique")).unwrap();
        assert_eq!(p.path, PathBuf::from("/"));
    }

    #[test]
    fn pseudo_file_systems_are_not_browsable() {
        // Not for secrecy — they are readable anyway — but because an "add
        // all" launched on /proc would wander into the recursive links of
        // /proc/self.
        assert!(!browsable(MOUNTS, Path::new("/proc/self")));
        assert!(!browsable(MOUNTS, Path::new("/sys/class")));
        assert!(!browsable(MOUNTS, Path::new("/run/user/1000")));
        assert!(!browsable(MOUNTS, Path::new("/dev/shm")));
    }

    #[test]
    fn a_path_under_a_real_volume_is_browsable() {
        assert!(browsable(MOUNTS, Path::new("/media/ma cle/Albums")));
        assert!(browsable(MOUNTS, Path::new("/home/pi/musique")));
        assert!(browsable(MOUNTS, Path::new("/")));
    }

    #[test]
    fn an_overmount_is_the_one_that_counts() {
        // Two mounts at the same place: the last one is the visible one, as
        // for the kernel. Getting this wrong would declare browsable a path
        // that the tmpfs has covered.
        let m = "/dev/sda1 /media/x ext4 rw 0 0\ntmpfs /media/x tmpfs rw 0 0\n";
        assert_eq!(owner(m, Path::new("/media/x/a")).unwrap().fstype, "tmpfs");
        assert!(!browsable(m, Path::new("/media/x/a")));
    }

    #[test]
    fn a_truncated_line_is_ignored_without_panicking() {
        // /proc/mounts is read live: a partial line must not bring down the
        // whole page.
        assert!(volumes("/dev/sda1\n\n/dev/sdb1 /media/y\n").is_empty());
    }
}
