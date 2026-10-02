//! Reading a language pack archive, and stopping there.
//!
//! **A reader of its own, and that is the security property.** A component
//! archive is a tree to extract at `/`; a pack archive is a flat payload the
//! core interprets, and the two never meet. The component reader's
//! `installable_from_ui`, `only_its_own_binary` and `ETC_PREFIXES` therefore
//! keep the exact meaning they had before language packs existed -- none of
//! them is widened, and none of them is asked a question about a shape it
//! was not written for.
//!
//! Two bounds, the same pair the component reader draws for the same reason:
//! the download bounds the **compressed** size, this module bounds the
//! **decompressed** one, because gzip expands and this runs on a device with
//! a gigabyte of memory.

use std::io::Read;

use ritornello_i18n::{Layer, PackError, PackManifest, MAX_FILES};

/// The manifest this pack declares, the layers it resolves to, and the raw
/// bytes to write.
///
/// `files` is kept beside `layers` rather than re-serialised from them: what
/// lands on disk must be byte-for-byte what was published and verified, not
/// this core's idea of how to write the same map back out.
#[derive(Debug, Clone)]
pub struct PackContents {
    pub manifest: PackManifest,
    /// Read directly by this module's own tests (`c.layers[0]`, …); no
    /// production caller needs the parsed form yet, since `store::install`
    /// writes `files` verbatim and reserialises `manifest` — never `layers`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the parsed layers, kept for a caller this crate does not have yet; store::install writes the raw files instead")
    )]
    pub layers: Vec<(String, Layer)>,
    /// `(<module>.toml, bytes)`, in the same order as `layers`.
    pub files: Vec<(String, Vec<u8>)>,
}

/// The manifest's own name inside the archive.
const MANIFEST: &str = "pack.toml";

/// Tar's framing on top of the payload, the same slack the component reader
/// allows and for the same reason: headers and end-of-archive padding are
/// decompressed bytes too, and a budget of exactly the cap would refuse a
/// payload sitting exactly at it.
const FRAMING_SLACK: usize = 256 * 1024;

/// `cap` is a parameter, not `MAX_BYTES` read directly, for the same reason
/// the component reader's own `read` takes one: it lets a test drive the
/// decompression bound with a small budget instead of a multi-megabyte
/// fixture. Production passes `ritornello_i18n::MAX_BYTES`; nothing about
/// the real bound changes.
pub fn read(bytes: &[u8], cap: usize) -> Result<PackContents, PackError> {
    let budget = cap + FRAMING_SLACK;
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder.take(budget as u64 + 1));

    let mut manifest_text: Option<String> = None;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0usize;

    let entries = archive
        .entries()
        .map_err(|e| PackError::Manifest(format!("the archive could not be read: {e}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| PackError::Manifest(format!("the archive could not be read: {e}")))?;
        // A directory describes no content: tar writes them for free, and
        // judging an archive on them would refuse every real one.
        if entry.header().entry_type().is_dir() {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(PackError::BadEntry("a non-regular entry".to_string()));
        }
        let raw = entry.path_bytes().iter().copied().collect::<Vec<u8>>();
        let name = String::from_utf8(raw)
            .map_err(|_| PackError::BadEntry("a non-UTF-8 entry name".to_string()))?;
        let name = name.strip_prefix("./").unwrap_or(&name).to_string();

        if files.len() > MAX_FILES {
            return Err(PackError::TooManyFiles(MAX_FILES));
        }
        let mut body = Vec::new();
        entry
            .read_to_end(&mut body)
            .map_err(|e| PackError::Manifest(format!("the archive could not be read: {e}")))?;
        total = total.saturating_add(body.len());
        if total > cap {
            return Err(PackError::TooLarge(cap));
        }
        if name == MANIFEST {
            manifest_text = Some(
                String::from_utf8(body)
                    .map_err(|e| PackError::Manifest(format!("{MANIFEST}: {e}")))?,
            );
            continue;
        }
        files.push((name, body));
    }

    let Some(text) = manifest_text else {
        return Err(PackError::Manifest(format!("the archive carries no {MANIFEST}")));
    };
    let manifest = ritornello_i18n::parse_manifest(&text)?;
    let layers = ritornello_i18n::validate(&manifest, &files)?;
    // `validate` sorted by module name; put `files` in the same order so a
    // caller can zip the two without re-looking anything up. `files` is
    // already `mut` from its declaration above, so this needs no rebinding.
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(PackContents { manifest, layers, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a gzipped tar from `(name, bytes)` pairs, the same idiom the
    /// component reader's own tests use.
    fn targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, *body).unwrap();
        }
        let raw = tar.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap()
    }

    fn sound_manifest() -> &'static [u8] {
        b"language = \"fr\"\nversion = \"0.2.0-beta.2\"\nsource = \"https://github.com/skerdudou/ritornello\"\nmodules = [\"core\"]\n"
    }

    #[test]
    fn a_sound_pack_archive_is_read_into_its_manifest_and_its_layers() {
        let bytes = targz(&[
            ("pack.toml", sound_manifest()),
            ("core.toml", b"standby = \"VEILLE\"\n"),
        ]);
        let c = read(&bytes, ritornello_i18n::MAX_BYTES).expect("a sound archive");
        assert_eq!(c.manifest.language, "fr");
        assert_eq!(c.layers.len(), 1);
        assert_eq!(c.layers[0].0, "core");
        assert_eq!(c.layers[0].1.get("standby"), Some("VEILLE"));
        assert_eq!(c.files.len(), 1, "pack.toml is not one of the files to place");
    }

    /// The `./` prefix tar writes for a relative archive is stripped, exactly
    /// as the component reader's `read` does -- otherwise every real archive
    /// this repository produces would be refused as an unknown entry.
    #[test]
    fn a_leading_dot_slash_is_stripped() {
        let bytes = targz(&[("./pack.toml", sound_manifest()), ("./core.toml", b"k = \"v\"\n")]);
        assert!(read(&bytes, ritornello_i18n::MAX_BYTES).is_ok());
    }

    #[test]
    fn an_archive_with_no_manifest_is_refused() {
        let bytes = targz(&[("core.toml", b"k = \"v\"\n")]);
        assert!(matches!(read(&bytes, ritornello_i18n::MAX_BYTES), Err(PackError::Manifest(_))));
    }

    /// The shape that must never be mistaken for a component archive. A pack
    /// is flat; anything carrying a tree is refused before its manifest is
    /// even consulted.
    ///
    /// Asserts `BadEntry` specifically, not "`BadEntry` or `UndeclaredFile`".
    /// The looser assertion was tried first and proved nothing: `validate`
    /// checks an entry's `.toml` suffix before it ever consults the declared
    /// module list, and neither fixture below carries a `.toml`-suffixed
    /// name at all, so both are refused at the suffix check -- `UndeclaredFile`
    /// is never reachable from either one. Measured by instrumenting `read`
    /// and printing the returned variant before writing this assertion, not
    /// assumed: `Err(BadEntry("usr/local/lib/ritornello/plugins/ritornello-plugin-radio"))`
    /// and `Err(BadEntry("etc/systemd/system/ritornello.service"))`.
    ///
    /// That the two variants are genuinely distinct, reachable outcomes --
    /// and so this tightening is a real claim -- was checked separately with
    /// a throwaway probe entry ending in `.toml` but declaring no matching
    /// module (`"rogue.toml"` with `modules = ["core"]`): that one comes back
    /// `Err(UndeclaredFile("rogue.toml"))`, which the `BadEntry`-only form
    /// here does *not* match, and declaring `"rogue"` in the manifest then
    /// makes the archive sound outright (`Ok`). The probe was a temporary,
    /// uncommitted test and is not part of this suite.
    #[test]
    fn an_archive_shaped_like_a_component_is_refused() {
        let bytes = targz(&[
            ("pack.toml", sound_manifest()),
            ("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
        ]);
        assert!(matches!(read(&bytes, ritornello_i18n::MAX_BYTES), Err(PackError::BadEntry(_))));

        let bytes = targz(&[
            ("pack.toml", sound_manifest()),
            ("core.toml", b"k = \"v\"\n"),
            ("etc/systemd/system/ritornello.service", b"[Unit]\n"),
        ]);
        assert!(matches!(read(&bytes, ritornello_i18n::MAX_BYTES), Err(PackError::BadEntry(_))));
    }

    /// The decompression cap, measured as it is consumed rather than after
    /// the fact: a bomb must error instead of being allocated. With the
    /// production cap (four mebibytes) this entry is well inside the
    /// `take`-bounded decoder's own budget (cap + `FRAMING_SLACK`), so
    /// `take` never truncates anything here -- this test walks only the
    /// post-read `total > cap` counter. The sibling test below drives the
    /// same archive against a cap small enough that `take` truncates the
    /// read first, and reports what actually happens then.
    #[test]
    fn an_archive_that_decompresses_past_the_cap_is_refused() {
        let huge = vec![b'x'; ritornello_i18n::MAX_BYTES + 4096];
        let bytes = targz(&[("pack.toml", sound_manifest()), ("core.toml", &huge)]);
        assert!(matches!(read(&bytes, ritornello_i18n::MAX_BYTES), Err(PackError::TooLarge(_))));
    }

    /// A cap tiny enough that the `take`-bounded decoder, not the post-read
    /// counter, is what actually stops the read: `take`'s budget here is
    /// `16 + FRAMING_SLACK` (about 256 KiB), far below the several
    /// megabytes this archive decompresses to, so `entry.read_to_end` can
    /// never see the whole entry -- only what `take` lets through before it
    /// starts returning `Ok(0)`.
    ///
    /// **Measured, not assumed: this still returns `TooLarge` through the
    /// exact same `total > cap` line as the test above, not through a
    /// separate error path.** `std::io::Read::take` fails silently (a short
    /// read, never an `Err`) once its budget is spent, so `read_to_end`
    /// returns `Ok` with whatever partial bytes it managed to collect
    /// (verified by instrumenting `read` and printing the returned variant
    /// before writing this assertion) -- and that partial length is still
    /// well over the cap, so the *same* counter check downstream catches
    /// it. Unlike the component reader's `Bounded` wrapper, which turns a
    /// spent budget into its own distinct I/O error via a `tripped` flag,
    /// this reader has no such branch: `take` and the counter are not two
    /// independent paths to a refusal, they are one path where `take` only
    /// changes how many bytes get buffered before the counter runs. What
    /// this test proves that the one above does not is exactly that bound
    /// on memory, not a different kind of refusal.
    #[test]
    fn a_tiny_cap_bounds_memory_even_on_an_archive_comfortably_larger_than_it() {
        let huge = vec![b'x'; ritornello_i18n::MAX_BYTES + 4096];
        let bytes = targz(&[("pack.toml", sound_manifest()), ("core.toml", &huge)]);
        assert!(matches!(read(&bytes, 16), Err(PackError::TooLarge(16))));
    }

    #[test]
    fn something_that_is_not_a_gzipped_tar_is_refused_without_panicking() {
        assert!(matches!(read(b"<html>rate limited</html>", ritornello_i18n::MAX_BYTES), Err(PackError::Manifest(_) | PackError::BadEntry(_))));
    }

    /// Directory entries describe no content. A pack archive should carry
    /// none, but tar writes them for free and refusing on them would refuse
    /// archives that are otherwise sound.
    #[test]
    fn a_directory_entry_is_ignored_rather_than_refused() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut d = tar::Header::new_gnu();
        d.set_size(0);
        d.set_entry_type(tar::EntryType::Directory);
        d.set_mode(0o755);
        d.set_cksum();
        tar.append_data(&mut d, "./", &[][..]).unwrap();
        for (name, body) in [("pack.toml", sound_manifest()), ("core.toml", b"k = \"v\"\n" as &[u8])] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, name, body).unwrap();
        }
        let raw = tar.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        assert!(read(&enc.finish().unwrap(), ritornello_i18n::MAX_BYTES).is_ok());
    }

    /// The two readers must never learn about each other. Stated as a test
    /// over the source text because that is the only thing that stays true
    /// when someone reaches for the nearest helper: a pack reader that
    /// started calling the component reader would inherit the component
    /// rules, and a component reader that called this one would inherit the
    /// pack's.
    #[test]
    fn the_pack_reader_and_the_component_reader_never_call_each_other() {
        // Built at runtime from three pieces, not written as one contiguous
        // literal: this file's own source has to name the pattern to search
        // for, and spelling it out whole right here would make the search
        // match this very line, failing the test unconditionally regardless
        // of what the rest of the file says.
        let forbidden = ["update", "::", "archive"].concat();
        let here = include_str!("archive.rs");
        assert!(!here.contains(&forbidden), "the pack reader reaches into the component reader");
        let there = include_str!("../update/archive.rs");
        assert!(!there.contains("langpack"), "the component reader reaches into the pack reader");
    }
}
