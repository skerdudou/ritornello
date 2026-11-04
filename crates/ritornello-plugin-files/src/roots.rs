//! The plugin's row of `media-roots.toml`: the share, plus the plugin's own
//! preferences about it.
//!
//! The share part — name, kind, path, host, share, subpath, credentials
//! identity, `writable` — is `ritornello_files_mount::roots::Root`, which the
//! root mount helper reads too, and its grammar is that library's alone. What
//! only this plugin reads lives here, beside it, **in the same `[[root]]`
//! row**: the file on disk keeps exactly the keys it always had, so tables
//! written before this split read back unchanged, and the helper reads this
//! plugin's tables while ignoring the keys it does not know.
//!
//! Why the split: every field of the library's `Root` is compiled into a
//! root binary. A preference there changed that binary, moved
//! `files-mount`'s number, and sent every device to `ritornello-install` for
//! a flag root never reads — `archive_lyrics` did exactly that. **A new
//! preference of this plugin goes here**, and the root helper does not move.
//!
//! The row derefs to the share part, so `root.name`, `root.base_dir()` or
//! `root.writable = true` read as they did when both lived in one struct.

use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};

pub use ritornello_files_mount::roots::{derive_name, field_on, RootError, RootKind, MOUNT_ROOT};

/// The share part of a row, as the root helper reads it.
pub use ritornello_files_mount::roots::Root as MountRoot;

/// The table as this plugin reads, validates and writes it. Validation is
/// the library's (`Roots::validate`, on the share part of each row).
pub type Roots = ritornello_files_mount::roots::Roots<Root>;

/// One declared root: the share, and what this plugin was told about it.
///
/// `flatten` writes the share's keys and these at the same level of one
/// `[[root]]` table, which is the layout every earlier version wrote and
/// read (pinned by `a_table_in_the_flat_layout_of_old_reads_back_and_rewrites_identically`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Root {
    #[serde(flatten)]
    pub mount: MountRoot,
    /// Keep on this root the full-size cover a network contributor found for a
    /// track that has none. False by default: writing into someone's music
    /// library is an explicit choice, like `writable`.
    ///
    /// **Not a mount option.** Unlike `writable`, changing it remounts
    /// nothing; on an SMB root it is inert until `writable` is set too, and
    /// the write path says so rather than letting an `EROFS` bubble up.
    #[serde(default)]
    pub archive_covers: bool,
    /// Keep on this root, beside a track, the lyrics a network contributor
    /// found for it when it has no `.lrc` of its own. Its own flag rather
    /// than a rider on `archive_covers`: a cover speaks for a whole folder,
    /// a `.lrc` for one track, and an owner may welcome one and not the
    /// other. False by default, for the same reason as `archive_covers`, and
    /// like it **not a mount option** and inert on a share until `writable`.
    #[serde(default)]
    pub archive_lyrics: bool,
}

impl From<MountRoot> for Root {
    /// A share with every preference at its default: nothing archived.
    fn from(mount: MountRoot) -> Self {
        Root { mount, archive_covers: false, archive_lyrics: false }
    }
}

impl AsRef<MountRoot> for Root {
    fn as_ref(&self) -> &MountRoot {
        &self.mount
    }
}

impl Deref for Root {
    type Target = MountRoot;

    fn deref(&self) -> &MountRoot {
        &self.mount
    }
}

impl DerefMut for Root {
    fn deref_mut(&mut self) -> &mut MountRoot {
        &mut self.mount
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row as it was before the split (`ritornello-files-mount` up to
    /// 1.0.2): one flat struct, frozen here so that the layout it wrote, and
    /// the way it reads what this plugin writes now, stay checked against
    /// the code rather than against a memory of it.
    #[derive(Debug, Serialize, Deserialize)]
    struct PreSplitRoot {
        name: String,
        kind: RootKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default)]
        host: String,
        #[serde(default)]
        share: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<String>,
        #[serde(default)]
        user: String,
        #[serde(default)]
        domain: String,
        #[serde(default)]
        writable: bool,
        #[serde(default)]
        archive_covers: bool,
        #[serde(default)]
        archive_lyrics: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct PreSplitRoots {
        #[serde(default)]
        root: Vec<PreSplitRoot>,
    }

    /// A table as the plugin wrote it before the split: one share with every
    /// field and both preferences set, one local folder with every optional
    /// field absent and no preference at all.
    const FLAT_LAYOUT: &str = r#"[[root]]
name = "music"
kind = "smb"
host = "192.168.1.15"
share = "music"
subpath = "Albums/Jazz, live"
user = "ritornello"
domain = "WORKGROUP"
writable = true
archive_covers = true
archive_lyrics = true

[[root]]
name = "usb"
kind = "local"
path = "/media/usb"
host = ""
share = ""
user = ""
domain = ""
writable = false
archive_covers = false
archive_lyrics = false
"#;

    #[test]
    fn a_table_in_the_flat_layout_of_old_reads_back_and_rewrites_identically() {
        // No migration: a device's existing table must read as it was meant,
        // and what this plugin writes back must be what an older plugin (or
        // a rollback to one) reads. Byte for byte, so that a key renamed,
        // nested under a sub-table or dropped by the split shows here.
        let table: Roots = toml::from_str(FLAT_LAYOUT).unwrap();
        table.validate().unwrap();
        let music = table.by_name("music").unwrap();
        assert_eq!(music.kind, RootKind::Smb);
        assert_eq!(music.host, "192.168.1.15");
        assert_eq!(music.subpath.as_deref(), Some("Albums/Jazz, live"));
        assert_eq!(music.domain, "WORKGROUP");
        assert!(music.writable && music.archive_covers && music.archive_lyrics);
        let usb = table.by_name("usb").unwrap();
        assert_eq!(usb.path.as_deref(), Some("/media/usb"));
        assert!(!usb.writable && !usb.archive_covers && !usb.archive_lyrics);

        assert_eq!(toml::to_string_pretty(&table).unwrap(), FLAT_LAYOUT);

        // And the fixture is what the flat struct wrote, and reads back
        // through it: a rollback to an older plugin finds its own table.
        let old: PreSplitRoots = toml::from_str(&toml::to_string_pretty(&table).unwrap()).unwrap();
        assert!(old.root[0].archive_covers && old.root[0].archive_lyrics && old.root[0].writable);
        assert_eq!(old.root[0].subpath.as_deref(), Some("Albums/Jazz, live"));
        assert_eq!(toml::to_string_pretty(&old).unwrap(), FLAT_LAYOUT);
    }

    #[test]
    fn every_preference_and_every_share_field_survives_a_rewrite() {
        // Each preference set alone on its own row, against rows that have
        // the other one: a write that dropped one, or a read that borrowed
        // one's value for the other, would show as a difference.
        let table: Roots = Roots {
            root: vec![
                Root {
                    archive_covers: true,
                    ..Root::from(MountRoot {
                        name: "covers".into(),
                        kind: RootKind::Smb,
                        path: None,
                        host: "nas".into(),
                        share: "music".into(),
                        subpath: Some("Ma Musique".into()),
                        user: "steven".into(),
                        domain: "HOME".into(),
                        writable: true,
                    })
                },
                Root {
                    archive_lyrics: true,
                    ..Root::from(MountRoot {
                        name: "lyrics".into(),
                        kind: RootKind::Local,
                        path: Some("/media/usb".into()),
                        host: String::new(),
                        share: String::new(),
                        subpath: None,
                        user: String::new(),
                        domain: String::new(),
                        writable: false,
                    })
                },
            ],
        };
        let reread: Roots = toml::from_str(&toml::to_string_pretty(&table).unwrap()).unwrap();
        assert_eq!(reread.root, table.root);
    }

    #[test]
    fn a_table_without_archive_lyrics_reads_back_as_false() {
        // Every table written before this flag existed lacks it, and the
        // device must read those as they were meant: nothing was ever
        // allowed to write a lyrics file. The other flags are set here so a
        // default borrowed from one of them (`archive_covers`, `writable`)
        // could not pass for this one.
        let roots: Roots = toml::from_str(
            r#"
[[root]]
name = "nas"
kind = "smb"
host = "192.168.1.20"
share = "musique"
writable = true
archive_covers = true
"#,
        )
        .unwrap();
        let nas = roots.by_name("nas").unwrap();
        assert!(nas.archive_covers && nas.writable);
        assert!(!nas.archive_lyrics);

        // And the flag reads back when it is there.
        let roots: Roots = toml::from_str(
            "[[root]]
name = \"usb\"
kind = \"local\"
path = \"/media/usb\"
archive_lyrics = true
",
        )
        .unwrap();
        assert!(roots.by_name("usb").unwrap().archive_lyrics);
    }

    #[test]
    fn the_library_validation_judges_the_share_part_of_the_plugin_row() {
        // The plugin row adds no grammar of its own and loses none: the
        // comma the root helper refuses is refused here, by the same code.
        let roots: Roots = toml::from_str(
            "[[root]]\nname = \"nas\"\nkind = \"smb\"\nhost = \"nas,uid=0\"\nshare = \"m\"\narchive_covers = true\n",
        )
        .unwrap();
        assert!(matches!(roots.validate(), Err(RootError::BadHost { .. })));
    }
}
