//! What a language pack is, and what makes one acceptable.
//!
//! **Pure on purpose, and separate from every other archive rule in this
//! repository.** A language pack carries text and nothing else, so it is
//! read by its own reader (`ritornello_core::langpack::archive`) and judged
//! by its own rules -- the ones below. `update::archive`'s
//! `installable_from_ui` and `only_its_own_binary` keep the exact meaning
//! they had: neither is widened, neither is consulted here, and a pack can
//! never reach the privileged installer at all because it carries no binary
//! to place.
//!
//! Every rule here applies to **our own packs too**. A guard that only ever
//! judges a stranger proves nothing about the day our own packaging is
//! wrong.

use std::collections::HashMap;

use crate::Layer;

/// At most this many module files in one pack. The largest pack this
/// repository produces covers eight modules; sixty-four leaves room for a
/// plugin ecosystem and still refuses a pack whose file count alone is an
/// attack.
pub const MAX_FILES: usize = 64;

/// At most this many decompressed bytes across every file of one pack. The
/// core's own French catalogue is about twenty kilobytes, so four mebibytes
/// is two orders of magnitude of headroom -- and still finite, which is the
/// property that matters. Distinct from the download cap, which bounds the
/// *compressed* size: gzip expands.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// A pack's own description of itself, `pack.toml`.
///
/// `deny_unknown_fields`, deliberately: a field this core does not know is a
/// pack built for something this core does not implement, and writing its
/// files anyway would be acting on half a contract.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    /// The one language this pack carries. Every file in it is this
    /// language's; see `validate`.
    pub language: String,
    /// The pack's own version, compared by equality like every component's.
    pub version: String,
    /// The repository that published it, verbatim.
    pub source: String,
    /// The modules this pack covers. Load-bearing rather than decorative:
    /// `validate` refuses a file nobody declared and a declaration with no
    /// file, and the installed copy of this list is what makes a later
    /// removal exact.
    pub modules: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    Manifest(String),
    BadLanguage(String),
    BadModule(String),
    UndeclaredFile(String),
    MissingFile(String),
    BadEntry(String),
    BadContent(String),
    TooManyFiles(usize),
    TooLarge(usize),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest(d) => write!(f, "the pack manifest could not be read: {d}"),
            Self::BadLanguage(l) => write!(f, "refusing the language {l:?}: it is not a bare code"),
            Self::BadModule(m) => write!(f, "refusing the module name {m:?}: it is not a bare name"),
            Self::UndeclaredFile(n) => write!(f, "the pack carries {n:?}, which its manifest does not declare"),
            Self::MissingFile(m) => write!(f, "the pack declares the module {m:?} and carries no file for it"),
            Self::BadEntry(n) => write!(f, "refusing the entry {n:?}: a pack carries only <module>.toml files"),
            Self::BadContent(d) => write!(f, "a pack file is not a flat table of text: {d}"),
            Self::TooManyFiles(max) => write!(f, "the pack carries more than {max} files"),
            Self::TooLarge(max) => write!(f, "the pack decompresses to more than {max} bytes"),
        }
    }
}

impl std::error::Error for PackError {}

/// A bare name: what a module directory and a pack identifier may be called.
///
/// The same alphabet the privileged side accepts for a plugin file name, and
/// for the same reason -- a name that cannot contain a separator, a dot run
/// or an absolute root cannot become a path anywhere else.
pub fn valid_pack_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// A language code: non-empty, at most sixteen characters, alphanumeric plus
/// `-` and `_`.
///
/// Deliberately the same rule `ritornello_core::status::locales::valid_locale`
/// already applies to what the browser may ask for, restated here because
/// this crate cannot depend on that one. A regionalised code (`pt-BR`) passes;
/// anything carrying a separator or a dot does not.
fn valid_language(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn parse_manifest(text: &str) -> Result<PackManifest, PackError> {
    toml::from_str(text).map_err(|e| PackError::Manifest(e.to_string()))
}

/// Judges a pack whole, and refuses it whole.
///
/// Returns one `(module, Layer)` per declared module, sorted by module name
/// so the result is deterministic for a caller and a test alike. Never a
/// partial answer: the first refusal ends it, because installing the
/// acceptable half of an archive is how a device ends up in a state nobody
/// designed.
pub fn validate(
    manifest: &PackManifest,
    files: &[(String, Vec<u8>)],
) -> Result<Vec<(String, Layer)>, PackError> {
    if !valid_language(&manifest.language) {
        return Err(PackError::BadLanguage(manifest.language.clone()));
    }
    if manifest.modules.len() > MAX_FILES || files.len() > MAX_FILES {
        return Err(PackError::TooManyFiles(MAX_FILES));
    }
    let total: usize = files.iter().map(|(_, b)| b.len()).sum();
    if total > MAX_BYTES {
        return Err(PackError::TooLarge(MAX_BYTES));
    }
    for module in &manifest.modules {
        if !valid_pack_name(module) {
            return Err(PackError::BadModule(module.clone()));
        }
    }
    let declared: std::collections::HashSet<&str> =
        manifest.modules.iter().map(String::as_str).collect();

    let mut seen: HashMap<&str, &[u8]> = HashMap::new();
    for (name, bytes) in files {
        let Some(module) = name.strip_suffix(".toml") else {
            return Err(PackError::BadEntry(name.clone()));
        };
        if !valid_pack_name(module) {
            return Err(PackError::BadEntry(name.clone()));
        }
        if !declared.contains(module) {
            return Err(PackError::UndeclaredFile(name.clone()));
        }
        seen.insert(module, bytes.as_slice());
    }

    let mut modules: Vec<&str> = manifest.modules.iter().map(String::as_str).collect();
    modules.sort_unstable();
    let mut out = Vec::with_capacity(modules.len());
    for module in modules {
        let Some(bytes) = seen.get(module) else {
            return Err(PackError::MissingFile(module.to_string()));
        };
        let text = std::str::from_utf8(bytes)
            .map_err(|e| PackError::BadContent(format!("{module}.toml: {e}")))?;
        let layer = Layer::parse(text)
            .map_err(|e| PackError::BadContent(format!("{module}.toml: {e}")))?;
        out.push((module.to_string(), layer));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(language: &str, modules: &[&str]) -> PackManifest {
        PackManifest {
            language: language.to_string(),
            version: "0.2.0-beta.2".to_string(),
            source: "https://github.com/skerdudou/ritornello".to_string(),
            modules: modules.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn file(name: &str, body: &str) -> (String, Vec<u8>) {
        (name.to_string(), body.as_bytes().to_vec())
    }

    #[test]
    fn a_well_formed_pack_yields_one_layer_per_declared_module() {
        let m = manifest("fr", &["core", "radio"]);
        let files = vec![
            file("core.toml", "standby = \"VEILLE\"\n"),
            file("radio.toml", "stations = \"Stations\"\n"),
        ];
        let layers = validate(&m, &files).expect("a sound pack");
        let names: Vec<&str> = layers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["core", "radio"], "sorted, so the result is deterministic");
        assert_eq!(layers[0].1.get("standby"), Some("VEILLE"));
    }

    /// Refusal 1: an entry that is not `<module>.toml`. A path, a traversal,
    /// a dotfile and a binary are all the same refusal, and each is listed
    /// because each is a shape a hostile archive actually takes.
    ///
    /// Asserts `BadEntry` specifically, not "`BadEntry` or `UndeclaredFile`".
    /// The looser assertion was tried first and proved nothing: with the
    /// declared-module list pre-validated, every one of these names also
    /// fails `declared.contains`, so `UndeclaredFile` alone would have
    /// refused all seven -- the whole point of `valid_pack_name` inside this
    /// loop (as opposed to leaving that job to `declared.contains`) is
    /// invisible unless the test pins the exact variant. Found by mutation:
    /// forcing `valid_pack_name(module)` to `true` here left this test
    /// green while it still refused every case, just via `UndeclaredFile`.
    ///
    /// `"core"` is the eighth entry, added after review: every other input
    /// here contains a `.` or a `/`, so `valid_pack_name` refuses all seven
    /// regardless of whether `.toml` was stripped first -- this test's own
    /// job, "prove every shape of bad entry is caught", was not actually
    /// proven for the shape `strip_suffix(".toml")` exists to produce. A
    /// bare name with no `.toml` suffix at all is the one input that
    /// distinguishes stripping from not stripping: mutating
    /// `strip_suffix(".toml")` to `Some(name)` makes `"core"` pass
    /// `valid_pack_name` unchanged, match the declared module `"core"`
    /// exactly, and be wrongly accepted -- caught only by this case.
    #[test]
    fn an_entry_that_is_not_a_bare_module_file_is_refused() {
        let m = manifest("fr", &["core"]);
        for bad in [
            "etc/ritornello/locales/core/fr.toml",
            "../core.toml",
            "sub/core.toml",
            ".hidden.toml",
            "usr/local/lib/ritornello/plugins/ritornello-plugin-radio",
            "plugins.toml.fragment",
            "core.txt",
            "core",
        ] {
            let files = vec![file(bad, "k = \"v\"\n")];
            assert!(matches!(validate(&m, &files), Err(PackError::BadEntry(_))), "{bad} was not refused as BadEntry");
        }
    }

    /// Refusal 2: the pack declares one language, and its files may only be
    /// that language's. The shape this forbids is a "German" pack quietly
    /// carrying a French file.
    #[test]
    fn a_language_that_is_not_a_bare_code_is_refused() {
        for bad in ["", "fr/../..", "a-very-long-language-code", "fr.toml", "fr toml"] {
            let m = manifest(bad, &["core"]);
            let files = vec![file("core.toml", "k = \"v\"\n")];
            assert!(matches!(validate(&m, &files), Err(PackError::BadLanguage(_))), "{bad:?} was not refused");
        }
    }

    /// Refusal 3, both directions: the manifest is load-bearing, not
    /// decorative. A file nobody declared cannot be written, and a declared
    /// file that is absent means the archive is not what it says it is --
    /// which is also what makes removal exact later on.
    #[test]
    fn the_declared_module_list_and_the_files_must_match_exactly() {
        let m = manifest("fr", &["core"]);
        let extra = vec![file("core.toml", "k = \"v\"\n"), file("radio.toml", "k = \"v\"\n")];
        assert!(matches!(validate(&m, &extra), Err(PackError::UndeclaredFile(_))));

        let m = manifest("fr", &["core", "radio"]);
        let short = vec![file("core.toml", "k = \"v\"\n")];
        assert!(matches!(validate(&m, &short), Err(PackError::MissingFile(_))));
    }

    /// Refusal 4: a pack is a flat table of text. A nested table and a
    /// non-string value are each refused, and they are separate cases
    /// because they fail in different places of the TOML parse.
    #[test]
    fn a_file_that_is_not_a_flat_table_of_text_is_refused() {
        let m = manifest("fr", &["core"]);
        for bad in ["[section]\nk = \"v\"\n", "k = 3\n", "k = true\n", "k = [\"a\"]\n", "not toml =\n"] {
            let files = vec![file("core.toml", bad)];
            assert!(matches!(validate(&m, &files), Err(PackError::BadContent(_))), "{bad:?} was not refused");
        }
    }

    /// Refusal 5, two caps and they are not the same cap: the number of
    /// files, and the total number of bytes.
    #[test]
    fn the_two_caps_are_separate_refusals() {
        let many: Vec<String> = (0..MAX_FILES + 1).map(|i| format!("m{i}")).collect();
        let m = PackManifest { modules: many.clone(), ..manifest("fr", &[]) };
        let files: Vec<(String, Vec<u8>)> =
            many.iter().map(|n| file(&format!("{n}.toml"), "k = \"v\"\n")).collect();
        assert!(matches!(validate(&m, &files), Err(PackError::TooManyFiles(_))));

        // The two halves of that condition are also independently load-bearing:
        // a compliant module count (exactly MAX_FILES) must not mask an
        // over-large file list on its own. Without this, a mutation that
        // disabled only `files.len() > MAX_FILES` left the suite green,
        // because the case above grows both counts together and the
        // module-count half of the check still caught it.
        let modules: Vec<String> = (0..MAX_FILES).map(|i| format!("m{i}")).collect();
        let m = PackManifest { modules: modules.clone(), ..manifest("fr", &[]) };
        let too_many_files: Vec<(String, Vec<u8>)> =
            (0..MAX_FILES + 1).map(|i| file(&format!("m{i}.toml"), "k = \"v\"\n")).collect();
        assert!(matches!(validate(&m, &too_many_files), Err(PackError::TooManyFiles(_))));

        let m = manifest("fr", &["core"]);
        let huge = vec![(String::from("core.toml"), vec![b'x'; MAX_BYTES + 1])];
        assert!(matches!(validate(&m, &huge), Err(PackError::TooLarge(_))));
    }

    #[test]
    fn a_manifest_with_an_unknown_field_is_refused() {
        let text = "language = \"fr\"\nversion = \"0.2.0\"\nsource = \"x\"\nmodules = []\nexec = \"/bin/sh\"\n";
        assert!(matches!(parse_manifest(text), Err(PackError::Manifest(_))));
    }

    /// A declared module name that is not a bare name is refused as
    /// `BadModule`, specifically -- not merely refused somehow.
    ///
    /// Before this test, no fixture ever put an invalid name into
    /// `manifest.modules` at all: the whole `BadModule` branch (the loop
    /// over `manifest.modules` calling `valid_pack_name`) could be deleted
    /// outright and the 62-test suite stayed green, because the file loop
    /// re-applies `valid_pack_name` to every file's derived module name and
    /// would refuse the same pack anyway -- just relabelled `MissingFile`
    /// (no file was ever going to match an invalid declared name) or
    /// `BadEntry`. Not a security hole, but a guard no test ever reached,
    /// which a mutation therefore could not catch. This test reaches it:
    /// `"Core"` fails `valid_pack_name` (uppercase), is declared, and no
    /// file is offered for it, so `BadModule` must be the reason, not an
    /// accident of some other check firing first.
    #[test]
    fn a_declared_module_that_is_not_a_bare_name_is_refused() {
        let m = manifest("fr", &["Core"]);
        assert!(matches!(validate(&m, &[]), Err(PackError::BadModule(_))));
    }
}
