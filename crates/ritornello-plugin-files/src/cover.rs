//! The cover lying next to the files: `folder.jpg` and its cousins.
//!
//! It is the plugin that does this work, and not the core: it is the one that
//! mounted the share and that knows the root of the declared source. And a
//! `folder.jpg` has nothing to extract — the path is enough, so no bytes travel
//! over the channel.

use ritornello_proto::CoverRef;
use std::path::{Path, PathBuf};

/// By order of preference. `cover` first: it is the most explicit name.
const PREFERENCES: [&str; 5] = ["cover", "folder", "front", "albumart", "album"];

/// Recognized extensions.
const EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

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
                p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(preferred))
            })
            .cloned()
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
}
