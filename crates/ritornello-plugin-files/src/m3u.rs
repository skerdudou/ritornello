//! Reading and writing m3u playlists.
//!
//! Two distinct objects pass through here, and confusing them would be a
//! mistake: the **user playlist** (edited, saved, reloadable, with relative
//! paths when possible) and the **playlist given to mpv** (generated, with
//! absolute paths, never shown).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub title: Option<String>,
    pub duration_s: Option<u32>,
}

impl Entry {
    /// Displayable name: the `#EXTINF` title if it exists, otherwise the file
    /// name without extension.
    ///
    /// This is what the Source declares as `preset_name`, so that the screen is
    /// **never silent** even without any metadata: the tags only enrich on top.
    pub fn display_name(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// Entries no rule managed to resolve. **Reported**, never silently
    /// dropped: a playlist that shrinks without saying anything is a defect
    /// that takes months to attribute.
    pub unresolved: Vec<String>,
}

/// Resolves a raw entry.
///
/// Three rules, in this order. An m3u written by the NAS often carries paths
/// that only make sense on it (`Z:\Musique\…`, `/volume1/music/…`, a UNC path):
/// the third rule is there to catch them rather than discard the entry.
fn resolve(raw: &str, m3u_dir: &Path, root: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = raw.replace('\\', "/");

    // 1. relative to the m3u directory — the normal case, and the one we write.
    let rel = m3u_dir.join(&normalized);
    if rel.is_file() {
        return Some(rel);
    }

    // 2. absolute as is, if it designates something here.
    let abs = Path::new(&normalized);
    if abs.is_absolute() && abs.is_file() {
        return Some(abs.to_path_buf());
    }

    // 3. path from another system: we strip a drive prefix (`Z:`), then try
    //    the successive suffixes under the root, from longest to shortest —
    //    `Musique/Album/02.mp3`, then `Album/02.mp3`, then `02.mp3`. The first
    //    one that exists wins.
    let without_drive = match normalized.find(':') {
        Some(i) if i <= 2 => &normalized[i + 1..],
        _ => normalized.as_str(),
    };
    let segments: Vec<&str> = without_drive.split('/').filter(|s| !s.is_empty()).collect();
    for start in 0..segments.len() {
        let candidate = root.join(segments[start..].join("/"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Parses an m3u. `m3u_dir` is the directory of the file read, `root` the root
/// under which to catch foreign paths.
pub fn parse(text: &str, m3u_dir: &Path, root: &Path) -> Parsed {
    let mut out = Parsed::default();
    let mut pending: Option<(Option<u32>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = Some(match rest.split_once(',') {
                Some((d, t)) => (
                    // `-1` is the "unknown duration" convention: it must not
                    // become a duration.
                    d.trim().parse::<i64>().ok().filter(|n| *n > 0).map(|n| n as u32),
                    (!t.trim().is_empty()).then(|| t.trim().to_string()),
                ),
                None => (None, None),
            });
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (duration, title) = pending.take().unwrap_or((None, None));
        match resolve(line, m3u_dir, root) {
            Some(path) => out.entries.push(Entry { path, title, duration_s: duration }),
            None => out.unresolved.push(line.to_string()),
        }
    }
    out
}

/// Renders an m3u.
///
/// With a `base`, the paths are **relative** to it: this is what makes the
/// playlist re-readable by another player and able to survive a change of
/// mount point. Without a base, they are absolute — the form of the playlist
/// meant for mpv, which must depend on no current directory.
pub fn render(entries: &[Entry], base: Option<&Path>) -> String {
    let mut s = String::from("#EXTM3U\n");
    for e in entries {
        let duration = e.duration_s.map(|d| d.to_string()).unwrap_or_else(|| "-1".into());
        s.push_str(&format!("#EXTINF:{duration},{}\n", e.display_name()));
        let path = base
            .and_then(|b| e.path.strip_prefix(b).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| e.path.to_string_lossy().into_owned());
        s.push_str(&path);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, rel: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
        p
    }

    #[test]
    fn a_relative_m3u_resolves_against_the_file_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = file(dir.path(), "Album/01.mp3");
        let text = "#EXTM3U\n#EXTINF:245,Miles Davis - So What\nAlbum/01.mp3\n";
        let p = parse(text, dir.path(), dir.path());
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].path, target);
        assert_eq!(p.entries[0].title.as_deref(), Some("Miles Davis - So What"));
        assert_eq!(p.entries[0].duration_s, Some(245));
        assert!(p.unresolved.is_empty());
    }

    #[test]
    fn a_windows_path_written_by_the_nas_is_caught_under_the_root() {
        // An m3u produced by the NAS carries paths that only make sense on it.
        // We strip the drive prefix and try the successive suffixes under the
        // root, rather than discarding the entry.
        let dir = tempfile::tempdir().unwrap();
        let target = file(dir.path(), "Musique/Album/02.mp3");
        let p = parse("#EXTM3U\nZ:\\Musique\\Album\\02.mp3\n", dir.path(), dir.path());
        assert_eq!(p.entries.len(), 1, "unresolved: {:?}", p.unresolved);
        assert_eq!(p.entries[0].path, target);
    }

    #[test]
    fn a_foreign_absolute_path_is_caught_by_its_suffix() {
        // The Synology case: /volume1/music/... does not exist here, but
        // "Album/03.mp3" is indeed under the root.
        let dir = tempfile::tempdir().unwrap();
        let target = file(dir.path(), "Album/03.mp3");
        let p = parse("#EXTM3U\n/volume1/music/Album/03.mp3\n", dir.path(), dir.path());
        assert_eq!(p.entries.len(), 1, "unresolved: {:?}", p.unresolved);
        assert_eq!(p.entries[0].path, target);
    }

    #[test]
    fn an_entry_not_found_is_reported_and_not_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse("#EXTM3U\n/volume1/music/absent.mp3\n", dir.path(), dir.path());
        assert!(p.entries.is_empty());
        assert_eq!(p.unresolved, vec!["/volume1/music/absent.mp3".to_string()]);
    }

    #[test]
    fn comments_empty_lines_and_an_orphan_extinf_are_handled() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse("#EXTM3U\n\n# a comment\n#EXTINF:12,Orphan\n\n", dir.path(), dir.path());
        assert!(p.entries.is_empty());
        assert!(p.unresolved.is_empty());
    }

    #[test]
    fn an_unknown_duration_does_not_become_a_duration() {
        // `-1` is the m3u convention for "I do not know": taking it for a
        // duration would show "-1 s" somewhere.
        let dir = tempfile::tempdir().unwrap();
        file(dir.path(), "a.mp3");
        let p = parse("#EXTM3U\n#EXTINF:-1,No duration\na.mp3\n", dir.path(), dir.path());
        assert_eq!(p.entries[0].duration_s, None);
        assert_eq!(p.entries[0].title.as_deref(), Some("No duration"));
    }

    #[test]
    fn the_rendering_is_relative_when_a_base_is_given() {
        let base = Path::new("/mnt/ritornello/nas");
        let entries = vec![Entry {
            path: base.join("Album/01.mp3"),
            title: Some("So What".into()),
            duration_s: Some(245),
        }];
        assert_eq!(render(&entries, Some(base)), "#EXTM3U\n#EXTINF:245,So What\nAlbum/01.mp3\n");
    }

    #[test]
    fn the_rendering_is_absolute_without_a_base_and_names_the_file_by_default() {
        let entries = vec![Entry {
            path: "/mnt/ritornello/nas/Album/01.mp3".into(),
            title: None,
            duration_s: None,
        }];
        assert_eq!(
            render(&entries, None),
            "#EXTM3U\n#EXTINF:-1,01\n/mnt/ritornello/nas/Album/01.mp3\n"
        );
    }

    #[test]
    fn writing_then_rereading_keeps_titles_and_durations() {
        // The real round trip: what we save must come back identical.
        let dir = tempfile::tempdir().unwrap();
        let a = file(dir.path(), "Album/01.mp3");
        let b = file(dir.path(), "Album/02.mp3");
        let entries = vec![
            Entry { path: a, title: Some("So What".into()), duration_s: Some(545) },
            Entry { path: b, title: Some("Blue in Green".into()), duration_s: None },
        ];
        let text = render(&entries, Some(dir.path()));
        let reread = parse(&text, dir.path(), dir.path());
        assert_eq!(reread.entries, entries);
        assert!(reread.unresolved.is_empty());
    }
}
