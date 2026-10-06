//! **Root** binary: reconciles the declared mounts.
//!
//! Launched by `ritornello-media-mount.service`, itself started by the plugin
//! via `systemctl` and authorized by polkit on this single unit.
//!
//! It consumes a configuration written by an **unprivileged** process. It
//! therefore revalidates everything it reads: the validation done on the
//! plugin side does not count as a guarantee, it is only a courtesy to the
//! user.

use anyhow::{Context, Result};
use ritornello_files_mount::mount_options::mount_command;
use ritornello_files_mount::mounts::{is_mounted_in, mount_points};
use ritornello_files_mount::roots::{RootKind, Roots, MOUNT_ROOT};
use std::path::{Path, PathBuf};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// The files plugin's data directory. Fixed rather than read from the
/// environment: this binary is started by systemd, as root, with no
/// environment of ours, and a path it trusts should not come from one anyway.
/// The same directory the core hands the plugin (`plugins::data_dir_for`).
const FILES_DATA_DIR: &str = "/var/lib/ritornello/plugins/files";

/// Where this helper reads the declared roots and the stored credentials,
/// relative to the plugin's data directory. A pure function so the paths it
/// derives can be checked without touching the real, root-owned location.
fn paths_in(data: &Path) -> (PathBuf, PathBuf) {
    (data.join("media-roots.toml"), data.join("credentials"))
}

/// Reads the `uid` and `gid` of the service user from `/etc/passwd`.
///
/// A file read rather than a dependency on `nix` or `libc`: it is three lines,
/// testable, and it avoids pulling a whole crate into a binary that does
/// nothing but call `mount`.
fn uid_gid(passwd: &str, user: &str) -> Option<(u32, u32)> {
    passwd.lines().find_map(|l| {
        let mut fields = l.split(':');
        (fields.next()? == user).then_some(())?;
        let _password = fields.next()?;
        let uid = fields.next()?.parse().ok()?;
        let gid = fields.next()?.parse().ok()?;
        Some((uid, gid))
    })
}

/// Mount points under `MOUNT_ROOT` currently mounted.
///
/// The parsing of `/proc/mounts` and its unescaping come from
/// `mount::mount_points`: a single implementation of this rule, otherwise the
/// two binaries would diverge on a rare detail — one handling the escaped tab
/// and not the other, for instance.
fn mounted_under_root(proc_mounts: &str) -> Vec<PathBuf> {
    mount_points(proc_mounts).filter(|p| p.starts_with(MOUNT_ROOT)).collect()
}

/// Mount points of the declared shares that are mounted, but not in the mode
/// the table now asks for: read-only where `writable` is set, or the reverse.
///
/// Without this, a share already mounted was skipped whatever its options, so
/// ticking "writable" on the page wrote the table, started this service, and
/// changed nothing until the next reboot — while the plugin, reading `writable`
/// from the table, went on to try writes the kernel refused. `ro` is a mount
/// option, not a flag read at every write: the only way to change it is to
/// mount again.
///
/// Answered by keeping only the lines whose options say `rw` and asking
/// `is_mounted_in` about those, rather than by reading the mount point column
/// here: the unescaping of that column has a single implementation (see
/// `mount_points`), and a second one in this binary is how the two would
/// drift apart.
fn mounted_in_another_mode(proc_mounts: &str, roots: &Roots) -> Vec<PathBuf> {
    let read_write: String = proc_mounts
        .lines()
        .filter(|l| l.split_whitespace().nth(3).is_some_and(|o| o.split(',').any(|o| o == "rw")))
        .map(|l| format!("{l}\n"))
        .collect();
    roots
        .root
        .iter()
        .filter(|r| r.kind == RootKind::Smb)
        .map(|r| (r.mount_point(), r.writable))
        .filter(|(point, writable)| {
            is_mounted_in(proc_mounts, point) && is_mounted_in(&read_write, point) != *writable
        })
        .map(|(point, _)| point)
        .collect()
}

/// Locations of the `mount.cifs` helper. Both, not just `/sbin`: on a
/// merged-`/usr` distribution it is the same file, on the others it is not.
const CIFS_HINTS: [&str; 2] = ["/sbin/mount.cifs", "/usr/sbin/mount.cifs"];

/// Is `mount.cifs` installed?
///
/// The existence predicate is injected rather than read directly: the rule
/// can then be tested without depending on the machine running the tests,
/// which has neither `cifs-utils` nor the right to place a file there.
fn cifs_help<F: Fn(&str) -> bool>(exists: F) -> Option<&'static str> {
    CIFS_HINTS.into_iter().find(|c| exists(c))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let (roots_path, creds_dir) = paths_in(Path::new(FILES_DATA_DIR));
    let user = env_or("RITORNELLO_USER", "ritornello");

    let roots = match Roots::load(&roots_path) {
        Ok(r) => r,
        Err(e) => {
            // No configuration = nothing to mount, not a failure: the service
            // is activated at machine boot, before any share has ever been
            // declared.
            if !roots_path.exists() {
                tracing::info!("{} does not exist yet: nothing to mount", roots_path.display());
                return Ok(());
            }
            return Err(e).with_context(|| format!("reading {}", roots_path.display()));
        }
    };
    // Belt and braces: `load` already validates, but this line is what makes
    // the invariant visible on the privileged side.
    roots.validate()?;

    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    let (uid, gid) = uid_gid(&passwd, &user)
        .with_context(|| format!("no user named {user} in /etc/passwd"))?;

    let proc_mounts = std::fs::read_to_string("/proc/mounts").context("reading /proc/mounts")?;

    // Unmount first what is no longer declared: a root removed from the page
    // must disappear, otherwise the share would stay mounted until the next
    // reboot of the machine.
    let wanted: Vec<PathBuf> = roots
        .root
        .iter()
        .filter(|r| r.kind == RootKind::Smb)
        .map(|r| r.mount_point())
        .collect();
    for mounted in mounted_under_root(&proc_mounts) {
        if wanted.contains(&mounted) {
            continue;
        }
        let output = std::process::Command::new("umount").arg(&mounted).output();
        match output {
            Ok(s) if s.status.success() => tracing::info!("unmounted {}", mounted.display()),
            Ok(s) => tracing::warn!(
                "unmounting {}: {}",
                mounted.display(),
                String::from_utf8_lossy(&s.stderr).trim()
            ),
            Err(e) => tracing::warn!("unmounting {}: {e}", mounted.display()),
        }
    }

    // Then what is declared but mounted in the wrong mode: unmounted here so
    // that the mount loop below mounts it again with the table's options.
    // **Lazily**, unlike a removed root: the owner flips "writable" from the
    // page, typically while a track of that very share is playing, and a
    // plain `umount` would then fail with "target is busy" — a toggle that
    // does nothing, again. Detached, the old mount lives on for the file mpv
    // holds open, and the new one takes its place at once.
    for point in mounted_in_another_mode(&proc_mounts, &roots) {
        let output = std::process::Command::new("umount").arg("--lazy").arg(&point).output();
        match output {
            Ok(s) if s.status.success() => {
                tracing::info!("unmounted {} to remount it in its new mode", point.display())
            }
            Ok(s) => tracing::error!(
                "unmounting {} to change its mode: {}",
                point.display(),
                String::from_utf8_lossy(&s.stderr).trim()
            ),
            Err(e) => tracing::error!("unmounting {} to change its mode: {e}", point.display()),
        }
    }
    // Read again rather than patched: what the loops above actually achieved
    // is the kernel's to say, and a failed `umount` must leave its share
    // counted as mounted, not mounted a second time on top of itself.
    let proc_mounts = std::fs::read_to_string("/proc/mounts").context("reading /proc/mounts")?;

    // `mount -t cifs` does not mount by itself: it delegates to `mount.cifs`,
    // the only one that knows how to read a `credentials=` file. Without that
    // program, `mount` calls mount(2) directly, the option is no longer read
    // by anyone and the opened session is anonymous — refused by the NAS. The
    // failure returned is then "cannot mount //host/share read-only", which
    // names neither the authentication nor the missing package: observed on
    // DietPi bookworm, one hour to attribute it. Hence this preliminary check,
    // which replaces an attempt whose message misleads with a line that says
    // what to install.
    //
    // After the unmount loop, not before: removing a share from the page must
    // keep unmounting it, which `umount` handles on its own.
    let to_mount = roots
        .root
        .iter()
        .filter(|r| r.kind == RootKind::Smb)
        .filter(|r| !is_mounted_in(&proc_mounts, &r.mount_point()))
        .count();
    if to_mount > 0 && cifs_help(|c| Path::new(c).exists()).is_none() {
        // `error!` then exit with success: the service remains a reconciler
        // that reports, and a failed unit would bring nothing more than noise
        // at machine boot.
        tracing::error!(
            "mount.cifs not found in /sbin or /usr/sbin: install cifs-utils \
             (see docs/installation.md); {to_mount} declared share(s) left unmounted"
        );
        return Ok(());
    }

    for r in roots.root.iter().filter(|r| r.kind == RootKind::Smb) {
        let point = r.mount_point();
        if is_mounted_in(&proc_mounts, &point) {
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&point) {
            tracing::error!("creating {}: {e}", point.display());
            continue;
        }
        let cmd = mount_command(r, &creds_dir, uid, gid);
        match std::process::Command::new(&cmd[0]).args(&cmd[1..]).output() {
            // A failure does not fail the service: the other shares must be
            // mounted anyway, and the user will see the state from the page.
            Ok(s) if s.status.success() => tracing::info!("mounted {}", r.name),
            Ok(s) => {
                tracing::error!("mounting {}: {}", r.name, String::from_utf8_lossy(&s.stderr).trim())
            }
            Err(e) => tracing::error!("mounting {}: {e}", r.name),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_MOUNTS: &str = "\
proc /proc proc rw,relatime 0 0
//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0
//192.168.1.20/photos /mnt/ritornello/ma\\040musique cifs ro 0 0
/dev/sda1 /media/usb ext4 rw 0 0
";

    #[test]
    fn a_mount_point_absent_from_proc_mounts_is_not_mounted() {
        assert!(is_mounted_in(PROC_MOUNTS, Path::new("/mnt/ritornello/nas")));
        assert!(!is_mounted_in(PROC_MOUNTS, Path::new("/mnt/ritornello/autre")));
    }

    #[test]
    fn a_mount_point_with_an_escaped_space_is_recognized() {
        // /proc/mounts escapes the space as \040. Without this handling, the
        // share would pass for unmounted and be remounted at every
        // reconciliation.
        assert!(is_mounted_in(PROC_MOUNTS, Path::new("/mnt/ritornello/ma musique")));
    }

    #[test]
    fn only_mounts_under_the_root_are_candidates_for_unmounting() {
        // The binary runs as root: it must never unmount anything outside its
        // domain, /proc and /media/usb included.
        let under = mounted_under_root(PROC_MOUNTS);
        assert_eq!(
            under,
            vec![
                PathBuf::from("/mnt/ritornello/nas"),
                PathBuf::from("/mnt/ritornello/ma musique")
            ]
        );
    }

    /// The table as the plugin writes it, one share named `music`.
    fn music_share(writable: bool) -> Roots {
        toml::from_str(&format!(
            "[[root]]\nname = \"music\"\nkind = \"smb\"\nhost = \"192.168.1.15\"\n\
             share = \"music\"\nuser = \"ritornello\"\ndomain = \"\"\n\
             writable = {writable}\narchive_covers = true\n"
        ))
        .unwrap()
    }

    /// Captured on the device on 2026-10-06, after "writable" had been ticked
    /// on the page: the table said writable, the kernel still said `ro`, and
    /// no cover could ever be archived.
    const MOUNTED_RO: &str = "//192.168.1.15/music /mnt/ritornello/music cifs ro,relatime,vers=3.1.1,\
cache=strict,upcall_target=app,username=ritornello,uid=986,forceuid,gid=986,forcegid,\
addr=192.168.1.15,file_mode=0755,dir_mode=0755,iocharset=utf8,soft,nounix,serverino,\
mapposix,reparse=nfs,nativesocket,symlink=native,rsize=4194304,wsize=4194304,\
bsize=1048576,retrans=1,echo_interval=10,actimeo=30,closetimeo=1 0 0\n";

    fn mounted_rw() -> String {
        MOUNTED_RO.replacen(" ro,", " rw,", 1)
    }

    #[test]
    fn a_share_made_writable_while_mounted_read_only_is_remounted() {
        // The defect met on the device: skipped because it was mounted.
        assert_eq!(
            mounted_in_another_mode(MOUNTED_RO, &music_share(true)),
            vec![PathBuf::from("/mnt/ritornello/music")]
        );
    }

    #[test]
    fn a_share_made_read_only_while_mounted_writable_is_remounted() {
        // The other direction of the same toggle: withdrawing the permission
        // must withdraw it from the kernel too, not only from the table.
        assert_eq!(
            mounted_in_another_mode(&mounted_rw(), &music_share(false)),
            vec![PathBuf::from("/mnt/ritornello/music")]
        );
    }

    #[test]
    fn a_share_already_in_its_mode_is_left_alone() {
        // Every reconciliation goes through here, at boot included: a share
        // remounted when nothing changed would cut the music for nothing.
        assert!(mounted_in_another_mode(MOUNTED_RO, &music_share(false)).is_empty());
        assert!(mounted_in_another_mode(&mounted_rw(), &music_share(true)).is_empty());
    }

    #[test]
    fn a_share_not_mounted_is_left_to_the_mount_loop() {
        // Not mounted at all is not "mounted in another mode": unmounting it
        // would fail, and the mount loop already mounts it with its options.
        let elsewhere = "/dev/sda1 /media/usb ext4 rw 0 0\n";
        assert!(mounted_in_another_mode(elsewhere, &music_share(true)).is_empty());
        assert!(mounted_in_another_mode(elsewhere, &music_share(false)).is_empty());
    }

    #[test]
    fn rw_is_read_in_the_options_column_only() {
        // A share whose path or source holds "rw" is still read-only when its
        // options say `ro`; and an option merely starting with "rw" is not `rw`.
        let named_rw = "//nas/rw /mnt/ritornello/music cifs ro,rwpidforward 0 0\n";
        assert_eq!(
            mounted_in_another_mode(named_rw, &music_share(true)),
            vec![PathBuf::from("/mnt/ritornello/music")]
        );
        assert!(mounted_in_another_mode(named_rw, &music_share(false)).is_empty());
    }

    #[test]
    fn the_mode_of_a_share_whose_name_has_a_space_is_read_too() {
        // The mount point column is escaped (`\040`): reading it here by hand
        // would miss this share, and leave its toggle without effect again.
        let roots: Roots = toml::from_str(
            "[[root]]\nname = \"ma musique\"\nkind = \"smb\"\nhost = \"nas\"\n\
             share = \"x\"\nuser = \"u\"\nwritable = true\n",
        )
        .unwrap();
        let mounts = "//nas/x /mnt/ritornello/ma\\040musique cifs ro 0 0\n";
        assert_eq!(
            mounted_in_another_mode(mounts, &roots),
            vec![PathBuf::from("/mnt/ritornello/ma musique")]
        );
        // And already writable, it is recognised as such: a column read by
        // hand would not find it among the `rw` lines, and would remount this
        // share at every reconciliation, cutting whatever it was playing.
        let mounts = "//nas/x /mnt/ritornello/ma\\040musique cifs rw 0 0\n";
        assert!(mounted_in_another_mode(mounts, &roots).is_empty());
    }

    #[test]
    fn a_local_root_is_never_remounted() {
        // Only shares are this binary's to mount; a device folder whose path
        // happens to be a read-only mount is not its business.
        let roots: Roots =
            toml::from_str("[[root]]\nname = \"usb\"\nkind = \"local\"\npath = \"/mnt/ritornello/usb\"\nwritable = true\n")
                .unwrap();
        let mounts = "/dev/sda1 /mnt/ritornello/usb ext4 ro 0 0\n";
        assert!(mounted_in_another_mode(mounts, &roots).is_empty());
    }

    #[test]
    fn uid_and_gid_are_read_from_passwd() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      ritornello:x:998:997::/var/lib/ritornello:/usr/sbin/nologin\n";
        assert_eq!(uid_gid(passwd, "ritornello"), Some((998, 997)));
        assert_eq!(uid_gid(passwd, "absent"), None);
    }

    #[test]
    fn a_truncated_passwd_does_not_yield_a_wrong_uid() {
        // Better to return nothing than to return 0: the mount would then
        // assign the files to root, and the service could no longer read them.
        assert_eq!(uid_gid("ritornello:x\n", "ritornello"), None);
        assert_eq!(uid_gid("ritornello:x:abc:997::/:/bin/sh\n", "ritornello"), None);
    }

    #[test]
    fn the_cifs_helper_is_looked_for_in_both_sbin() {
        assert_eq!(cifs_help(|c| c == "/sbin/mount.cifs"), Some("/sbin/mount.cifs"));
        // A distribution without merged /usr only has that one.
        assert_eq!(cifs_help(|c| c == "/usr/sbin/mount.cifs"), Some("/usr/sbin/mount.cifs"));
    }

    #[test]
    fn without_cifs_utils_the_helper_is_absent() {
        // The case that cost one hour on the device: `cifs-utils` not
        // installed, and a "cannot mount … read-only" that did not say so.
        assert_eq!(cifs_help(|_| false), None);
    }

    #[test]
    fn the_helper_and_the_plugin_agree_on_the_data_directory() {
        // Two independent binaries, one path: a helper reading a directory of
        // its own would silently stop finding what the plugin just wrote.
        assert_eq!(Path::new(FILES_DATA_DIR), ritornello_plugin_sdk::default_data_dir("files"));
    }

    #[test]
    fn the_helper_reads_the_table_and_the_credentials_from_its_data_directory() {
        let d = Path::new("/x/plugins/files");
        let (roots, creds) = paths_in(d);
        assert_eq!(roots, d.join("media-roots.toml"));
        assert_eq!(creds, d.join("credentials"));
    }
}
