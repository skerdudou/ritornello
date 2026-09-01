//! Recursive walk of a directory: extension filter, guard against symbolic
//! link loops, cap.

use ritornello_i18n::Catalog;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Cap of a playlist. Protects three things at once: the JSON payload served
/// to the page, the writing of the m3u, and mpv's playback list.
pub const MAX_TRACKS: usize = 2000;

/// **Visit** cap of a search, distinct from `MAX_TRACKS`.
///
/// `MAX_TRACKS` caps what the playback list may hold; confusing it with the
/// cost of a walk made every search launched in a folder of more than 2000
/// tracks refused, with the add message — measured at the root of a NAS. A
/// search fills nothing: it only needs a bound that keeps it from running
/// forever, and exceeding it is reported as "truncated".
///
/// Now counts **every inspected entry** — folder or file, audio or not — and
/// no longer only the audio files met: a folder full of non-audio files was
/// bounded by nothing before this change. Raised accordingly: it is
/// [`SEARCH_TIMEOUT`] that now protects a slow share (the dominant cost there
/// is the `read_dir` per folder, not the entry count); this cap remains a
/// safety net for the local case, fast per entry, where only an outsized
/// number of entries must be refused.
pub const MAX_VISITS: usize = 500_000;

/// Maximum time granted to a search.
///
/// Well under the 5 s of the admin protocol, which is **serial**: past that
/// timeout, the page's polling `get_data` pile up behind the running search
/// and all expire — this is the failure mode of the 2026-08-17 incident, where
/// the page disappeared. The remaining margin covers the path resolution and
/// the serialization that follow the walk.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(3);

/// Number of inspected entries between two deadline measurements.
///
/// `Instant::elapsed` is not free: measuring it at every entry would add one
/// system call per file, on the hottest path of the walk.
const DEADLINE_STEP: usize = 64;

const EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "aiff", "ape", "wv", "mpc",
];

#[derive(Debug)]
pub enum ScanError {
    TooMany { cap: usize },
    Io { path: String },
}

pub fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.iter().any(|k| k.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Walks `dir` recursively and returns the audio files, **sorted**.
///
/// Sorting makes the add reproducible: without it, two adds of the same folder
/// would give different preset numbers from one day to the next, the order of
/// `read_dir` being guaranteed by no file system.
///
/// The anti-loop guard remembers the **canonicalized** directories already
/// visited: a symbolic link pointing to an ancestor would otherwise make the
/// walk spin until the cap, with a symptom that looks like a huge library
/// rather than a defect.
pub fn walk(dir: &Path, cap: usize) -> Result<Vec<PathBuf>, ScanError> {
    walk_with(dir, cap, &|_, _| {})
}

/// Same walk, with a **progress hook** called at every visited directory: the
/// number of tracks found so far, and the current directory.
///
/// It exists because a walk over a sleeping SMB share can take a long time,
/// and the admin protocol pushes nothing: the page polls, and it needs
/// something to show in the meantime. Without it, the user would see a frozen
/// screen without knowing whether anything is moving.
pub fn walk_with(
    dir: &Path,
    cap: usize,
    progress: &dyn Fn(usize, &Path),
) -> Result<Vec<PathBuf>, ScanError> {
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    walk_dir(dir, cap, &mut out, &mut seen, progress)?;
    out.sort();
    Ok(out)
}

/// Extensions of playlist files.
///
/// Kept apart from the audio extensions: an m3u is not added to the playlist,
/// it **replaces** it. Confusing them would add a text file that mpv would try
/// to play.
const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8"];

pub fn is_playlist(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| PLAYLIST_EXTENSIONS.iter().any(|k| k.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Contents of a single level, each category sorted.
///
/// A named structure rather than a triple: three anonymous `Vec<String>` get
/// swapped at the first refactor, and the error only shows on screen.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Contents {
    pub dirs: Vec<String>,
    pub audio: Vec<String>,
    /// Playlist files, which are **loaded** instead of being added.
    pub playlists: Vec<String>,
}

/// Contents of a single level: subdirectories, audio files and playlist files.
/// This is what the page's **lazy** tree consumes, which never asks for the
/// whole tree at once.
pub fn list_dir(dir: &Path) -> Result<Contents, ScanError> {
    let read =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut out = Contents::default();
    for entry in read.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        // Hidden entries are not shown: a library is full of them (`.DS_Store`,
        // a Synology's `@eaDir`) and they have no business in a music
        // navigation tree.
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            out.dirs.push(name);
        } else if meta.is_file() && is_audio(&path) {
            out.audio.push(name);
        } else if meta.is_file() && is_playlist(&path) {
            out.playlists.push(name);
        }
    }
    out.dirs.sort();
    out.audio.sort();
    out.playlists.sort();
    Ok(out)
}

/// Why a search stopped.
///
/// Two causes, two pieces of advice to give: too many matches invites
/// narrowing the pattern, an interrupted walk invites descending into a
/// subfolder. Confusing them showed "No results" — that is, "this file does
/// not exist" — to someone whose search had simply given up before reaching
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchEnd {
    /// The whole folder was walked.
    Complete,
    /// The result cap is reached: there were more.
    TooManyResults,
    /// The walk was interrupted before seeing everything.
    Interrupted,
}

/// Recursively searches the audio files whose name contains `pattern`
/// (case-insensitive comparison).
///
/// Two bounds, and two distinct reasons: `cap` limits what we **report** to
/// the page, `visit_cap` what we agree to **browse**. Either one returns a
/// distinct [`SearchEnd`], never a refusal: a partial list announced as such is
/// useful, a refusal is not.
///
/// The filter applies **during** the walk: collecting the whole folder first
/// to then keep only a handful of names was what made the search hit the
/// playback list cap.
pub fn search(
    dir: &Path,
    pattern: &str,
    cap: usize,
    visit_cap: usize,
    timeout: Duration,
) -> Result<(Vec<PathBuf>, SearchEnd), ScanError> {
    let pattern = pattern.to_lowercase();
    if pattern.is_empty() {
        return Ok((Vec::new(), SearchEnd::Complete));
    }
    let mut out = Vec::new();
    let mut visits = 0usize;
    let mut seen = HashSet::new();
    let start = Instant::now();
    // `cap + 1`: we search for one more than we return, to tell "exactly cap
    // results" from "there were more". Without that, a complete list of cap
    // elements would be announced as truncated.
    let stopped = walk_searching(
        dir,
        &pattern,
        cap + 1,
        visit_cap,
        start,
        timeout,
        &mut out,
        &mut visits,
        &mut seen,
    )?;
    out.truncate(cap);
    Ok((out, stopped.unwrap_or(SearchEnd::Complete)))
}

/// Filtering walk. Returns the cause of an early stop, `None` if the walk
/// covered the whole folder.
// Nine parameters: the last three are the recursion state, the first six its
// bounds. Grouping them in a struct would only add one more name to read —
// accepted as is.
#[allow(clippy::too_many_arguments)]
fn walk_searching(
    dir: &Path,
    pattern: &str,
    cap: usize,
    visit_cap: usize,
    start: Instant,
    timeout: Duration,
    out: &mut Vec<PathBuf>,
    visits: &mut usize,
    seen: &mut HashSet<PathBuf>,
) -> Result<Option<SearchEnd>, ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    // Same guard as `walk_dir`: a link pointing to an ancestor would make the
    // walk spin, producing ever longer paths.
    if !seen.insert(canon) {
        return Ok(None);
    }
    let read =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut subdirs = Vec::new();
    for entry in read {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // Every entry counts, folder or file, audio or not: a folder full of
        // non-audio files was bounded by nothing as long as only audio files
        // were counted.
        *visits += 1;
        if *visits > visit_cap {
            return Ok(Some(SearchEnd::Interrupted));
        }
        // Measured every `DEADLINE_STEP` entries, not at every entry:
        // `Instant::elapsed` is not free.
        if visits.is_multiple_of(DEADLINE_STEP) && start.elapsed() >= timeout {
            return Ok(Some(SearchEnd::Interrupted));
        }
        // `metadata` and not `symlink_metadata`, as in `walk_dir`: a link to a
        // real folder must be followed.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            subdirs.push(path);
            continue;
        }
        if !(meta.is_file() && is_audio(&path)) {
            continue;
        }
        let matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_lowercase().contains(pattern));
        if matches {
            out.push(path);
            if out.len() >= cap {
                return Ok(Some(SearchEnd::TooManyResults));
            }
        }
    }
    subdirs.sort();
    for d in subdirs {
        if let Some(reason) =
            walk_searching(&d, pattern, cap, visit_cap, start, timeout, out, visits, seen)?
        {
            return Ok(Some(reason));
        }
    }
    Ok(None)
}

fn walk_dir(
    dir: &Path,
    cap: usize,
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    progress: &dyn Fn(usize, &Path),
) -> Result<(), ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    if !seen.insert(canon) {
        return Ok(());
    }
    progress(out.len(), dir);
    let read =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut subdirs = Vec::new();
    for entry in read {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // `metadata` and not `symlink_metadata`: a link to a real folder must
        // be followed. It is the loop we refuse, not the link. A broken link
        // or a forbidden directory moves on to the next rather than failing: a
        // library is rarely perfect, and refusing the whole add for one
        // wonky file would be disproportionate.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            subdirs.push(path);
        } else if meta.is_file() && is_audio(&path) {
            if out.len() >= cap {
                return Err(ScanError::TooMany { cap });
            }
            out.push(path);
        }
    }
    subdirs.sort();
    for d in subdirs {
        walk_dir(&d, cap, out, seen, progress)?;
    }
    Ok(())
}

impl ScanError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ScanError::TooMany { cap } => {
                catalog.get("too_many_tracks").replace("{cap}", &cap.to_string())
            }
            ScanError::Io { path } => catalog.get("scan_io_error").replace("{path}", path),
        }
    }
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::TooMany { cap } => write!(f, "more than {cap} tracks"),
            ScanError::Io { path } => write!(f, "cannot read {path}"),
        }
    }
}

impl std::error::Error for ScanError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn only_audio_files_are_kept_whatever_the_case() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.mp3", "b.FLAC", "c.Opus", "cover.jpg", "notes.txt", "sans-extension"] {
            touch(dir.path(), name);
        }
        let mut names: Vec<String> = walk(dir.path(), MAX_TRACKS)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.mp3", "b.FLAC", "c.Opus"]);
    }

    #[test]
    fn a_single_level_separates_folders_tracks_and_playlists() {
        // Playlists travel apart because they carry a different action: they
        // **replace** the current playlist instead of being added to it.
        // Filing them with the tracks would add a text file that mpv would try
        // to play.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Album")).unwrap();
        for name in ["a.mp3", "tout.m3u", "autre.M3U8", "cover.jpg", ".cache.m3u"] {
            touch(dir.path(), name);
        }
        let c = list_dir(dir.path()).unwrap();
        assert_eq!(c.dirs, vec!["Album"]);
        assert_eq!(c.audio, vec!["a.mp3"]);
        // Case-insensitive, as for audio; the hidden entry stays out.
        assert_eq!(c.playlists, vec!["autre.M3U8", "tout.m3u"]);
    }

    #[test]
    fn an_m3u_is_not_an_audio_file() {
        // Safeguard of the separation above, on the predicates' side: a
        // recursive sweep must not pick up playlists as tracks.
        assert!(is_playlist(Path::new("x/tout.m3u")));
        assert!(is_playlist(Path::new("x/tout.M3U8")));
        assert!(!is_audio(Path::new("x/tout.m3u")));
        assert!(!is_playlist(Path::new("x/piste.mp3")));
    }

    #[test]
    fn the_walk_is_recursive_and_ordered() {
        // The order must be stable: two adds of the same folder produce the
        // same list, otherwise the preset numbers would change from one day to
        // the next.
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "A/02.mp3");
        touch(dir.path(), "A/01.mp3");
        touch(dir.path(), "B/sous/03.mp3");
        let relative: Vec<String> = walk(dir.path(), MAX_TRACKS)
            .unwrap()
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(relative, vec!["A/01.mp3", "A/02.mp3", "B/sous/03.mp3"]);
    }

    #[test]
    fn the_cap_is_refused_and_not_silently_truncated() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            touch(dir.path(), &format!("{i}.mp3"));
        }
        assert!(matches!(walk(dir.path(), 3), Err(ScanError::TooMany { cap: 3 })));
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_loop_does_not_make_the_walk_spin() {
        // Without a guard, a link pointing to an ancestor makes the walk spin
        // until the cap, producing ever longer paths. The symptom looks like a
        // huge library, not like a defect.
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "sous/a.mp3");
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sous/boucle")).unwrap();
        let found = walk(dir.path(), MAX_TRACKS).unwrap();
        assert_eq!(found.len(), 1, "the loop was followed: {found:?}");
    }

    #[test]
    fn a_nonexistent_directory_gives_a_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = walk(&dir.path().join("absent"), MAX_TRACKS).unwrap_err();
        assert!(matches!(err, ScanError::Io { .. }));
    }

    #[test]
    fn a_search_beyond_the_cap_truncates_instead_of_refusing() {
        // Symptom measured on a real NAS: searching at the root returned "this
        // folder holds more than 2000 tracks: narrow it down, or add its
        // subfolders one by one" — the ADD message — for a search that adds
        // nothing to the playlist. The cause: `search` reused `MAX_TRACKS`,
        // the playback list cap, as the walk cap. A search that is too broad
        // truncates and says so; it does not refuse.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            touch(dir.path(), &format!("{i}.mp3"));
        }
        let (found, end) = search(dir.path(), "mp3", 200, 3, SEARCH_TIMEOUT).expect("no refusal expected");
        assert_ne!(end, SearchEnd::Complete, "a reached cap must be reported");
        assert!(!found.is_empty(), "partial results are better than nothing");
    }

    #[test]
    fn a_search_interrupted_by_the_visit_cap_says_so() {
        // Defect found in review: the walk returned `Ok(true)` whether the cap
        // reached was the VISITS one or the RESULTS one, and the page then
        // showed "No results" — that is, "this file does not exist" — for a
        // search that had simply given up before reaching it. Here the visit
        // cap is reached well before the result one (200): the cause must be
        // told apart.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            touch(dir.path(), &format!("{i}.mp3"));
        }
        let (_, end) = search(dir.path(), "mp3", 200, 3, SEARCH_TIMEOUT).unwrap();
        assert_eq!(end, SearchEnd::Interrupted);
    }

    #[test]
    fn a_search_exceeding_the_result_cap_says_so() {
        // The other stop cause: here the visit cap is wide, only the result
        // one is at play.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            touch(dir.path(), &format!("miles{i}.mp3"));
        }
        let (found, end) = search(dir.path(), "miles", 3, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        assert_eq!(end, SearchEnd::TooManyResults);
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn a_search_that_walked_everything_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "A/miles.flac");
        let (_, end) = search(dir.path(), "miles", 200, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        assert_eq!(end, SearchEnd::Complete);
    }

    #[test]
    fn a_search_with_exactly_cap_results_is_complete() {
        // Regime not covered before the review, and yet the whole reason for
        // the `cap + 1`: without it, a complete list of `cap` elements would be
        // announced as truncated although it is exhaustive.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..3 {
            touch(dir.path(), &format!("miles{i}.mp3"));
        }
        let (found, end) = search(dir.path(), "miles", 3, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(end, SearchEnd::Complete);
    }

    #[test]
    fn a_search_with_more_than_cap_results_and_a_wide_visit_cap_is_truncated() {
        // The other regime not covered: the visit cap does not come into play,
        // only the result one must trigger.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            touch(dir.path(), &format!("miles{i}.mp3"));
        }
        let (found, end) = search(dir.path(), "miles", 3, 1_000_000, SEARCH_TIMEOUT).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(end, SearchEnd::TooManyResults);
    }

    #[test]
    fn the_search_timeout_is_well_under_the_protocol_cap() {
        // The admin protocol gives up on a request after 5 s; margin is needed
        // for the path resolution and the serialization that follow the walk,
        // otherwise the timeout itself would exceed the core's cap.
        assert!(SEARCH_TIMEOUT < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_search_exceeding_its_timeout_is_interrupted_without_waiting() {
        // The visit count does not protect a slow share: the cost there is
        // dominated by `read_dir`, not per entry. A zero timeout lets the
        // interruption be observed without making the test depend on a clock.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(DEADLINE_STEP * 2) {
            touch(dir.path(), &format!("{i}.mp3"));
        }
        let (_, end) = search(dir.path(), "mp3", 200, MAX_VISITS, Duration::ZERO).unwrap();
        assert_eq!(end, SearchEnd::Interrupted);
    }

    #[test]
    fn a_folder_full_of_non_audio_files_is_bounded_by_the_visit_cap() {
        // Defect fixed: `visits` only counted AUDIO files, so a folder full of
        // non-audio files was bounded by nothing — the walk could inspect an
        // arbitrary number of files without ever stopping on this cap.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            touch(dir.path(), &format!("{i}.txt"));
        }
        let (found, end) = search(dir.path(), "txt", 200, 3, SEARCH_TIMEOUT).unwrap();
        assert_eq!(end, SearchEnd::Interrupted);
        assert!(found.is_empty(), "no non-audio file must be reported");
    }

    // The two caps do not measure the same thing: `MAX_TRACKS` caps what can
    // be ADDED, `MAX_VISITS` what can be BROWSED while searching. Confusing
    // them is exactly the defect fixed here. Checked at compile time: a test on
    // two constants cannot fail at runtime, clippy rightly refuses it.
    const _: () = assert!(MAX_VISITS > MAX_TRACKS);

    #[test]
    fn a_search_returns_the_matches_and_only_them() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "A/miles.flac");
        touch(dir.path(), "A/autre.mp3");
        touch(dir.path(), "B/sous/MILES live.mp3");
        let (found, end) = search(dir.path(), "miles", 200, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        let mut relative: Vec<String> = found
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        relative.sort();
        // Case-insensitive, and on the file name alone.
        assert_eq!(relative, vec!["A/miles.flac", "B/sous/MILES live.mp3"]);
        assert_eq!(end, SearchEnd::Complete, "three files fill no cap");
    }

    #[test]
    fn every_refusal_resolves_against_the_embedded_catalog() {
        let catalog = Catalog::load("files", "en", Path::new("/inexistant"), crate::FILES_EN);
        for m in [
            ScanError::TooMany { cap: 2000 }.message(&catalog),
            ScanError::Io { path: "/mnt/ritornello/nas".into() }.message(&catalog),
        ] {
            assert!(m.contains(' '), "message reduced to a raw key: {m:?}");
        }
        let capped = ScanError::TooMany { cap: 2000 }.message(&catalog);
        assert!(capped.contains("2000"), "cap not interpolated: {capped:?}");
        assert!(!capped.contains("{cap}"), "token left as is: {capped:?}");
    }
}
