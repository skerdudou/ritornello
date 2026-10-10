//! The cover lying next to the files: `folder.jpg` and its cousins.
//!
//! It is the plugin that does this work, and not the core: it is the one that
//! mounted the share and that knows the root of the declared source. And a
//! `folder.jpg` has nothing to extract — the path is enough, so no bytes travel
//! over the channel.

use ritornello_proto::CoverRef;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A name this module recognises as a folder's front face — and therefore the
/// only names the archiver may be told to write under.
///
/// **A closed set, and that is the point.** The owner picks the name the
/// appliance writes a found cover under, and a name outside this list would be
/// written and then never read back: the next track of the album would announce
/// no cover, and the core would fetch the same image from the network again,
/// for ever. Making the setting this type rather than a string is what keeps the
/// writer and the reader from drifting apart — [`PREFERENCES`] is built from it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoverName {
    /// The default, and what Kodi and MPD look for first.
    #[default]
    Cover,
    /// What Windows writes, and what many other players look for first.
    Folder,
    Front,
    Albumart,
    Album,
}

impl CoverName {
    /// The file stem, without an extension: the extension is chosen by the
    /// bytes at write time, never by the setting.
    pub fn stem(self) -> &'static str {
        match self {
            CoverName::Cover => "cover",
            CoverName::Folder => "folder",
            CoverName::Front => "front",
            CoverName::Albumart => "albumart",
            CoverName::Album => "album",
        }
    }
}

/// By order of preference. `cover` first: it is the most explicit name.
///
/// Every variant of [`CoverName`] appears here, and a test holds it: one
/// missing would be a name the archiver writes and this module never reads.
pub(crate) const PREFERENCES: [CoverName; 5] = [
    CoverName::Cover,
    CoverName::Folder,
    CoverName::Front,
    CoverName::Albumart,
    CoverName::Album,
];

/// Recognized extensions.
///
/// `pub(crate)` because `archive` reads **this** list and not a copy of it: what
/// this module accepts as the image of a folder is exactly what the archiver
/// must refuse to write over. Two lists that drifted would let the appliance
/// drop a `cover.jpg` beside a `cover.webp` the owner scanned himself — two
/// front faces in one folder, and the winner settled by the alphabetical sort
/// in `images_of` rather than by him.
pub(crate) const EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// Artwork subdirectories visited, on **one single** level.
const SUBDIRECTORIES: [&str; 4] = ["artwork", "scans", "covers", "art"];

/// What is not the front face.
///
/// Applies **only to the single-image rule**, the only one that guesses: the
/// preference lists only retain a name they know, so a directory carrying
/// `front.jpg` and `back.jpg` is settled by the preference.
const EXCLUDED: [&str; 8] =
    ["back", "verso", "inlay", "cd", "disc", "disque", "booklet", "matrix"];

/// Searches for the cover of the played file. `None` = nothing certain, we stay silent.
pub fn search(file: &Path) -> Option<CoverRef> {
    let directory = file.parent()?;
    if let Some(p) = by_preference(directory) {
        return Some(path(p));
    }
    for sub in SUBDIRECTORIES {
        let Some(candidate) = subdirectory(directory, sub) else { continue };
        if let Some(p) = by_preference(&candidate) {
            return Some(path(p));
        }
    }
    single_image(directory).map(path)
}

fn path(p: PathBuf) -> CoverRef {
    CoverRef::Path { path: p.to_string_lossy().into_owned() }
}

/// The artwork subdirectory, whatever its case.
fn subdirectory(directory: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .find(|e| {
            e.file_name().to_string_lossy().eq_ignore_ascii_case(name)
                && e.file_type().is_ok_and(|t| t.is_dir())
        })
        .map(|e| e.path())
}

/// The first name of the preference list present in the directory.
fn by_preference(directory: &Path) -> Option<PathBuf> {
    let images = images_of(directory);
    PREFERENCES.iter().find_map(|preferred| {
        images
            .iter()
            .find(|p| {
                p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(preferred.stem()))
            })
            .cloned()
    })
}

/// Does this path name an image `by_preference` would pick up — any of the
/// [`PREFERENCES`], in any case, with any of the [`EXTENSIONS`]?
///
/// The archiver's "never overwrite" check, and the reason it lives here: once
/// the owner may choose `folder`, a folder already holding a `cover.jpg` of the
/// owner's own scanning must refuse the write too, or it would end up with two
/// front faces — settled by preference order rather than by the owner. A check on the chosen
/// name alone would let exactly that through.
pub(crate) fn names_a_cover(path: &Path) -> bool {
    let Some(stem) = path.file_stem() else { return false };
    let stem = stem.to_string_lossy();
    if !PREFERENCES.iter().any(|p| stem.eq_ignore_ascii_case(p.stem())) {
        return false;
    }
    path.extension().is_some_and(|e| {
        EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
    })
}

/// The single image of the directory, if it is unique **and** if its name does
/// not say it is something other than the front face.
fn single_image(directory: &Path) -> Option<PathBuf> {
    let images = images_of(directory);
    let [only] = images.as_slice() else { return None };
    let stem = only.file_stem()?.to_string_lossy().to_ascii_lowercase();
    EXCLUDED.iter().all(|excluded| !stem.contains(excluded)).then(|| only.clone())
}

fn images_of(directory: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| {
                    EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
                })
        })
        .collect();
    // `read_dir` guarantees no order: sorting makes the choice reproducible.
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes a directory with the named files, and returns its path.
    fn tree(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in names {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        }
        dir
    }

    fn found(dir: &tempfile::TempDir) -> Option<String> {
        match search(&dir.path().join("01 - piste.flac")) {
            Some(ritornello_proto::CoverRef::Path { path }) => {
                Some(std::path::Path::new(&path).file_name().unwrap().to_string_lossy().into_owned())
            }
            _ => None,
        }
    }

    #[test]
    fn the_preference_order_wins_over_the_alphabetical_order() {
        let dir = tree(&["01 - piste.flac", "albumart.png", "cover.jpg", "front.jpg"]);
        assert_eq!(found(&dir).as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn case_does_not_matter() {
        let dir = tree(&["01 - piste.flac", "Folder.JPG"]);
        assert_eq!(found(&dir).as_deref(), Some("Folder.JPG"));
    }

    #[test]
    fn a_single_image_without_a_recognizable_name_is_taken() {
        let dir = tree(&["01 - piste.flac", "scan001.png"]);
        assert_eq!(found(&dir).as_deref(), Some("scan001.png"));
    }

    #[test]
    fn a_single_image_named_like_a_back_is_set_aside() {
        // Without this exclusion, we would show the back of the case. And
        // staying silent lets the generic relay take over.
        for back in ["back.jpg", "Scan_verso.png", "inlay.jpg", "booklet.png", "cd.jpg"] {
            let dir = tree(&["01 - piste.flac", back]);
            assert_eq!(found(&dir), None, "{back} should not be retained");
        }
    }

    #[test]
    fn two_images_without_a_recognizable_name_settle_nothing() {
        let dir = tree(&["01 - piste.flac", "scan001.png", "scan002.png"]);
        assert_eq!(found(&dir), None);
    }

    #[test]
    fn the_exclusion_does_not_apply_to_the_preference_list() {
        // `cd` is an exclusion pattern, but a file named `cover.jpg` is
        // retained without discussion: the exclusion only concerns the rule
        // that guesses.
        let dir = tree(&["01 - piste.flac", "cover.jpg", "back.jpg"]);
        assert_eq!(found(&dir).as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn an_artwork_subdirectory_is_visited_on_a_single_level() {
        let dir = tree(&["01 - piste.flac", "Artwork/front.jpg"]);
        assert_eq!(found(&dir).as_deref(), Some("front.jpg"));
        // Two levels: we do not walk a NAS to find an image.
        let deep = tree(&["01 - piste.flac", "Artwork/haute-def/front.jpg"]);
        assert_eq!(found(&deep), None);
    }

    #[test]
    fn the_directory_comes_before_the_subdirectory() {
        let dir = tree(&["01 - piste.flac", "folder.jpg", "Artwork/cover.jpg"]);
        assert_eq!(found(&dir).as_deref(), Some("folder.jpg"));
    }

    /// Every name the archiver can be told to write under. The `match` is what
    /// makes this list honest: a variant added to `CoverName` does not compile
    /// here until it is listed, and the test below then asks the reader about it.
    fn every_cover_name() -> Vec<CoverName> {
        let all = vec![
            CoverName::Cover,
            CoverName::Folder,
            CoverName::Front,
            CoverName::Albumart,
            CoverName::Album,
        ];
        for n in &all {
            match n {
                CoverName::Cover
                | CoverName::Folder
                | CoverName::Front
                | CoverName::Albumart
                | CoverName::Album => {}
            }
        }
        all
    }

    #[test]
    fn every_name_the_archiver_may_write_is_read_back() {
        // The writer and the reader must agree, or the archived cover is never
        // found and the network is asked again on every track. A second,
        // unrecognised image sits beside it, so that the single-image rule
        // cannot be what finds it: only the preference list may.
        for name in every_cover_name() {
            let written = format!("{}.webp", name.stem());
            let dir = tree(&["01 - piste.flac", &written, "scan001.png"]);
            assert_eq!(found(&dir).as_deref(), Some(written.as_str()), "{name:?}");
        }
    }

    #[test]
    fn a_cover_name_travels_as_its_stem() {
        // The page and the state file both carry the stem, lowercase.
        for name in every_cover_name() {
            assert_eq!(serde_json::to_value(name).unwrap(), name.stem());
        }
    }

    #[test]
    fn any_recognised_name_counts_as_a_cover_whatever_its_case() {
        for present in ["cover.jpg", "Folder.JPG", "FRONT.png", "albumart.Webp", "album.jpeg"] {
            assert!(names_a_cover(Path::new(present)), "{present}");
        }
        // Not a preference name, not an image, or a temporary left by a crash.
        for absent in ["scan001.jpg", "cover.txt", "cover", ".cover.jpg.1-2-3.tmp", "back.jpg"] {
            assert!(!names_a_cover(Path::new(absent)), "{absent}");
        }
    }
}
