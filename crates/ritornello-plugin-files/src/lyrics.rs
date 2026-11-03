//! The lyrics lying next to a track: `<track stem>.lrc`, read and announced,
//! and the lyrics found elsewhere kept there when the owner asked for it.
//!
//! The twin of `cover` and `archive`, at the granularity of a **track** rather
//! than a folder: a `.lrc` speaks for one file, so there is no homogeneity
//! question and no per-folder memory to keep.
//!
//! # What reading accepts
//!
//! A `.lrc` is written by hand and by every tagger that ever existed, so this
//! reads leniently: UTF-8 with or without a byte-order mark, and anything that
//! is not UTF-8 as Windows-1252 — the encoding of every Windows tool of the
//! 2000s, and one in which any byte sequence decodes, so the fallback never
//! fails (see `windows_1252`).
//! What it is then is decided by `ritornello_proto::parse_lrc`, the one LRC
//! parser of the project: timestamps make it `synced`, their absence `plain`.
//!
//! # What writing refuses
//!
//! Everything `archive` refuses for a cover, for the same reason — the file
//! lands in someone's own music library and stays there — minus the
//! homogeneity rule, which has no object here. Above all it never replaces a
//! `.lrc` that is there already, whatever its case: the owner's own lyrics,
//! timed by hand, are worth more than anything the network found. That rule
//! is the filesystem's to enforce, not a listing's (see `place`).

use crate::archive::Outcome;
use ritornello_plugin_files::roots::{RootKind, Roots};
use ritornello_proto::{parse_lrc, Lyrics, LYRICS_MAX_BYTES};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The extension a sidecar carries, compared case-insensitively: `.LRC` is as
/// common as `.lrc` on files that went through a Windows tool.
const EXTENSION: &str = "lrc";

/// The stem of the temporary a write goes through before its rename (see
/// `archive::temporary_name`): hidden, never read, and recognisable as ours if
/// a crash leaves it behind.
const TEMPORARY_STEM: &str = "lyrics";

/// Does `candidate` name the sidecar of `track`?
///
/// The **whole** name compared case-insensitively, the stem included, and the
/// writer uses this very predicate to decide a lyrics file "is already there".
/// On a share mounted case-insensitively, `01 - song.lrc` and `01 - Song.lrc`
/// are one file: a stem compared exactly would let the reader miss what the
/// writer then overwrites. One predicate keeps "what is read" and "what is not
/// written over" the same set, as `cover::names_a_cover` does for images.
fn names_the_sidecar(candidate: &Path, track: &Path) -> bool {
    let (Some(stem), Some(name)) = (track.file_stem(), candidate.file_name()) else {
        return false;
    };
    let wanted = format!("{}.{EXTENSION}", stem.to_string_lossy());
    name.to_string_lossy().eq_ignore_ascii_case(&wanted)
}

/// The `.lrc` beside `file`: same folder, same stem, extension in any case.
///
/// `Ok(None)` when there is none; `Err` when the folder cannot be listed,
/// which is not an absence — a share refusing a listing at once would
/// otherwise read as "no sidecar" and earn an offer to write beside lyrics
/// nobody has looked for.
///
/// Several candidates (`x.lrc` and `x.LRC` on a case-sensitive disk) are
/// settled by name, the exact-case one first, so the answer does not depend on
/// the order `read_dir` happens to return.
pub fn sidecar(file: &Path) -> std::io::Result<Option<PathBuf>> {
    let Some(dir) = file.parent() else {
        return Ok(None);
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
        .map(|e| e.path())
        .filter(|p| names_the_sidecar(p, file))
        .collect();
    let exact = file.with_extension(EXTENSION);
    found.sort_by_key(|p| (p != &exact, p.clone()));
    Ok(found.into_iter().next())
}

/// `bytes` as Windows-1252, the encoding Windows tools write when they do not
/// write UTF-8 — and not Latin-1, which it extends: a typographic apostrophe
/// (`Don’t`) is the byte 0x92 there, which Latin-1 turns into an invisible
/// control character. Every byte decodes: the five positions Windows-1252
/// leaves undefined (0x81, 0x8D, 0x8F, 0x90, 0x9D) keep their Latin-1 code
/// point, as browsers decode them, so this fallback never fails either.
fn windows_1252(bytes: &[u8]) -> String {
    /// 0x80 to 0x9F; `None` for the five undefined positions.
    const HIGH: [Option<char>; 32] = [
        Some('€'), None, Some('‚'), Some('ƒ'), Some('„'), Some('…'), Some('†'), Some('‡'),
        Some('ˆ'), Some('‰'), Some('Š'), Some('‹'), Some('Œ'), None, Some('Ž'), None,
        None, Some('‘'), Some('’'), Some('“'), Some('”'), Some('•'), Some('–'), Some('—'),
        Some('˜'), Some('™'), Some('š'), Some('›'), Some('œ'), None, Some('ž'), Some('Ÿ'),
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => HIGH[usize::from(b - 0x80)].unwrap_or(char::from(b)),
            _ => char::from(b),
        })
        .collect()
}

/// The lyrics held by the `.lrc` at `path`, or `None` when it holds none
/// that are worth showing (empty, unreadable, or over the ceiling).
///
/// Read through a `take` and not a `read_to_end`: a multi-gigabyte file named
/// `.lrc` by mistake must cost a few kilobytes, not the appliance's memory.
/// The ceiling is checked on the bytes, a byte-order mark aside; Windows-1252
/// can then only grow under UTF-8, and `validated` checks the result again.
pub fn read(path: &Path) -> Option<Lyrics> {
    const BOM: &[u8] = b"\xEF\xBB\xBF";
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take((LYRICS_MAX_BYTES + BOM.len() + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let bytes = bytes.strip_prefix(BOM).unwrap_or(&bytes);
    if bytes.len() > LYRICS_MAX_BYTES {
        return None;
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        // No crate, and no failure — which is the point of the fallback.
        Err(_) => windows_1252(bytes),
    };
    // One line ending, whatever the editor wrote: `parse_lrc` copes with CRLF
    // already, but the plain text is shown and archived as it stands.
    let text = text.replace("\r\n", "\n");
    let lyrics = if parse_lrc(&text).is_some() {
        Lyrics { plain: None, synced: Some(text) }
    } else {
        Lyrics { plain: Some(text), synced: None }
    };
    lyrics.validated()
}

/// What the sidecar lookup answers for `file`, in the three outcomes the
/// Source tells apart (see `FilesSource::lyrics_in_flight`), less the one only
/// the circuit breaker can give:
///
/// - `Some(None)` — no `.lrc` beside the track: the one case worth an offer;
/// - `Some(Some(l))` — found and readable;
/// - `None` — a `.lrc` is there but holds nothing worth showing. Not
///   `Some(None)`: an offer would only end in "a lyrics file is already
///   there", and the owner's file is not ours to judge. `None` too when the
///   folder could not be listed: unknown, not absent.
pub fn lookup(file: &Path) -> Option<Option<Lyrics>> {
    match sidecar(file) {
        Err(e) => {
            tracing::info!("{}: cannot list its folder: {e}", file.display());
            None
        }
        Ok(None) => Some(None),
        Ok(Some(path)) => match read(&path) {
            Some(lyrics) => Some(Some(lyrics)),
            None => {
                tracing::info!("{}: ignored, it holds no readable lyrics", path.display());
                None
            }
        },
    }
}

/// Keeps `lyrics` beside the track `identity` names, as `<track stem>.lrc`.
///
/// Refusals come in the same order as `archive::decide`'s and for the same
/// reason: the cheapest and most decisive first, the filesystem last, so a
/// refusal the table alone can pronounce never waits on a share.
pub fn store(roots: &Roots, identity: &serde_json::Value, lyrics: &Lyrics) -> Outcome {
    // 1. The echo, read and not trusted — the same reader as the cover's.
    let Some(played) = crate::archive::echoed_file(identity) else {
        return Outcome::Refused("the identity is not a file of this source");
    };
    // 2. The folder it designates.
    let Some(dir) = played.parent() else {
        return Outcome::Refused("the designated file has no parent directory");
    };
    // A parent but no stem is a path ending in `..`, which the echo already
    // refused; said with the echo's sentence should it ever get this far.
    let Some(stem) = played.file_stem() else {
        return Outcome::Refused("the identity is not a file of this source");
    };

    // 3. What the roots table says, without touching a filesystem.
    let Some(root) = roots.root_of(dir) else {
        return Outcome::Refused("no declared root owns this folder");
    };
    if !root.archive_lyrics {
        return Outcome::Refused("this root was never told to archive lyrics");
    }
    // `writable` only means something for a share; see `archive::decide`.
    if root.kind == RootKind::Smb && !root.writable {
        return Outcome::Refused("this share is mounted read-only");
    }

    // 4. What would be written. The lyrics crossed a process boundary, so
    //    they pass the one gate again rather than being trusted to have
    //    passed it: an empty or oversized text would otherwise land as a
    //    `.lrc` that the next listen reads as nothing, and that blocks every
    //    later attempt for good. Timed lyrics are what a `.lrc` is for, so
    //    `synced` wins when both are there.
    let Some(text) = lyrics.clone().validated().and_then(|l| l.synced.or(l.plain)) else {
        return Outcome::Refused("the lyrics are empty or over the ceiling");
    };

    // 5. Never overwrite: any `<stem>.lrc`, whatever its case (see
    //    `names_the_sidecar`). Only an early refusal, cheap and readable: the
    //    guarantee itself is `place`'s, since a listing can neither see a
    //    file that arrives after it nor fold case the way a share does.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect::<Vec<PathBuf>>(),
        Err(e) => return Outcome::Failed(format!("cannot list {}: {e}", dir.display())),
    };
    if entries.iter().any(|p| names_the_sidecar(p, &played)) {
        return Outcome::Refused(ALREADY_THERE);
    }

    // 6. UTF-8, whatever the sidecar this replaces the absence of would have
    //    been: it is what every reader expects today, and what `read` tries
    //    first.
    let target = dir.join(format!("{}.{EXTENSION}", stem.to_string_lossy()));
    place(dir, &target, &format!("{text}\n"))
}

/// The refusal an existing target earns, from the listing or from the
/// filesystem: one sentence for both, since they are one rule.
const ALREADY_THERE: &str = "a lyrics file is already there";

/// Writes `content` at `target` **only if nothing is there** — judged by the
/// filesystem, atomically, under its own case rules.
///
/// A temporary in the same folder (named as argued in
/// `archive::attempt_name`), then a hard link to the target, and not a
/// rename: a rename replaces what it lands on, so a `.lrc` the owner saved
/// between the listing and the rename would be lost, and a share folding `É`
/// to `é` sees a match the listing's ASCII folding cannot. A link fails with
/// `EEXIST` in both cases. Readers never see a half-written file.
///
/// Some shares refuse hard links (cifs without the Unix extensions answers
/// `EPERM` or `EOPNOTSUPP`): any link failure other than "already there" then
/// falls back to `create_exclusively`. Still never an overwrite; only the
/// atomicity of the content is lost.
fn place(dir: &Path, target: &Path, content: &str) -> Outcome {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temporary = dir.join(crate::archive::temporary_name(TEMPORARY_STEM, EXTENSION, stamp));
    if let Err(e) = std::fs::write(&temporary, content) {
        let _ = std::fs::remove_file(&temporary);
        return Outcome::Failed(format!("cannot write into {}: {e}", dir.display()));
    }
    let linked = std::fs::hard_link(&temporary, target);
    let _ = std::fs::remove_file(&temporary);
    match linked {
        Ok(()) => Outcome::Written(target.to_path_buf()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Outcome::Refused(ALREADY_THERE),
        Err(e) => {
            tracing::debug!("{}: no hard link ({e}), creating it exclusively", target.display());
            create_exclusively(target, content)
        }
    }
}

/// `place`'s fallback where hard links are refused: `create_new` is
/// `O_EXCL`, so an existing target is refused by the filesystem here too. A
/// failed write removes the file this very call created.
fn create_exclusively(target: &Path, content: &str) -> Outcome {
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(target) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Outcome::Refused(ALREADY_THERE),
        Err(e) => return Outcome::Failed(format!("cannot name {}: {e}", target.display())),
    };
    if let Err(e) = file.write_all(content.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(target);
        return Outcome::Failed(format!("cannot write {}: {e}", target.display()));
    }
    Outcome::Written(target.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_plugin_files::roots::Root;

    /// The echo the core sends, built exactly as `FilesSource::identity`
    /// builds it.
    fn identity(path: &Path) -> serde_json::Value {
        serde_json::json!({ "kind": "file", "path": path.to_string_lossy() })
    }

    /// A local root over `dir`, told to archive lyrics — and **not** covers,
    /// so no test below can pass on the cover's flag.
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
                archive_covers: false,
                archive_lyrics: true,
            }],
        }
    }

    /// A folder holding `names` as files with `content`.
    fn folder(names: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in names {
            std::fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    fn synced(text: &str) -> Lyrics {
        Lyrics { plain: None, synced: Some(text.into()) }
    }

    fn refusal(outcome: Outcome) -> &'static str {
        match outcome {
            Outcome::Refused(why) => why,
            other => panic!("expected a refusal: {other:?}"),
        }
    }

    const LRC: &str = "[00:01.00]Hello\n[00:02.50]World";

    #[test]
    fn a_sidecar_with_the_track_s_stem_is_found_whatever_the_extension_case() {
        for name in ["01 - Song.lrc", "01 - Song.LRC", "01 - Song.Lrc"] {
            let dir = folder(&[("01 - Song.flac", b""), (name, LRC.as_bytes())]);
            let track = dir.path().join("01 - Song.flac");
            assert_eq!(sidecar(&track).unwrap(), Some(dir.path().join(name)), "{name}");
        }
    }

    #[test]
    fn another_track_s_lrc_is_not_taken() {
        // An album folder holds one `.lrc` per track: the neighbour's, a
        // name that merely starts like ours, or the track's own name with
        // `.lrc` appended are all someone else's words.
        let dir = folder(&[
            ("01 - Song.flac", b""),
            ("02 - Other.lrc", LRC.as_bytes()),
            ("01 - Song (live).lrc", LRC.as_bytes()),
            ("01 - Song.flac.lrc", LRC.as_bytes()),
        ]);
        assert_eq!(sidecar(&dir.path().join("01 - Song.flac")).unwrap(), None);
        // The lookup's own answer for that: no sidecar, which is what allows
        // an offer.
        assert_eq!(lookup(&dir.path().join("01 - Song.flac")), Some(None));
    }

    #[test]
    fn a_dotted_stem_keeps_its_dots() {
        // `file_stem` drops the last extension only: "Vol. 2" must not be
        // matched as "Vol".
        let dir = folder(&[("Vol. 2.flac", b""), ("Vol. 2.lrc", LRC.as_bytes()), ("Vol.lrc", b"x")]);
        assert_eq!(sidecar(&dir.path().join("Vol. 2.flac")).unwrap(), Some(dir.path().join("Vol. 2.lrc")));
    }

    #[test]
    fn a_windows_lrc_is_read_as_windows_1252() {
        // What a Windows tagger of the 2000s writes: "Café" with é as the
        // single byte 0xE9, which is not UTF-8 — and the typographic
        // apostrophe, the œ ligature and the ellipsis in the 0x80-0x9F range
        // that Latin-1 would turn into invisible control characters.
        let dir = folder(&[(
            "t.lrc",
            b"[00:01.00]Caf\xe9 cr\xe8me\r\n[00:02.00]Don\x92t, c\x9cur\x85\r\n[00:03.00]Na\xefve",
        )]);
        let lyrics = read(&dir.path().join("t.lrc")).expect("Windows-1252 is lyrics too");
        let text = lyrics.synced.expect("timestamps make it synced");
        assert_eq!(text, "[00:01.00]Café crème\n[00:02.00]Don’t, cœur…\n[00:03.00]Naïve");
        assert_eq!(parse_lrc(&text).unwrap()[1].text, "Don’t, cœur…");
    }

    #[test]
    fn every_byte_decodes_and_the_undefined_ones_keep_their_latin1_point() {
        // The 27 defined positions of 0x80-0x9F, and the 5 undefined ones
        // mapped as browsers do: the fallback must never fail.
        let high: Vec<u8> = (0x80..=0x9F).collect();
        assert_eq!(
            windows_1252(&high),
            "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ\u{8D}Ž\u{8F}\u{90}‘’“”•–—˜™š›œ\u{9D}žŸ"
        );
        let low: Vec<u8> = (0x00..0x80).chain(0xA0..=0xFF).collect();
        let identity: String = low.iter().map(|&b| char::from(b)).collect();
        assert_eq!(windows_1252(&low), identity);
    }

    #[test]
    fn a_bom_is_not_part_of_the_first_line() {
        // A BOM left in would sit before the first `[`, and the first line
        // would not read as timed — or the plain text would start with an
        // invisible character.
        let dir = folder(&[("t.lrc", b"\xEF\xBB\xBF[00:01.00]Hello\r\n[00:02.00]World\r\n")]);
        let text = read(&dir.path().join("t.lrc")).unwrap().synced.unwrap();
        assert!(text.starts_with("[00:01.00]"), "{text:?}");
        assert!(!text.contains('\r'), "{text:?}");
        let lines = parse_lrc(&text).unwrap();
        assert_eq!(lines[0].text, "Hello");
        assert_eq!(lines.len(), 2);

        // And on plain text too: the BOM is not the first character.
        let dir = folder(&[("t.lrc", "\u{feff}Just words".as_bytes())]);
        assert_eq!(read(&dir.path().join("t.lrc")).unwrap().plain.as_deref(), Some("Just words"));
    }

    #[test]
    fn word_tags_and_an_offset_stay_in_the_synced_text_for_the_parser() {
        // Enhanced LRC and `[offset:]` are the parser's business: this module
        // keeps the file as it is, so the one parser decides once.
        let raw = "[ar:Daft Punk]\n[offset:+500]\n[00:12.34]<00:12.34>Like <00:12.80>the legend";
        let dir = folder(&[("t.lrc", raw.as_bytes())]);
        let text = read(&dir.path().join("t.lrc")).unwrap().synced.unwrap();
        assert_eq!(text, raw);
        let lines = parse_lrc(&text).unwrap();
        assert_eq!(lines[0].text, "Like the legend");
    }

    #[test]
    fn an_lrc_with_only_headers_becomes_plain_text() {
        // No timestamp at all: not synced lyrics, but not nothing either —
        // the plain-text fallback is what `validated` would have chosen.
        let dir = folder(&[("t.lrc", b"[ar:Daft Punk]\r\n[ti:Get Lucky]\r\n")]);
        let lyrics = read(&dir.path().join("t.lrc")).unwrap();
        assert_eq!(lyrics.synced, None);
        assert_eq!(lyrics.plain.as_deref(), Some("[ar:Daft Punk]\n[ti:Get Lucky]"));
    }

    #[test]
    fn an_empty_or_oversized_lrc_holds_no_lyrics() {
        let big = vec![b'a'; LYRICS_MAX_BYTES + 1];
        let dir = folder(&[("empty.lrc", b"  \r\n"), ("big.lrc", &big)]);
        assert_eq!(read(&dir.path().join("empty.lrc")), None);
        assert_eq!(read(&dir.path().join("big.lrc")), None);
        // Exactly at the ceiling, a BOM ahead of it: still read.
        let mut at = b"\xEF\xBB\xBF".to_vec();
        at.extend(vec![b'a'; LYRICS_MAX_BYTES]);
        let dir = folder(&[("at.lrc", &at)]);
        assert!(read(&dir.path().join("at.lrc")).is_some());
    }

    #[test]
    fn an_unreadable_sidecar_is_neither_lyrics_nor_an_absence() {
        // A `.lrc` holding nothing is the owner's file all the same: no
        // offer, since the write would only be refused as "already there".
        let dir = folder(&[("01.flac", b""), ("01.lrc", b"")]);
        assert_eq!(lookup(&dir.path().join("01.flac")), None);
        let dir = folder(&[("01.flac", b""), ("01.lrc", LRC.as_bytes())]);
        assert_eq!(lookup(&dir.path().join("01.flac")), Some(Some(synced(LRC))));
    }

    #[test]
    fn found_lyrics_land_beside_the_track_as_lrc() {
        let dir = folder(&[("01 - Song.flac", b"")]);
        let track = dir.path().join("01 - Song.flac");
        let both = Lyrics { plain: Some("Hello\nWorld".into()), synced: Some(LRC.into()) };
        match store(&local_table(dir.path()), &identity(&track), &both) {
            Outcome::Written(p) => assert_eq!(p, dir.path().join("01 - Song.lrc")),
            other => panic!("expected a write: {other:?}"),
        }
        // The timed form is the one kept, in UTF-8, and the next lookup of
        // this track finds exactly it.
        assert_eq!(lookup(&track), Some(Some(synced(LRC))));
        // Nothing of the attempt is left beside it.
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["01 - Song.flac", "01 - Song.lrc"]);
    }

    #[test]
    fn plain_lyrics_are_kept_when_nothing_is_timed() {
        let dir = folder(&[("01.flac", b"")]);
        let track = dir.path().join("01.flac");
        let plain = Lyrics { plain: Some("Bonjour à tous".into()), synced: None };
        assert!(matches!(store(&local_table(dir.path()), &identity(&track), &plain), Outcome::Written(_)));
        assert_eq!(std::fs::read_to_string(dir.path().join("01.lrc")).unwrap(), "Bonjour à tous\n");
        assert_eq!(lookup(&track), Some(Some(plain)));
    }

    #[test]
    fn an_existing_lrc_is_never_overwritten_whatever_its_case() {
        // The stem's case too: on a share mounted case-insensitively these
        // are one file with the target, and the rename would replace it.
        for existing in ["01 - Song.lrc", "01 - Song.LRC", "01 - SONG.lrc"] {
            let dir = folder(&[("01 - Song.flac", b""), (existing, b"the owner's own timing")]);
            let track = dir.path().join("01 - Song.flac");
            assert_eq!(
                refusal(store(&local_table(dir.path()), &identity(&track), &synced(LRC))),
                "a lyrics file is already there",
                "{existing}"
            );
            assert_eq!(std::fs::read(dir.path().join(existing)).unwrap(), b"the owner's own timing");
        }
    }

    #[test]
    fn a_lyrics_file_that_appears_after_the_listing_is_never_overwritten() {
        // The race the listing cannot see: the owner saves `01.lrc` once the
        // check has passed. The write step finds it in place and must refuse.
        let dir = folder(&[("01.flac", b"")]);
        let target = dir.path().join("01.lrc");
        std::fs::write(&target, b"the owner's own timing").unwrap();
        assert_eq!(refusal(place(dir.path(), &target, "[00:01.00]Found\n")), "a lyrics file is already there");
        assert_eq!(std::fs::read(&target).unwrap(), b"the owner's own timing");
        // Nothing of the attempt is left beside it.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn the_fallback_without_hard_links_never_overwrites_either() {
        let dir = folder(&[("01.lrc", b"the owner's own timing")]);
        let target = dir.path().join("01.lrc");
        assert_eq!(refusal(create_exclusively(&target, "[00:01.00]Found\n")), "a lyrics file is already there");
        assert_eq!(std::fs::read(&target).unwrap(), b"the owner's own timing");
        // And where nothing is there, it writes.
        let fresh = dir.path().join("02.lrc");
        assert!(matches!(create_exclusively(&fresh, "[00:01.00]Found\n"), Outcome::Written(_)));
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "[00:01.00]Found\n");
    }

    #[test]
    #[cfg(unix)]
    fn a_folder_that_cannot_be_listed_is_unknown_not_absent() {
        // A share that refuses the listing at once: no offer may follow, so
        // the answer is "unknown" and not "no sidecar".
        use std::os::unix::fs::PermissionsExt;
        let dir = folder(&[]);
        // A folder gone from under the track fails the listing too.
        assert_eq!(lookup(&dir.path().join("Nowhere").join("01.flac")), None);
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o300)).unwrap();
        let listable = std::fs::read_dir(&locked).is_ok();
        let answer = lookup(&locked.join("01.flac"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if listable {
            // Running as root: permissions do not bite, and the case cannot
            // be produced this way.
            eprintln!("skipped: the folder stayed listable");
            return;
        }
        assert_eq!(answer, None);
    }

    #[test]
    fn a_root_that_was_never_told_to_archive_lyrics_is_refused() {
        // Covers allowed, lyrics not: the two flags are not one.
        let dir = folder(&[("01.flac", b"")]);
        let mut table = local_table(dir.path());
        table.root[0].archive_lyrics = false;
        table.root[0].archive_covers = true;
        assert_eq!(
            refusal(store(&table, &identity(&dir.path().join("01.flac")), &synced(LRC))),
            "this root was never told to archive lyrics"
        );
        assert!(!dir.path().join("01.lrc").exists());
    }

    #[test]
    fn a_read_only_share_is_refused_before_the_write_is_tried() {
        // The folder does not exist: the table alone pronounces the refusal,
        // so nothing was tried and nothing was even listed.
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
                archive_covers: false,
                archive_lyrics: true,
            }],
        };
        let played = Path::new("/mnt/ritornello/nas/Kind of Blue/01.flac");
        assert_eq!(refusal(store(&table, &identity(played), &synced(LRC))), "this share is mounted read-only");

        // The same share made writable gets past the table and fails on the
        // missing folder: the refusal above was the flag's, not the path's.
        let mut writable = table.clone();
        writable.root[0].writable = true;
        assert!(matches!(store(&writable, &identity(played), &synced(LRC)), Outcome::Failed(_)));
    }

    #[test]
    fn a_traversal_is_refused_even_when_it_lands_inside_a_root() {
        let dir = folder(&[("01.flac", b"")]);
        let traversal = dir.path().join("sub").join("..").join("01.flac");
        assert_eq!(
            refusal(store(&local_table(dir.path()), &identity(&traversal), &synced(LRC))),
            "the identity is not a file of this source"
        );
        assert!(!dir.path().join("01.lrc").exists());
    }

    #[test]
    fn an_identity_that_is_not_a_file_of_ours_is_refused() {
        let dir = folder(&[("01.flac", b"")]);
        for foreign in [
            serde_json::json!({ "kind": "radio", "url": "https://x" }),
            serde_json::json!({ "kind": "file" }),
            serde_json::json!("bare string"),
        ] {
            assert_eq!(
                refusal(store(&local_table(dir.path()), &foreign, &synced(LRC))),
                "the identity is not a file of this source",
                "{foreign}"
            );
        }
    }

    #[test]
    fn a_file_without_a_parent_is_refused() {
        let dir = folder(&[]);
        assert_eq!(
            refusal(store(&local_table(dir.path()), &identity(Path::new("/")), &synced(LRC))),
            "the designated file has no parent directory"
        );
    }

    #[test]
    fn a_folder_owned_by_no_root_is_refused() {
        // A root armed for lyrics exists — over another folder. The table is
        // the only authority on where this plugin may write.
        let dir = folder(&[("01.flac", b"")]);
        let elsewhere = folder(&[]);
        assert_eq!(
            refusal(store(&local_table(elsewhere.path()), &identity(&dir.path().join("01.flac")), &synced(LRC))),
            "no declared root owns this folder"
        );
        assert!(!dir.path().join("01.lrc").exists());
    }

    #[test]
    fn empty_lyrics_are_never_written() {
        // A `.lrc` holding nothing would read back as no lyrics, and would
        // block every later attempt as "already there".
        let dir = folder(&[("01.flac", b"")]);
        let empty = Lyrics { plain: Some("  ".into()), synced: None };
        assert_eq!(
            refusal(store(&local_table(dir.path()), &identity(&dir.path().join("01.flac")), &empty)),
            "the lyrics are empty or over the ceiling"
        );
        assert!(!dir.path().join("01.lrc").exists());
    }

    #[test]
    fn a_vanished_folder_is_a_failure_not_a_refusal() {
        let dir = folder(&[]);
        let played = dir.path().join("Nowhere").join("01.flac");
        match store(&local_table(dir.path()), &identity(&played), &synced(LRC)) {
            Outcome::Failed(why) => assert!(why.starts_with("cannot list "), "{why}"),
            other => panic!("expected a failure: {other:?}"),
        }
    }

    #[test]
    fn a_leftover_temporary_is_invisible_to_the_lookup_and_to_the_overwrite_check() {
        // What a crash between the write and the rename leaves behind.
        let dir = folder(&[("01.flac", b"")]);
        let leftover = dir.path().join(crate::archive::temporary_name(TEMPORARY_STEM, EXTENSION, 1));
        std::fs::write(&leftover, LRC).unwrap();
        let track = dir.path().join("01.flac");
        assert_eq!(lookup(&track), Some(None));
        assert!(matches!(store(&local_table(dir.path()), &identity(&track), &synced(LRC)), Outcome::Written(_)));
    }
}
