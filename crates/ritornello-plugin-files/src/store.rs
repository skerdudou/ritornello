//! Saved playlists: in the internal storage, or on a root.
//!
//! The format is m3u, the same as what we load: a playlist dropped on the NAS
//! must be readable there by any other player, and therefore carry paths
//! **relative** to the root it is placed on.
//!
//! Two asymmetries deserve to be stated, because they are deliberate:
//! saving requires `writable = true` whereas loading requires nothing (a
//! read-only root is perfectly legitimate for playback); and an unreachable
//! root is ignored by `list` without ever raising an error, failing which a
//! sleeping NAS would prevent seeing the internal playlists.

use crate::m3u::{self, Entry};
use crate::roots::Roots;
use ritornello_i18n::Catalog;
use std::path::{Path, PathBuf};

/// Where a saved playlist lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// The plugin's state directory, on the device.
    Internal,
    /// A declared root, designated by its name.
    Root(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    pub location: Location,
}

/// Typed error: the user-facing text is produced at the HTTP boundary via
/// `message(&Catalog)`. `Display` provides an English version for the internal
/// logs, outside the i18n scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    BadPlaylistName { name: String },
    ReadOnlyRoot { root: String },
    UnknownRoot { name: String },
    Io { path: String },
}

/// A playlist name becomes a **file name**, written either in `/var/lib` or
/// **on the network share**. Anything that could traverse is refused: no
/// separator (in either direction, an m3u coming from Windows carrying one),
/// no reserved name, no leading dot that would hide the playlist, no NUL byte
/// that would truncate a C string on the kernel side.
///
/// The length bound is not cosmetic: many file systems cap a component at
/// 255 bytes, and the name still receives a suffix.
fn valid_playlist_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// Destination directory of a **write**. The mount is `ro` by default:
/// refusing here with a sentence beats letting a kernel I/O error bubble up,
/// which nobody could attribute.
fn writable_dir(
    dest: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<PathBuf, StoreError> {
    match dest {
        Location::Internal => Ok(internal_dir.to_path_buf()),
        Location::Root(name) => {
            let r =
                roots.by_name(name).ok_or_else(|| StoreError::UnknownRoot { name: name.clone() })?;
            if !r.writable {
                return Err(StoreError::ReadOnlyRoot { root: name.clone() });
            }
            Ok(r.base_dir())
        }
    }
}

/// **Read** directory. No write check: that is the whole point of the
/// distinction with `writable_dir`.
fn readable_dir(
    from: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<PathBuf, StoreError> {
    match from {
        Location::Internal => Ok(internal_dir.to_path_buf()),
        Location::Root(name) => roots
            .by_name(name)
            .map(|r| r.base_dir())
            .ok_or_else(|| StoreError::UnknownRoot { name: name.clone() }),
    }
}

/// Atomic write: a temporary file, then `rename`. An interrupted save must
/// never leave behind a truncated playlist in place of the previous one.
fn write_atomically(file: &Path, tmp: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(tmp, text)?;
    std::fs::rename(tmp, file)
}

pub fn save(
    entries: &[Entry],
    name: &str,
    dest: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<(), StoreError> {
    if !valid_playlist_name(name) {
        return Err(StoreError::BadPlaylistName { name: name.to_string() });
    }
    let dir = writable_dir(dest, internal_dir, roots)?;
    // The internal directory, we create: on the first save it does not exist
    // yet. A root's directory, **never**: an unmounted share has an empty
    // mount point, and creating the tree there would write onto the local disk
    // a playlist that would vanish at the next mount.
    if matches!(dest, Location::Internal) {
        std::fs::create_dir_all(&dir)
            .map_err(|_| StoreError::Io { path: dir.display().to_string() })?;
    }
    // Relative paths when the destination is a root: that is what makes the
    // playlist readable elsewhere and lets it survive a change of mount point.
    // Internally, a base would make no sense — the tracks are not under the
    // state directory: absolute paths.
    let base = matches!(dest, Location::Root(_)).then(|| dir.clone());
    let text = m3u::render(entries, base.as_deref());
    let file = dir.join(format!("{name}.m3u"));
    let tmp = dir.join(format!("{name}.m3u.tmp"));
    write_atomically(&file, &tmp, &text).map_err(|_| {
        // A temporary file abandoned on the share would be visible to everyone
        // and serve no purpose any more.
        let _ = std::fs::remove_file(&tmp);
        StoreError::Io { path: file.display().to_string() }
    })
}

pub fn load(
    name: &str,
    from: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<m3u::Parsed, StoreError> {
    if !valid_playlist_name(name) {
        return Err(StoreError::BadPlaylistName { name: name.to_string() });
    }
    let dir = readable_dir(from, internal_dir, roots)?;
    let file = dir.join(format!("{name}.m3u"));
    let text = std::fs::read_to_string(&file)
        .map_err(|_| StoreError::Io { path: file.display().to_string() })?;
    Ok(m3u::parse(&text, &dir, &dir))
}

/// Every visible playlist, internal and roots combined.
///
/// An unreachable root is **ignored without error**: a sleeping NAS must not
/// prevent seeing the internal playlists. Each directory is returned sorted,
/// since no file system guarantees the order of `read_dir` — otherwise the
/// page would reorder its playlists from one refresh to the next.
pub fn list(internal_dir: &Path, roots: &Roots) -> Vec<Saved> {
    let mut out = in_dir(internal_dir, Location::Internal);
    for r in &roots.root {
        out.extend(in_dir(&r.base_dir(), Location::Root(r.name.clone())));
    }
    out
}

/// The playlists of **a single** directory.
///
/// Separated from `list` so the caller can bound each directory individually:
/// `read_dir` on a reconnecting share does not return, and the Admin half
/// serves its requests serially. See `health`.
pub fn in_dir(dir: &Path, loc: Location) -> Vec<Saved> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        let m3u = p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x.eq_ignore_ascii_case("m3u"))
            .unwrap_or(false);
        if m3u
            && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names.into_iter().map(|name| Saved { name, location: loc.clone() }).collect()
}

impl StoreError {
    /// Localized message handed to the user (body of the HTTP refusal).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            StoreError::BadPlaylistName { name } => {
                catalog.get("bad_playlist_name").replace("{name}", name)
            }
            StoreError::ReadOnlyRoot { root } => {
                catalog.get("read_only_root").replace("{name}", root)
            }
            StoreError::UnknownRoot { name } => catalog.get("unknown_root").replace("{name}", name),
            StoreError::Io { path } => catalog.get("store_io_error").replace("{path}", path),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::BadPlaylistName { name } => write!(f, "invalid playlist name: {name}"),
            StoreError::ReadOnlyRoot { root } => write!(f, "root mounted read-only: {root}"),
            StoreError::UnknownRoot { name } => write!(f, "unknown root: {name}"),
            StoreError::Io { path } => write!(f, "cannot write or read {path}"),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roots::{Root, RootKind};
    use tempfile::TempDir;

    /// The test roots are built **inside a `tempdir`**, hence as
    /// `RootKind::Local`: an `Smb` root would have `/mnt/ritornello/<name>` as
    /// `base_dir()`, where the test suite cannot write. Since the `writable`
    /// flag is checked whatever the kind, the rule remains provable without
    /// any mount at all.
    fn fixture_with(writable: bool) -> (TempDir, Roots) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("nas");
        std::fs::create_dir_all(&base).unwrap();
        let roots = Roots {
            root: vec![Root {
                name: "nas".into(),
                kind: RootKind::Local,
                path: Some(base.to_string_lossy().into_owned()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable,
            }],
        };
        (dir, roots)
    }

    fn fixture() -> (TempDir, Roots) {
        fixture_with(false)
    }

    fn writable_fixture() -> (TempDir, Roots) {
        fixture_with(true)
    }

    fn three_files(dir: &TempDir) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for name in ["Musique/01.mp3", "Musique/02.mp3", "Musique/03.mp3"] {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"").unwrap();
            out.push(p);
        }
        out
    }

    #[test]
    fn a_playlist_name_that_traverses_is_refused() {
        // The name becomes a file name, written either in /var/lib or on the
        // share: "../../etc/cron.d/x" must never reach the disk. The backslash
        // counts as much as the slash, since a name typed from a Windows
        // machine carries one; the leading dot would hide the playlist; the
        // NUL byte would truncate a C string on the kernel side.
        let (dir, roots) = fixture();
        let too_long = "x".repeat(65);
        let bad =
            ["../evasion", "a/b", "a\\b", "", ".", "..", ".cache", "x\0y", too_long.as_str()];
        for m in bad {
            assert!(
                matches!(
                    save(&[], m, &Location::Internal, dir.path(), &roots),
                    Err(StoreError::BadPlaylistName { .. })
                ),
                "wrongly accepted on save: {m:?}"
            );
            // Loading validates the name too: it builds the same path, and
            // refusing on one side only would leave the traversal open on
            // read.
            assert!(
                matches!(
                    load(m, &Location::Internal, dir.path(), &roots),
                    Err(StoreError::BadPlaylistName { .. })
                ),
                "wrongly accepted on load: {m:?}"
            );
        }
        // And an ordinary name does pass — the rule must not be so strict that
        // it forbids saving.
        assert!(save(&[], "Jazz du dimanche", &Location::Internal, dir.path(), &roots).is_ok());
    }

    #[test]
    fn saving_on_a_read_only_root_is_refused_with_a_sentence() {
        // The mount is `ro` by default: it has to be said clearly rather than
        // letting a kernel I/O error bubble up, which would name neither the
        // root nor the remedy.
        let (dir, roots) = fixture(); // "nas" is writable = false
        let err = save(&[], "Jazz", &Location::Root("nas".into()), dir.path(), &roots).unwrap_err();
        assert!(matches!(err, StoreError::ReadOnlyRoot { .. }), "{err:?}");
        assert!(!dir.path().join("nas/Jazz.m3u").exists(), "written despite the refusal");
    }

    #[test]
    fn loading_from_a_read_only_root_stays_allowed() {
        // The asymmetry is the heart of the rule: reading requires no write,
        // and the common case is precisely a share mounted `ro`.
        let (dir, roots) = fixture(); // writable = false
        let base = dir.path().join("nas");
        std::fs::write(base.join("Album.m3u"), "#EXTM3U\n#EXTINF:-1,So What\nAlbum/01.mp3\n")
            .unwrap();
        std::fs::create_dir_all(base.join("Album")).unwrap();
        std::fs::write(base.join("Album/01.mp3"), b"").unwrap();
        let reloaded = load("Album", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        assert_eq!(reloaded.entries.len(), 1, "unresolved: {:?}", reloaded.unresolved);
        assert_eq!(reloaded.entries[0].path, base.join("Album/01.mp3"));
    }

    #[test]
    fn a_playlist_saved_internally_reloads_identically() {
        let (dir, roots) = fixture();
        let files = three_files(&dir);
        let entries: Vec<Entry> = files
            .iter()
            .map(|p| Entry { path: p.clone(), title: None, duration_s: None })
            .collect();
        save(&entries, "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        let reloaded = load("Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        assert_eq!(reloaded.entries.len(), 3);
        assert!(reloaded.unresolved.is_empty());
        assert_eq!(reloaded.entries[0].path, files[0]);
    }

    #[test]
    fn a_playlist_saved_on_a_root_carries_relative_paths() {
        // That is what makes it readable by another player and lets it survive
        // a change of mount point.
        let (dir, roots) = writable_fixture();
        let base = roots.by_name("nas").unwrap().base_dir();
        let entries =
            vec![Entry { path: base.join("Album/01.mp3"), title: None, duration_s: None }];
        save(&entries, "Jazz", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        let text = std::fs::read_to_string(base.join("Jazz.m3u")).unwrap();
        assert!(text.contains("Album/01.mp3"), "{text}");
        assert!(!text.contains(base.to_str().unwrap()), "absolute path written: {text}");
    }

    #[test]
    fn listing_shows_internal_and_roots_together() {
        let (dir, roots) = writable_fixture();
        save(&[], "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        save(&[], "Rock", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        let listed = list(dir.path(), &roots);
        let mut names: Vec<String> = listed.iter().map(|s| s.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["Jazz", "Rock"]);
        // And each one knows where it comes from: otherwise reloading "Rock"
        // would look in the internal storage.
        assert_eq!(
            listed.iter().find(|s| s.name == "Rock").unwrap().location,
            Location::Root("nas".into())
        );
    }

    #[test]
    fn an_unreachable_root_does_not_prevent_seeing_internal_playlists() {
        // A sleeping NAS makes its mount point unreadable. If `list` failed
        // because of it, the page would show nothing at all any more.
        let (dir, mut roots) = writable_fixture();
        save(&[], "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        roots.root[0].path = Some("/inexistant/nulle-part".into());
        let names: Vec<String> = list(dir.path(), &roots).into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["Jazz"]);
    }

    #[test]
    fn writing_leaves_no_temporary_file_behind() {
        // The `.tmp` of the atomic rename must not stay visible on the share,
        // nor be mistaken for a saved playlist.
        let (dir, roots) = writable_fixture();
        save(&[], "Jazz", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        let remaining: Vec<String> = std::fs::read_dir(dir.path().join("nas"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining, vec!["Jazz.m3u".to_string()], "{remaining:?}");
    }

    #[test]
    fn an_unknown_root_is_named_rather_than_guessed() {
        // A root removed from the configuration while a playlist still
        // designates it: the refusal must say which one, on both sides.
        let (dir, roots) = writable_fixture();
        let absent = Location::Root("absente".into());
        assert!(matches!(
            save(&[], "Jazz", &absent, dir.path(), &roots),
            Err(StoreError::UnknownRoot { .. })
        ));
        assert!(matches!(
            load("Jazz", &absent, dir.path(), &roots),
            Err(StoreError::UnknownRoot { .. })
        ));
    }

    #[test]
    fn loading_a_missing_playlist_fails_naming_the_file() {
        // Without the path in the refusal, "cannot read" helps nobody
        // understand where the plugin went looking.
        let (dir, roots) = fixture();
        let err = load("Jazz", &Location::Internal, dir.path(), &roots).unwrap_err();
        match err {
            StoreError::Io { path } => assert!(path.ends_with("Jazz.m3u"), "{path}"),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn every_store_refusal_resolves_against_the_embedded_catalog() {
        // `Catalog::get` returns the key when it cannot find it: without this
        // test, a typo would display "read_only_root" on screen without
        // anything complaining. So we resolve against the catalog actually
        // embedded, and refuse a message reduced to its own key.
        let catalog =
            Catalog::load("files", "en", std::path::Path::new("/inexistant"), crate::FILES_EN);
        let messages = [
            StoreError::BadPlaylistName { name: "../x".into() }.message(&catalog),
            StoreError::ReadOnlyRoot { root: "nas".into() }.message(&catalog),
            StoreError::UnknownRoot { name: "absent".into() }.message(&catalog),
            StoreError::Io { path: "/x".into() }.message(&catalog),
        ];
        for m in &messages {
            assert!(m.contains(' '), "message reduced to a raw key: {m:?}");
        }
        // And the interpolation goes through: no token left as is.
        let interpolated = StoreError::ReadOnlyRoot { root: "nas".into() }.message(&catalog);
        assert!(interpolated.contains("nas"), "the refusal must name the root: {interpolated:?}");
        assert!(!interpolated.contains("{name}"), "token left as is: {interpolated:?}");
    }
}
