//! Keeping on the share the cover the core found on the network.
//!
//! It is the plugin that writes, and not the core: it is the one that mounted
//! the share, that holds the per-root flag, and that can read the neighbours'
//! tags to tell an album folder from a catch-all. The core hands over a local
//! path — never bytes, this channel stays textual.
//!
//! # What this module refuses to do
//!
//! Everything here is written against one fact: the file it produces lands in
//! someone's own music library, on a NAS, and stays there. A cover written into
//! the wrong folder is worse than no feature at all, because a local image
//! wins over the network for ever after and nothing will ever look again. So
//! the module refuses far more often than it writes, and every refusal is a
//! sentence in the journal rather than an errno nobody can attribute.
//!
//! # The echo designates, it never compares
//!
//! `identity` is the echo of the track the core was looking at when it decided
//! to fetch the original. Fetching takes seconds, during which playback happily
//! moves on to the next track of the same album — so comparing this echo with
//! what is playing *now* would refuse a perfectly correct write. It names a
//! folder, and that is all it is asked to do. See `SourceReq::ArchiveCover`.
//!
//! But it crossed a process boundary, so it is read and never trusted: only
//! this plugin's own `{"kind":"file","path":…}` shape is accepted, and a path
//! carrying a `..` component is refused outright rather than resolved.

use ritornello_plugin_files::roots::{RootKind, Roots};
use std::path::{Component, Path, PathBuf};

/// What became of a hand-over. Three outcomes and not a `Result`: a **refusal**
/// is a decision on a stable state, a **failure** is an accident, and the
/// journal must not read the same for both. Retrying a refusal would be a bug —
/// the folder is heterogeneous, or an image is already there, and it will still
/// be so next time; retrying a failure is merely waiting for the share to come
/// back.
#[derive(Debug)]
pub enum Outcome {
    /// Written, and the path it landed on — the sentence the journal wants.
    Written(PathBuf),
    /// A decision: the folder is heterogeneous, an image is already there, the
    /// root says no. Nothing to retry.
    Refused(&'static str),
    /// An accident: the share went away, the disk is full.
    Failed(String),
}

/// The name the next listen will find. `cover` is the first entry of
/// `cover::PREFERENCES`, so this is the file `cover::search` picks up without
/// the network — and the one other players read too.
const NAME: &str = "cover";

/// The file the echo designates, if the echo is one of ours and is safe.
///
/// Public because the caller needs it *before* calling `store`: `Health::bounded`
/// is keyed on the mount point owning the path it is handed, and what this work
/// touches is the album folder on the share — not the staged file, which sits in
/// the appliance's own tmpfs and would charge a sleeping NAS's timeout to the
/// wrong mount.
///
/// `..` is refused and never resolved. Resolving would mean `canonicalize`,
/// which is a filesystem call on a possibly sleeping share made *before* the
/// circuit breaker is in play; and a component-wise refusal is a rule one can
/// read, where a normalisation is a rule one has to trust.
pub fn echoed_file(identity: &serde_json::Value) -> Option<PathBuf> {
    if identity.get("kind").and_then(serde_json::Value::as_str) != Some("file") {
        return None;
    }
    let path = PathBuf::from(identity.get("path").and_then(serde_json::Value::as_str)?);
    if path.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    Some(path)
}

/// Keeps the staged original beside the tracks of the folder the echo names.
///
/// The staged file is **ours from the moment we are handed the path**, and it is
/// removed here on every exit path, refusals included: the core never comes back
/// for it, and a megabyte left in the appliance's tmpfs once per album would
/// stay there until reboot. That is why the deletion is a wrapper around the
/// decision rather than a line inside it — there is no way to add an early
/// return that forgets it.
///
/// `album_of` is injected, and that is a design decision rather than a testing
/// convenience: the homogeneity rule below is the delicate part of this module,
/// and it deserves to be provable without writing a single tagged audio file.
/// Production passes [`album_from_tags`].
pub fn store(
    roots: &Roots,
    identity: &serde_json::Value,
    staged: &Path,
    album_of: &dyn Fn(&Path) -> Option<String>,
) -> Outcome {
    let outcome = decide(roots, identity, staged, album_of);
    let _ = std::fs::remove_file(staged);
    outcome
}

/// The decision itself. Cheapest and most decisive checks first, filesystem
/// last: a refusal the table alone can pronounce must never wait on a share.
fn decide(
    roots: &Roots,
    identity: &serde_json::Value,
    staged: &Path,
    album_of: &dyn Fn(&Path) -> Option<String>,
) -> Outcome {
    // 1. The echo, read and not trusted.
    let Some(played) = echoed_file(identity) else {
        return Outcome::Refused("the identity is not a file of this source");
    };
    // 2. The folder it designates.
    let Some(dir) = played.parent() else {
        return Outcome::Refused("the designated file has no parent directory");
    };

    // 3. What the roots table says — three answers, none of which touches a
    //    filesystem.
    let Some(root) = roots.root_of(dir) else {
        return Outcome::Refused("no declared root owns this folder");
    };
    if !root.archive_covers {
        return Outcome::Refused("this root was never told to archive covers");
    }
    // `writable` gates the cifs mount options, so it only means something for a
    // share; a local root answers at write time. Saying it here, and by name,
    // beats a kernel `EROFS` nobody can attribute — the same reasoning
    // `offers_archive` follows on the way out.
    if root.kind == RootKind::Smb && !root.writable {
        return Outcome::Refused("this share is mounted read-only");
    }

    // One `read_dir` for the two questions that follow. A sleeping NAS must be
    // asked once, not once per neighbour.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect::<Vec<PathBuf>>(),
        Err(e) => return Outcome::Failed(format!("cannot list {}: {e}", dir.display())),
    };

    // 4. Never overwrite. Any recognised extension counts, and the comparison
    //    is case-insensitive because `cover::by_preference` is: a `Cover.JPG`
    //    the owner scanned himself is already the answer to this folder, and
    //    replacing it with a stranger's would be the one damage this feature
    //    could do that nobody could undo.
    if entries.iter().any(|p| is_target_image(p)) {
        return Outcome::Refused("an image of that name is already there");
    }

    // 5. Homogeneity. A cover dropped in a folder speaks for the whole folder,
    //    so a catch-all — a "to sort" directory, a compilation of singles —
    //    must not receive the cover of one of its tracks.
    // The designated file's own silence abstains, and it abstains first: with
    // nothing to compare against, guessing is exactly what this module must not
    // do, and there is then no point asking the neighbours anything. A lone
    // untagged file proves no more than a folder full of them — a "to sort"
    // directory holding one download looks exactly like this.
    let Some(album) = album_of(&played) else {
        return Outcome::Refused("the designated file names no album");
    };
    let neighbours: Vec<PathBuf> =
        entries.into_iter().filter(|p| ritornello_plugin_files::scan::is_audio(p)).collect();
    // A neighbour's silence, on the other hand, is not disagreement. A badly
    // tagged album is still an album, and demanding unanimity of the tagged
    // *and* the mute would write nothing, ever.
    for neighbour in &neighbours {
        if neighbour == &played {
            continue;
        }
        if let Some(other) = album_of(neighbour)
            && !same_album(&album, &other)
        {
            return Outcome::Refused("the folder holds more than one album");
        }
    }

    // 6. The bytes decide the extension, never the staged name. A PNG written
    //    as `cover.jpg` would be read back by other players as a broken file.
    let bytes = match std::fs::read(staged) {
        Ok(bytes) => bytes,
        Err(e) => return Outcome::Failed(format!("cannot read the staged file: {e}")),
    };
    let Some(extension) = sniff(&bytes) else {
        return Outcome::Refused("the staged file is not a recognised image");
    };

    // 7. A temporary in the **same** directory, then a rename: atomic, so no
    //    listener ever sees a half-written cover, and no rename across a
    //    filesystem (the staged file lives in tmpfs, the target on the share).
    //    The name is unique to the attempt — see `attempt_name`, which is where
    //    that requirement is argued and where the three properties it has to
    //    keep are written down. The clock it needs is read here and passed in,
    //    rather than read inside it: it is the only part of that name a test
    //    cannot hold still, and holding it still is what proves the counter —
    //    and not the clock — is what keeps two attempts apart. Same injection,
    //    and the same reason, as `album_of` above.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temporary = dir.join(attempt_name(extension, stamp));
    let target = dir.join(format!("{NAME}.{extension}"));
    // The half-written file must not survive the failure that produced it, and
    // this branch is the one that actually half-writes: a share that goes away
    // mid-copy, a disk that fills. Leaving it would litter the owner's folder
    // with a dotfile per failure, one that nothing ever comes back to collect.
    if let Err(e) = std::fs::write(&temporary, &bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Outcome::Failed(format!("cannot write into {}: {e}", dir.display()));
    }
    if let Err(e) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Outcome::Failed(format!("cannot name {}: {e}", target.display()));
    }
    Outcome::Written(target)
}

/// Name of the temporary this attempt writes before renaming it into place.
///
/// **Unique to the attempt, and it has to be.** Two `store` calls on the same
/// folder can overlap: one still blocked on a share that stopped answering, a
/// second arriving when the owner comes back to that album. Both pass the "an
/// image is already there" check, and under a fixed temporary name both would
/// write into the same file and both would rename it — what would land as
/// `cover.jpg` is then neither image, permanently and silently, and it wins over
/// the network for ever after. The core does hold a single slot per cover key,
/// but it is a slot in another process that re-arms when the owner leaves an
/// album and returns; the integrity of a file in someone's music library must
/// not rest on another process's invariant.
///
/// Three properties this name must keep, and a test pins each of them:
///
/// - **it is a bare file name**, so the caller's `dir.join` puts it in the
///   destination directory and nowhere else: the rename then stays inside one
///   filesystem, which is what makes it atomic;
/// - **the leading dot and the `.tmp` extension survive** however long the
///   middle grows, so a leftover from a crash stays invisible to both
///   `cover::search` and `is_target_image` — each judges on the extension, and
///   `is_target_image` also on a stem that no longer reads as `cover`;
/// - **two attempts of the same process cannot collide.** That is the counter's
///   doing and not the clock's: a nanosecond stamp is only as fine as the
///   platform's clock, and the pid only separates processes. `nanos` is passed
///   in rather than read here for exactly that reason — a test that freezes it
///   is the only one that can tell the counter's work from the clock's, and
///   without that freeze a clock-only name passes on any machine whose clock
///   happens to be fine-grained. Measured: it does pass, on this one.
fn attempt_name(extension: &str, nanos: u128) -> String {
    static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The stamp separates this run from a leftover of an earlier one that
    // happened to be handed the same pid; the counter separates two attempts of
    // this run. A clock that refuses to answer costs neither.
    format!(".{NAME}.{extension}.{}-{nanos}-{seq}.tmp", std::process::id())
}

/// Is this an image already occupying the name we would write? Case-insensitive
/// on both halves, because `cover::by_preference` is: a `Cover.JPG` is already
/// this folder's answer, whatever the shift key was doing when it was scanned.
fn is_target_image(path: &Path) -> bool {
    let Some(stem) = path.file_stem() else { return false };
    if !stem.to_string_lossy().eq_ignore_ascii_case(NAME) {
        return false;
    }
    // `cover::EXTENSIONS` itself, never a copy of it: an extension `cover::search`
    // knows and this check does not would have us write a second front face into
    // a folder that already has one. See that constant's own doc.
    path.extension().is_some_and(|e| {
        crate::cover::EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
    })
}

/// Two album names for the same album?
///
/// Trimmed and case-insensitive: a folder tagged `Kind Of Blue` on one track and
/// `Kind of Blue` on the next is one album, and refusing it would be pedantry
/// paid for by the owner.
fn same_album(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The extension the bytes call for, or `None` if these are not an image we
/// recognise. Same three families the core serves, and the same headers.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// The production album reader. Passed to [`store`] as `&album_from_tags`; the
/// tests pass their own, which is why the homogeneity rule is provable without
/// `lofty` and without a single tagged file on disk.
///
/// Every failure reads as "this file names no album": an unreadable file, a
/// format the crate does not know, a tag without an album field and an album
/// field holding only spaces all mean the same thing to the rule above — there
/// is nothing here to agree or disagree with.
pub fn album_from_tags(path: &Path) -> Option<String> {
    let file = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    let tag = lofty::file::TaggedFileExt::primary_tag(&file)
        .or_else(|| lofty::file::TaggedFileExt::first_tag(&file))?;
    let album = lofty::tag::Accessor::album(tag)?;
    let album = album.trim();
    (!album.is_empty()).then(|| album.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_plugin_files::roots::Root;

    /// A temporary directory that answers like a `PathBuf`.
    ///
    /// The guard that erases the directory has to be kept alive by the test, but
    /// what the test wants to write is `dir.join(…)` and `dir.as_path()`. Holding
    /// both in one value is what lets the cases below read as assertions about
    /// paths rather than about `tempfile`.
    struct Dir {
        path: PathBuf,
        _guard: tempfile::TempDir,
    }

    impl std::ops::Deref for Dir {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.path
        }
    }

    /// The echo the core sends: this plugin's own identity shape, built exactly
    /// as `FilesSource::identity` builds it.
    fn identity(path: &Path) -> serde_json::Value {
        serde_json::json!({ "kind": "file", "path": path.to_string_lossy() })
    }

    /// An album reader over a fixed table, in place of the tags. This is what
    /// makes the homogeneity rule provable in three lines per case instead of
    /// one tagged FLAC per case.
    fn albums(pairs: &[(&str, &str)]) -> impl Fn(&Path) -> Option<String> + use<> {
        let map: std::collections::HashMap<String, String> =
            pairs.iter().map(|(n, a)| ((*n).to_string(), (*a).to_string())).collect();
        move |p: &Path| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            map.get(&name).cloned()
        }
    }

    /// A one-root table over `dir`: local, and told to archive. The ordinary
    /// case, which each refusal test then spoils in exactly one way.
    fn local_table(dir: &Path) -> Roots {
        Roots {
            root: vec![Root {
                name: "usb".into(),
                kind: RootKind::Local,
                path: Some(dir.to_string_lossy().into_owned()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable: false,
                archive_covers: true,
            }],
        }
    }

    /// An album folder holding `names` as empty files, its table, and a reader
    /// that gives every one of those files the same `album`.
    ///
    /// Empty files, and not tagged ones: the directory walk only has to *see*
    /// them, the albums coming from the injected reader. A homogeneous reader is
    /// what keeps each of the tests that is **not** about homogeneity measuring
    /// its own subject.
    fn local_root_with(
        names: &[&str],
        album: &str,
    ) -> (Dir, Roots, impl Fn(&Path) -> Option<String> + use<>) {
        let guard = tempfile::tempdir().unwrap();
        let path = guard.path().to_path_buf();
        for name in names {
            std::fs::write(path.join(name), b"").unwrap();
        }
        let table = local_table(&path);
        let pairs: Vec<(&str, &str)> = names.iter().map(|n| (*n, album)).collect();
        (Dir { path, _guard: guard }, table, albums(&pairs))
    }

    const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01the rest is not parsed";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR not parsed either";

    /// A staged original: a real temporary file holding real magic bytes.
    /// `store` only sniffs, so a header and some filler is the whole of what a
    /// decodable image would add here.
    fn staged(bytes: &[u8], stem: &str, extension: &str) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new()
            .prefix(stem)
            .suffix(extension)
            .tempfile()
            .expect("a staged file");
        std::fs::write(file.path(), bytes).unwrap();
        file
    }

    fn staged_jpeg() -> tempfile::NamedTempFile {
        staged(JPEG, "cover-", ".jpg")
    }

    fn staged_png_named(name: &str) -> tempfile::NamedTempFile {
        let (stem, extension) = name.rsplit_once('.').expect("a name with an extension");
        staged(PNG, stem, &format!(".{extension}"))
    }

    #[test]
    fn the_original_lands_beside_the_tracks_under_the_preferred_name() {
        // `cover` is the first name of the preference list, so the next
        // listen finds it without the network — and other players read it too.
        let (dir, table, albums) = local_root_with(&["01.flac", "02.flac"], "Kind of Blue");
        let staged = staged_jpeg();
        // Read **before** the call: the staged file belongs to us once handed
        // over, and the last assertion of this test is that it no longer exists.
        let handed_over = std::fs::read(staged.path()).unwrap();
        match store(&table, &identity(&dir.join("01.flac")), staged.path(), &albums) {
            Outcome::Written(p) => {
                assert_eq!(p.file_name().unwrap(), "cover.jpg");
                assert_eq!(p.parent().unwrap(), dir.as_path());
                assert_eq!(std::fs::read(&p).unwrap(), handed_over);
            }
            other => panic!("expected a write: {other:?}"),
        }
        // The staged file belongs to us once handed over.
        assert!(!staged.path().exists(), "the temp file must be reaped");
    }

    #[test]
    fn what_is_written_is_what_the_next_listen_finds() {
        // The whole point of the name: `cover::search` must pick up, without
        // the network, exactly the file this module just wrote. The two modules
        // hold the same list of names and extensions in two places, and this is
        // what keeps them honest.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        let staged = staged_jpeg();
        let played = dir.join("01.flac");
        let written = match store(&table, &identity(&played), staged.path(), &albums) {
            Outcome::Written(p) => p,
            other => panic!("expected a write: {other:?}"),
        };
        match crate::cover::search(&played) {
            Some(ritornello_proto::CoverRef::Path { path }) => {
                assert_eq!(PathBuf::from(path), written)
            }
            other => panic!("the archived cover must be the one the next listen finds: {other:?}"),
        }
    }

    #[test]
    fn the_extension_follows_the_bytes_and_not_the_staged_name() {
        // A PNG written as `cover.jpg` would be read back by other players as
        // a broken file. The magic number decides.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        let staged = staged_png_named("whatever.jpg");
        match store(&table, &identity(&dir.join("01.flac")), staged.path(), &albums) {
            Outcome::Written(p) => assert_eq!(p.file_name().unwrap(), "cover.png"),
            other => panic!("expected a write: {other:?}"),
        }
    }

    #[test]
    fn bytes_that_are_not_an_image_are_refused() {
        // Nothing downstream would ever look at this file again: an HTML error
        // page saved as `cover.jpg` would sit there for ever, and would win
        // over the network for ever.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        let staged = staged(b"<html>404</html>", "not-an-image-", ".jpg");
        assert_eq!(
            match store(&table, &identity(&dir.join("01.flac")), staged.path(), &albums) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "the staged file is not a recognised image"
        );
        assert!(!dir.join("cover.jpg").exists());
        assert!(!staged.path().exists(), "a refusal reaps the temp file too");
    }

    #[test]
    fn an_existing_image_is_never_overwritten() {
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        std::fs::write(dir.join("cover.jpg"), b"the owner's own scan").unwrap();
        let staged = staged_jpeg();
        assert_eq!(
            match store(&table, &identity(&dir.join("01.flac")), staged.path(), &albums) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "an image of that name is already there"
        );
        assert_eq!(std::fs::read(dir.join("cover.jpg")).unwrap(), b"the owner's own scan");
    }

    #[test]
    fn an_existing_image_blocks_whatever_its_case_and_its_extension() {
        // Driven off `cover::EXTENSIONS` itself, and not off a list written out
        // here: an extension added to that constant tomorrow becomes a case of
        // this test the same day. That is what makes the shared list a rule
        // rather than a coincidence — an extension `cover::search` recognised
        // and this check did not would leave two front faces in one folder,
        // with the winner settled by an alphabetical sort rather than by the owner.
        //
        // Upper-cased on purpose: `cover::by_preference` matches the stem
        // case-insensitively, so `COVER.JPG` is already this folder's answer.
        for extension in crate::cover::EXTENSIONS {
            let existing = format!("COVER.{}", extension.to_ascii_uppercase());
            let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
            std::fs::write(dir.join(&existing), b"the owner's own scan").unwrap();
            let staged = staged_jpeg();
            assert_eq!(
                match store(&table, &identity(&dir.join("01.flac")), staged.path(), &albums) {
                    Outcome::Refused(why) => why,
                    other => panic!("{existing}: expected a refusal: {other:?}"),
                },
                "an image of that name is already there",
                "{existing} must block the write"
            );
            assert!(!dir.join("cover.jpg").exists(), "{existing} must block the write");
        }
    }

    #[test]
    fn a_heterogeneous_folder_is_refused() {
        // A cover dropped in a folder speaks for the whole folder. A
        // catch-all must not receive the cover of one of its tracks.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01.flac"), b"").unwrap();
        std::fs::write(dir.path().join("02.flac"), b"").unwrap();
        let table = local_table(dir.path());
        let staged = staged_jpeg();
        let reader = albums(&[("01.flac", "Kind of Blue"), ("02.flac", "A Love Supreme")]);
        assert_eq!(
            match store(&table, &identity(&dir.path().join("01.flac")), staged.path(), &reader) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "the folder holds more than one album"
        );
        assert!(!dir.path().join("cover.jpg").exists());
    }

    #[test]
    fn a_neighbour_without_an_album_tag_does_not_block() {
        // Silence is not disagreement: a badly tagged album is still an
        // album, and demanding unanimity of the tagged *and* the mute would
        // write nothing, ever.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01.flac"), b"").unwrap();
        std::fs::write(dir.path().join("02.flac"), b"").unwrap();
        let table = local_table(dir.path());
        let staged = staged_jpeg();
        let reader = albums(&[("01.flac", "Kind of Blue")]);
        assert!(matches!(
            store(&table, &identity(&dir.path().join("01.flac")), staged.path(), &reader),
            Outcome::Written(_)
        ));
    }

    #[test]
    fn a_played_file_without_an_album_tag_is_abstained_from() {
        // Nothing to compare against: abstain rather than guess.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01.flac"), b"").unwrap();
        std::fs::write(dir.path().join("02.flac"), b"").unwrap();
        let table = local_table(dir.path());
        let staged = staged_jpeg();
        let reader = albums(&[("02.flac", "Kind of Blue")]);
        assert_eq!(
            match store(&table, &identity(&dir.path().join("01.flac")), staged.path(), &reader) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "the designated file names no album"
        );
    }

    #[test]
    fn the_same_album_spelled_differently_is_one_album() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01.flac"), b"").unwrap();
        std::fs::write(dir.path().join("02.flac"), b"").unwrap();
        let table = local_table(dir.path());
        let staged = staged_jpeg();
        let reader = albums(&[("01.flac", "Kind of Blue"), ("02.flac", " Kind Of Blue ")]);
        assert!(matches!(
            store(&table, &identity(&dir.path().join("01.flac")), staged.path(), &reader),
            Outcome::Written(_)
        ));
    }

    #[test]
    fn a_non_audio_neighbour_has_no_say() {
        // A `.cue`, a `.log`, a `readme.txt` next to the tracks say nothing
        // about the album, and asking their album would refuse the folder for
        // a reason that has nothing to do with music.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01.flac"), b"").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"").unwrap();
        let table = local_table(dir.path());
        let staged = staged_jpeg();
        let reader = albums(&[("01.flac", "Kind of Blue"), ("notes.txt", "A Love Supreme")]);
        assert!(matches!(
            store(&table, &identity(&dir.path().join("01.flac")), staged.path(), &reader),
            Outcome::Written(_)
        ));
    }

    #[test]
    fn a_read_only_share_is_refused_before_the_write_is_tried() {
        // A sentence in the journal beats a kernel EROFS nobody can
        // attribute — the rule `offers_archive` already states on the way out.
        //
        // The folder here **does not exist**, and that is the point: the
        // refusal is pronounced by the table alone, so nothing was tried and
        // nothing was even looked at. A temporary directory would have proved
        // the refusal but not that it came first.
        let table = Roots {
            root: vec![Root {
                name: "nas".into(),
                kind: RootKind::Smb,
                path: None,
                host: "192.168.1.20".into(),
                share: "musique".into(),
                subpath: None,
                user: "steven".into(),
                domain: String::new(),
                writable: false,
                archive_covers: true,
            }],
        };
        let played = Path::new("/mnt/ritornello/nas/Kind of Blue/01.flac");
        let staged = staged_jpeg();
        let reader = albums(&[("01.flac", "Kind of Blue")]);
        assert_eq!(
            match store(&table, &identity(played), staged.path(), &reader) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "this share is mounted read-only"
        );
        assert!(!staged.path().exists(), "a refusal reaps the temp file too");
    }

    #[test]
    fn a_root_that_was_never_told_to_archive_is_refused() {
        let (dir, mut table, albums) = local_root_with(&["01.flac"], "Album");
        table.root[0].archive_covers = false;
        let staged = staged_jpeg();
        assert_eq!(
            match store(&table, &identity(&dir.join("01.flac")), staged.path(), &albums) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "this root was never told to archive covers"
        );
        assert!(!dir.join("cover.jpg").exists());
    }

    #[test]
    fn a_folder_owned_by_no_root_is_refused() {
        // The table is the only authority on where this plugin may write. A
        // path outside every declared root is refused even though the echo is
        // well formed and the folder exists.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("01.flac"), b"").unwrap();
        let staged = staged_jpeg();
        let reader = albums(&[("01.flac", "Album")]);
        assert_eq!(
            match store(
                &Roots::default(),
                &identity(&dir.path().join("01.flac")),
                staged.path(),
                &reader
            ) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "no declared root owns this folder"
        );
        assert!(!dir.path().join("cover.jpg").exists());
    }

    #[test]
    fn an_identity_that_is_not_a_file_of_ours_is_refused() {
        // The echo is read, never trusted: it crossed a process boundary.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        for foreign in [
            serde_json::json!({ "kind": "radio", "url": "https://x" }),
            serde_json::json!({ "kind": "file" }),
            serde_json::json!("bare string"),
            serde_json::json!({ "kind": "file", "path": "/etc/ritornello/../../etc/passwd" }),
        ] {
            let staged = staged_jpeg();
            assert_eq!(
                match store(&table, &foreign, staged.path(), &albums) {
                    Outcome::Refused(why) => why,
                    other => panic!("{foreign}: expected a refusal: {other:?}"),
                },
                "the identity is not a file of this source",
                "{foreign} should be refused by the echo check and by no other"
            );
            assert!(!staged.path().exists(), "a refusal reaps the temp file too");
        }
        assert!(!dir.join("cover.jpg").exists());
    }

    #[test]
    fn a_traversal_is_refused_even_when_it_lands_inside_a_root() {
        // `..` is refused as a component, not resolved: this path would
        // normalise to a perfectly ordinary folder of the root, and accepting
        // it would mean the rule is "wherever the string ends up", which is
        // exactly the rule a hostile echo would like us to have.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        let traversal = dir.join("sub").join("..").join("01.flac");
        let staged = staged_jpeg();
        assert_eq!(
            match store(&table, &identity(&traversal), staged.path(), &albums) {
                Outcome::Refused(why) => why,
                other => panic!("expected a refusal: {other:?}"),
            },
            "the identity is not a file of this source"
        );
        assert!(!dir.join("cover.jpg").exists());
    }

    #[test]
    fn the_echo_names_the_folder_even_when_another_is_at_hand() {
        // Fetching the original takes seconds, during which playback moves on
        // — to the next track, or to another album entirely. `store` writes
        // into the folder of the **echo** and knows nothing of "now": that is
        // what makes a track advance harmless, where comparing identities
        // would have refused a correct write.
        let root = tempfile::tempdir().unwrap();
        let album = root.path().join("Kind of Blue");
        let other = root.path().join("A Love Supreme");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(album.join("01.flac"), b"").unwrap();
        std::fs::write(other.join("01.flac"), b"").unwrap();
        let table = local_table(root.path());
        let staged = staged_jpeg();

        let reader = albums(&[("01.flac", "Kind of Blue")]);
        match store(&table, &identity(&album.join("01.flac")), staged.path(), &reader) {
            Outcome::Written(p) => assert_eq!(p.parent().unwrap(), album.as_path()),
            other => panic!("expected a write: {other:?}"),
        }
        assert!(!other.join("cover.jpg").exists(), "the other folder is untouched");
    }

    #[test]
    fn two_attempts_in_one_folder_cannot_pick_the_same_temporary() {
        // The stamp is **frozen**, and that is the whole point: with the clock
        // held still, anything that keeps these names apart is the counter.
        // Measured, and worth recording — with the clock live, this test passes
        // even with the counter removed, because WSL's clock is fine-grained
        // enough that two thousand tight iterations never repeat a nanosecond.
        // It would not have caught a clock-only name; frozen, it does.
        //
        // No sleeping anywhere either: the property must hold for attempts
        // issued as fast as the machine can issue them, which is the shape a
        // share stuck for a second and a listener coming back actually produce.
        const FROZEN: u128 = 1_757_000_000_000_000_000;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1_000 {
            let name = attempt_name("jpg", FROZEN);
            // A bare file name: `dir.join` must land it in the destination
            // directory and nowhere else, or the rename would cross a
            // filesystem and stop being atomic.
            assert_eq!(
                Path::new(&name).components().count(),
                1,
                "{name} must be a bare file name"
            );
            // Whatever the middle grows into, the two things that hide a
            // leftover must survive.
            assert!(name.starts_with(".cover."), "{name} must stay a dotfile");
            assert_eq!(Path::new(&name).extension().unwrap(), "tmp", "{name}");
            assert!(seen.insert(name), "a temporary name repeated itself");
        }
        // Two threads, because the two racing attempts really are on two
        // threads: `Health::bounded` runs each `store` on its own
        // `spawn_blocking`, and the abandoned one keeps running.
        let names: Vec<String> = std::thread::scope(|s| {
            let a = s.spawn(|| (0..500).map(|_| attempt_name("jpg", FROZEN)).collect::<Vec<_>>());
            let b = s.spawn(|| (0..500).map(|_| attempt_name("jpg", FROZEN)).collect::<Vec<_>>());
            let mut v = a.join().unwrap();
            v.extend(b.join().unwrap());
            v
        });
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "two threads collided on a temporary name");
    }

    #[test]
    fn a_leftover_temporary_is_invisible_to_the_search_and_to_the_overwrite_check() {
        // What a crash between the write and the rename leaves in the folder.
        // It must not be taken for a cover by the next listen, and it must not
        // make the archiver believe the folder is already answered — the name
        // grew a pid, a stamp and a counter since it was last checked, so the
        // two rules that hide it are re-proved against the shape it has now.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        let leftover = dir.join(attempt_name("jpg", 1_757_000_000_000_000_000));
        std::fs::write(&leftover, JPEG).unwrap();
        let played = dir.join("01.flac");
        assert!(
            crate::cover::search(&played).is_none(),
            "a leftover temporary must not be served as the cover"
        );
        let staged = staged_jpeg();
        match store(&table, &identity(&played), staged.path(), &albums) {
            Outcome::Written(p) => assert_eq!(p.file_name().unwrap(), "cover.jpg"),
            other => panic!("a leftover temporary must not block the write: {other:?}"),
        }
    }

    #[test]
    fn a_successful_write_leaves_nothing_but_the_cover_behind() {
        // The rename consumes the temporary; nothing of the attempt survives it.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        let staged = staged_jpeg();
        assert!(matches!(
            store(&table, &identity(&dir.join("01.flac")), staged.path(), &albums),
            Outcome::Written(_)
        ));
        let mut left: Vec<String> = std::fs::read_dir(dir.as_path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["01.flac".to_string(), "cover.jpg".to_string()]);
    }

    #[test]
    fn a_failure_leaves_no_half_written_file_behind() {
        // A folder that vanished between the offer and the hand-over is an
        // accident, not a decision: it reads as `Failed`, and the journal must
        // be able to tell the two apart.
        let (dir, table, albums) = local_root_with(&["01.flac"], "Album");
        let played = dir.join("Nowhere").join("01.flac");
        let staged = staged_jpeg();
        // Named, not merely matched: `Failed` carries a formatted errno, so the
        // assertion is on the sentence this module wrote in front of it. A
        // `matches!` alone would be satisfied by a failure from any other step.
        let why = match store(&table, &identity(&played), staged.path(), &albums) {
            Outcome::Failed(why) => why,
            other => panic!("expected a failure: {other:?}"),
        };
        assert!(why.starts_with("cannot list "), "{why}");
        assert!(!staged.path().exists(), "a failure reaps the temp file too");
    }
}
