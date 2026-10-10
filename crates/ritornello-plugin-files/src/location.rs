//! How a person would name where a file comes from: the address of the share
//! for a file on the NAS, the path on the device otherwise.
//!
//! The core shows it in the provenance popover (see
//! `SourceMessage::location`). Only this plugin can say it: a file's identity
//! is its mount path on the device, and only the roots table knows which
//! share hides behind which mount point.

use std::path::Path;

use crate::roots::{RootKind, Roots};

/// `smb://<host>/<share>/<path inside the share>` for a file under an SMB
/// root, the path itself for anything else.
///
/// Never the user name nor the domain: the address names a place, and the
/// popover is read by anyone holding the remote. Not percent-encoded: this is
/// text to read and copy, not a link.
pub fn location_of(roots: &Roots, path: &Path) -> String {
    if let Some(root) = roots.root_of(path)
        && root.kind == RootKind::Smb
        && let Ok(inside) = path.strip_prefix(root.mount_point())
    {
        return format!("smb://{}/{}/{}", root.host, root.share, inside.to_string_lossy());
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roots::{Root, RootKind};

    fn smb(name: &str, subpath: Option<&str>) -> Root {
        Root {
            name: name.into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: "musique".into(),
            subpath: subpath.map(str::to_string),
            user: "steven".into(),
            domain: "WORKGROUP".into(),
            writable: false,
            archive_covers: false,
        }
    }

    #[test]
    fn a_share_is_named_by_its_address_without_any_credential() {
        let roots = Roots { root: vec![smb("nas", None)] };
        let file = smb("nas", None).mount_point().join("Mon Album/01 Titre.flac");
        let got = location_of(&roots, &file);
        assert_eq!(got, "smb://192.168.1.20/musique/Mon Album/01 Titre.flac");
        assert!(!got.contains("steven") && !got.contains("WORKGROUP"), "{got}");
    }

    #[test]
    fn the_subpath_is_part_of_the_address() {
        let root = smb("nas", Some("Jazz/Vinyles"));
        let file = root.base_dir().join("Kind of Blue/01.flac");
        let roots = Roots { root: vec![root] };
        assert_eq!(location_of(&roots, &file), "smb://192.168.1.20/musique/Jazz/Vinyles/Kind of Blue/01.flac");
    }

    #[test]
    fn a_local_root_or_no_root_gives_the_path_on_the_device() {
        let local = Root { name: "usb".into(), kind: RootKind::Local, path: Some("/media/cle".into()), ..smb("usb", None) };
        let roots = Roots { root: vec![local] };
        assert_eq!(location_of(&roots, Path::new("/media/cle/a.mp3")), "/media/cle/a.mp3");
        assert_eq!(location_of(&roots, Path::new("/elsewhere/b.mp3")), "/elsewhere/b.mp3");
    }

    #[test]
    fn a_local_root_is_never_taken_for_a_share_even_under_a_mount_point() {
        // The kind, not the place, decides: a local directory declared inside
        // the mount area (a bind mount, say) still has no share behind it, and
        // its empty host and share must not leak into an `smb://` address.
        let local = Root {
            name: "usb".into(),
            kind: RootKind::Local,
            path: Some("/mnt/ritornello/usb".into()),
            host: "192.168.1.20".into(),
            ..smb("usb", None)
        };
        let roots = Roots { root: vec![local] };
        assert_eq!(location_of(&roots, Path::new("/mnt/ritornello/usb/a.mp3")), "/mnt/ritornello/usb/a.mp3");
    }
}
