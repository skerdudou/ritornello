//! Building the mount command line.
//!
//! Isolated in its own module to be testable **without privilege**: it is the
//! code that decides what root will execute, and it deserves to be exercised
//! without any mount taking place.

use crate::roots::Root;
use std::path::Path;

/// Builds the mount command of a share.
///
/// The options are a **closed list**. No pass-through to `mount -o`: an option
/// coming from the configuration would be an option chosen by whoever reaches
/// the web UI, and executed by root.
///
/// `soft` because a sleeping NAS must return an I/O error rather than block
/// the reading process indefinitely. The corruption risk that advises against
/// `soft` for writing does not apply to a `ro` mount; it is accepted on a root
/// declared writable, which only serves to drop an m3u.
///
/// `soft` **is not enough**, and that is a lesson paid for: it bounds the
/// attempts of an operation on an already established session, not the
/// reconnection. On 2026-08-17, a NAS that cut its idle connections left the
/// cifs client blocked in the kernel long enough to freeze an `ls` — well beyond
/// the five seconds of the admin protocol. The three options that follow
/// shorten this worst case, without ever bringing it under a second; the real
/// bound lives on the caller's side, in `health`. Do not remove them believing
/// that `soft` covers the matter, and do not believe they make `health` useless.
///
/// - `echo_interval=10`: the kernel notices a session is dead after ten
///   seconds instead of sixty, its default.
/// - `retrans=1`: a single retry before `soft` returns its error.
/// - `actimeo=30`: attributes are cached for half a minute. This is the one
///   that matters most here — the page checks the existence of every track at
///   every probe, and without this cache each one would go back out on the
///   network.
///
/// None of the three could be measured from the development machine: they
/// require a real share that stops responding.
///
/// No `vers=`: the kernel's negotiation is better than a frozen version that
/// would age badly against an updated NAS.
pub fn mount_command(root: &Root, creds_dir: &Path, uid: u32, gid: u32) -> Vec<String> {
    let mut options = Vec::new();
    if !root.writable {
        options.push("ro".to_string());
    }
    options.push("soft".to_string());
    options.push("echo_interval=10".to_string());
    options.push("retrans=1".to_string());
    options.push("actimeo=30".to_string());
    options.push("iocharset=utf8".to_string());
    options.push(format!("uid={uid}"));
    options.push(format!("gid={gid}"));
    options.push(format!("credentials={}", root.credentials_path(creds_dir).display()));
    vec![
        "mount".to_string(),
        "-t".to_string(),
        "cifs".to_string(),
        format!("//{}/{}", root.host, root.share),
        // The mount point comes from `mount_point()`, hence from a constant and
        // the validated name — never from the configuration.
        root.mount_point().display().to_string(),
        "-o".to_string(),
        options.join(","),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roots::RootKind;

    /// Factory **local** to this module: the utilities of a `#[cfg(test)]`
    /// module do not cross modules.
    fn smb_root() -> Root {
        Root {
            name: "nas".into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: "musique".into(),
            subpath: Some("Albums".into()),
            user: "steven".into(),
            domain: String::new(),
            writable: false,
        }
    }

    #[test]
    fn the_mount_line_imposes_the_point_and_the_options() {
        let cmd =
            mount_command(&smb_root(), Path::new("/etc/ritornello/media-credentials"), 998, 998);
        assert_eq!(cmd[0], "mount");
        assert_eq!(cmd[1], "-t");
        assert_eq!(cmd[2], "cifs");
        assert_eq!(cmd[3], "//192.168.1.20/musique");
        // The subpath does NOT enter the mount point: it is the whole share
        // that is mounted, the subpath being only a place to look inside it.
        assert_eq!(cmd[4], "/mnt/ritornello/nas");
        assert_eq!(cmd[5], "-o");
        let options: Vec<&str> = cmd[6].split(',').collect();
        assert!(options.contains(&"ro"), "{options:?}");
        assert!(options.contains(&"soft"), "{options:?}");
        assert!(options.contains(&"iocharset=utf8"), "{options:?}");
        assert!(options.contains(&"uid=998"), "{options:?}");
        assert!(options.contains(&"gid=998"), "{options:?}");
        assert!(
            options.contains(&"credentials=/etc/ritornello/media-credentials/nas.cred"),
            "{options:?}"
        );
        // No frozen version: the kernel's negotiation is better.
        assert!(!options.iter().any(|o| o.starts_with("vers=")), "{options:?}");
    }

    #[test]
    fn a_writable_root_loses_ro_and_nothing_else() {
        let cmd = mount_command(&Root { writable: true, ..smb_root() }, Path::new("/c"), 1, 1);
        let options: Vec<&str> = cmd[6].split(',').collect();
        assert!(!options.contains(&"ro"), "{options:?}");
        assert!(
            options.contains(&"soft"),
            "soft must stay: a sleeping NAS must not block playback"
        );
        // Pinned because they look decorative and are not: they shorten the
        // kernel block that stuck the whole plugin on 2026-08-17. See the
        // documentation of `mount_command`.
        for o in ["echo_interval=10", "retrans=1", "actimeo=30"] {
            assert!(options.contains(&o), "{o} missing: {options:?}");
        }
    }

    #[test]
    fn the_line_contains_no_empty_argument() {
        // An empty argument would shift the whole rest of the line executed by
        // root, with a hard-to-predict effect.
        let cmd = mount_command(&smb_root(), Path::new("/c"), 1, 1);
        assert!(cmd.iter().all(|a| !a.is_empty()), "{cmd:?}");
    }
}
