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
use ritornello_plugin_files::mount::{is_mounted_in, mount_points};
use ritornello_plugin_files::mount_options::mount_command;
use ritornello_plugin_files::roots::{RootKind, Roots, MOUNT_ROOT};
use std::path::{Path, PathBuf};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
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

    let roots_path = env_or("RITORNELLO_FILES_ROOTS", "/etc/ritornello/media-roots.toml");
    let creds_dir = env_or("RITORNELLO_FILES_CREDENTIALS", "/etc/ritornello/media-credentials");
    let user = env_or("RITORNELLO_USER", "ritornello");

    let roots = match Roots::load(Path::new(&roots_path)) {
        Ok(r) => r,
        Err(e) => {
            // No configuration = nothing to mount, not a failure: the service
            // is activated at machine boot, before any share has ever been
            // declared.
            if !Path::new(&roots_path).exists() {
                tracing::info!("{roots_path} does not exist yet: nothing to mount");
                return Ok(());
            }
            return Err(e).with_context(|| format!("reading {roots_path}"));
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
        let cmd = mount_command(r, Path::new(&creds_dir), uid, gid);
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
}
