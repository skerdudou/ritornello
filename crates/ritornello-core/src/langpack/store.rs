//! Where an installed pack lives, and what a removal takes.
//!
//! One directory per pack, holding the `pack.toml` it was published with and
//! one `<module>.toml` per module it covers. The manifest travels **with**
//! the pack rather than into `state.json`, and that is deliberate: a device
//! whose settings are reset to their defaults must still know what it has
//! installed, and a pack copied onto a card by hand must be legible to the
//! same walk.

use std::path::{Path, PathBuf};

use ritornello_i18n::{Layer, PackManifest};

use super::archive::PackContents;

/// One pack found on disk.
#[derive(Debug, Clone)]
pub struct InstalledPack {
    /// Its directory name, which is also its component name.
    pub id: String,
    pub manifest: PackManifest,
    /// `(module, layer)`, sorted by module name.
    pub layers: Vec<(String, Layer)>,
    /// When this pack was **first** installed, in seconds since the epoch
    /// (`INSTALLED_AT`). `0` when the file is absent or unreadable: a pack
    /// written before the file existed counts as the oldest there is, which is
    /// what spec §5.3 needs when two third-party packs cover the same module
    /// (`i18n::Registry::ordered_packs`, which never reads it for ours).
    pub installed_at: u64,
}

/// The file, inside a pack's own directory, that holds the day it was first
/// installed. Beside `pack.toml` rather than in `state.json`, for the reason
/// the manifest is there: a device whose settings are reset must still know.
const INSTALLED_AT: &str = "installed-at";

/// The prefix that turns a language into a pack id, and the prefix
/// `release::classify_asset` looks for when it reads a published archive's
/// name back apart.
///
/// A **constant**, not a literal repeated at each of its three call sites,
/// because the name is a contract those three places must agree on: what a
/// pack is published under (`archive`/the release job), what it is stored
/// on disk as and read back from (`pack_id`/`inventory`, this module), and
/// what a running core recognises as a pack rather than a plugin when it
/// reads a release's asset list (`classify_asset`). Three copies of this
/// string would be three chances for one of them to drift from the other
/// two -- exactly the shape of defect this constant exists to make
/// impossible rather than merely unlikely.
pub const PACK_ID_PREFIX: &str = "ritornello-lang-";

/// The component name of the pack for `language`.
///
/// The same string three times over: the directory on disk, the row on the
/// components page, and the prefix of the published archive
/// (`ritornello-lang-fr-0.2.0.tar.gz`). One function so the three cannot
/// drift.
pub fn pack_id(language: &str) -> String {
    format!("{PACK_ID_PREFIX}{language}")
}

/// The reverse of `pack_id`: the language a component row names, when that
/// row is a language pack's.
///
/// A component row (`ComponentOffer::name`) is keyed by **pack id**
/// (`ritornello-lang-fr`), while the `/api/locale` page needs a
/// `LanguagePackRow` keyed by **language** (`fr`) — the card shows
/// languages, not archive names. Kept next to `pack_id` rather than
/// unspelling the prefix at the call site: the shape of an id belongs to
/// one module, and a second copy of that prefix is a thing that drifts
/// from this one the day either changes.
pub fn language_of(id: &str) -> Option<&str> {
    id.strip_prefix(PACK_ID_PREFIX)
}

/// The prefix of a **third-party** pack's id. Deliberately not an extension of
/// `PACK_ID_PREFIX`: `ritornello-xlang-` does not start with
/// `ritornello-lang-`, so `language_of` answers `None` for a third-party id and
/// no route written for our own packs can be reached with one.
pub const THIRD_PARTY_PACK_PREFIX: &str = "ritornello-xlang-";

/// How many hex digits of the source's digest a third-party id carries.
const SOURCE_HASH_LEN: usize = 12;

/// The id of the pack for `language` published by the repository `repo`
/// (`owner/repo`, any case): `ritornello-xlang-<language>-<h12>`, where `<h12>`
/// is the first twelve lowercase hex digits of the SHA-256 of the lowercased
/// `owner/repo`.
///
/// **Why a digest and not the repository itself** (the spec's
/// `ritornello-lang-<lang>+<owner>+<repo>`): `ritornello_i18n::valid_pack_id`
/// admits neither `+` nor `.` and stops at 64 bytes, while a GitHub repository
/// name may hold dots and run to a hundred bytes. Widening it would change a
/// shared crate, which republishes every component under unchanged numbers.
/// Sixteen bytes of language at most (`valid_locale`) make this id 46 bytes
/// at most.
///
/// **The digest comes last**, so the language is read back unambiguously
/// (`third_party_language_of`): hex holds no dash, so the last dash of the id
/// is the one before the digest, whatever dashes the language holds (`pt-BR`).
///
/// The repository is never read back out of the id: the pack's own
/// `pack.toml` names its source, and `inventory` checks that the two agree.
pub fn third_party_pack_id(language: &str, repo: &str) -> String {
    let digest = crate::update::download::digest_hex(repo.to_ascii_lowercase().as_bytes());
    format!("{THIRD_PARTY_PACK_PREFIX}{language}-{}", &digest[..SOURCE_HASH_LEN])
}

/// The id a pack for `language` takes when its source is `source` (an
/// `owner/repo`, as `release::parse_repo_url` reads one): ours for `None` or
/// for the official repository in any case, a third party's otherwise.
///
/// `None` mapping to ours is only ever right for a pack **already on disk**
/// (`inventory`): a pack being downloaded is held to the source it came from,
/// and an unreadable `source` there is a refusal, never "ours"
/// (`update::Worker::install_pack`).
pub fn pack_id_for(language: &str, source: Option<&str>) -> String {
    match source {
        Some(repo) if !repo.eq_ignore_ascii_case(crate::update::release::REPO) => {
            third_party_pack_id(language, repo)
        }
        _ => pack_id(language),
    }
}

/// The language a **third-party** pack id names, or `None` for anything that
/// is not exactly `ritornello-xlang-<language>-<twelve lowercase hex digits>`
/// with a language `valid_locale` accepts.
///
/// Every part is checked, not merely split: a name that only looks like a
/// third-party id must not be read as naming some language.
pub fn third_party_language_of(id: &str) -> Option<&str> {
    let (language, hash) = id.strip_prefix(THIRD_PARTY_PACK_PREFIX)?.rsplit_once('-')?;
    let is_hash =
        hash.len() == SOURCE_HASH_LEN && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (is_hash && crate::status::valid_locale(language)).then_some(language)
}

/// The language any pack id names, ours or a third party's.
pub fn any_language_of(id: &str) -> Option<&str> {
    language_of(id).or_else(|| third_party_language_of(id))
}

/// `root` joined with `id`, or `None` for an `id` that is not a bare name.
///
/// **This is the security boundary**, the same shape
/// `ritornello_updater::target::target_of` draws for the privileged side and
/// for the same reason: `id` reaching here may have come from an HTTP
/// request (a language code, wrapped by `pack_id`) rather than from a value
/// this crate produced itself, so a bare `root.join(id)` would let `".."`
/// resolve to the parent of `root` -- `/etc/ritornello`, on a real device,
/// which holds every other component's configuration too.
/// Refusing here, before either caller ever joins the path itself, is what
/// makes that refusal apply everywhere rather than at each call site.
fn pack_dir(root: &Path, id: &str) -> Option<PathBuf> {
    if !ritornello_i18n::valid_pack_id(id) {
        return None;
    }
    Some(root.join(id))
}

/// The error a refused id is reported as. Carries the id verbatim: an
/// operator reading the journal after a refusal needs the string that was
/// refused, the same reasoning `ritornello_updater::target::TargetError`
/// applies to a plugin file name.
fn refused_id(id: &str) -> std::io::Error {
    std::io::Error::other(format!("refusing the language pack id {id:?}: it is not a bare name"))
}

/// Writes a pack, replacing whatever was there.
///
/// **Replaces rather than merges**: the previous version's directory is
/// removed first, so a module a new version no longer carries stops
/// answering instead of lingering as a file nothing references. Each file
/// is written through `write_atomic` (a temporary beside its target, then
/// `rename`), so a power cut mid-write leaves either the old file or the new
/// one, never a truncated one -- the device this runs on gets unplugged.
///
/// The manifest is written last **on purpose**, but that ordering does not
/// by itself guarantee a directory holding a manifest has every file it
/// names: a crash can still land between two of these `write_atomic` calls.
/// What actually carries the property is `inventory`'s posture on the way
/// back in -- an absent directory, an empty one, files with no manifest yet,
/// a manifest torn by a crash mid-`rename`, and a manifest naming a module
/// whose file never landed are all read as "not a pack" and skipped, never
/// presented as one that installed successfully.
pub fn install(root: &Path, id: &str, contents: &PackContents) -> std::io::Result<()> {
    let dir = pack_dir(root, id).ok_or_else(|| refused_id(id))?;
    // Read **before** the directory goes: an update replaces every file, and
    // the day of the first installation must survive it, or every update
    // would make its pack the newest (spec §5.3).
    let installed_at = read_installed_at(&dir).unwrap_or_else(crate::update::now_unix_s);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in &contents.files {
        crate::update::write_atomic(&dir.join(name), bytes)?;
    }
    crate::update::write_atomic(&dir.join(INSTALLED_AT), installed_at.to_string().as_bytes())?;
    let text = toml::to_string(&contents.manifest)
        .map_err(|e| std::io::Error::other(format!("serialising the pack manifest: {e}")))?;
    crate::update::write_atomic(&dir.join("pack.toml"), text.as_bytes())?;
    Ok(())
}

/// The day a pack directory says it was first installed, when it says so
/// readably.
fn read_installed_at(dir: &Path) -> Option<u64> {
    std::fs::read_to_string(dir.join(INSTALLED_AT)).ok()?.trim().parse().ok()
}

/// Removes a pack's directory. `Ok(false)` when there was nothing to remove
/// -- a fact the caller reports rather than an error it handles. A refused
/// id is a different thing entirely and is never folded into `Ok(false)`:
/// reporting a refusal as "nothing was there" is a lie the caller would act
/// on, most dangerously by treating a `".."` it should have rejected as an
/// ordinary miss instead of an attempt to escape into `/etc/ritornello` at large.
pub fn remove(root: &Path, id: &str) -> std::io::Result<bool> {
    let dir = pack_dir(root, id).ok_or_else(|| refused_id(id))?;
    if !dir.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(true)
}

/// Every readable pack under `root`, sorted by id.
///
/// Every failure is "not a pack", never an error: an absent root is the
/// normal state of a device that has installed none, and a directory that
/// does not parse is something this walk did not write. Both are traced, so
/// a pack the operator meant to install cannot vanish in silence -- the same
/// posture `Layer::from_disk` already takes.
pub fn inventory(root: &Path) -> Vec<InstalledPack> {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in dirs.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if !ritornello_i18n::valid_pack_id(&id) {
            tracing::warn!("language pack directory {id:?} ignored: not a bare name");
            continue;
        }
        let dir = entry.path();
        let Ok(text) = std::fs::read_to_string(dir.join("pack.toml")) else {
            continue;
        };
        let manifest = match ritornello_i18n::parse_manifest(&text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("language pack {id} ignored: {e}");
                continue;
            }
        };
        // The directory must be named after the language its own manifest
        // declares **and the source it names** -- `pack_id_for(language,
        // source)` -- and not merely be some bare name `valid_pack_id`
        // happens to accept. Without this, a
        // hand-placed directory under a different name (`french/` declaring
        // `language = "fr"`) is listed as a second row for a language the
        // release's own pack already covers, and pressing Remove on it
        // reaches `store::remove(root, &that_row_id)`, which looks up a
        // directory named after the *row's* id -- not this one -- so it
        // answers `Ok(false)` and nothing happens: a row nobody can ever
        // clear from the page. `docs/interface.md` documents the id-named
        // convention for what an install writes; this is that same
        // convention enforced for what the sweep reads back.
        //
        // The source half is what keeps the two namespaces apart: a
        // stranger's pack moved under our id (`ritornello-lang-fr`) is not
        // listed as ours, nor ours under a stranger's id. A `source` that
        // does not read as a GitHub repository maps to our id here, and only
        // here: this walk reads what is already on the device, while a
        // download is held to the exact repository it came from
        // (`update::Worker::install_pack`).
        let source = crate::update::release::parse_repo_url(&manifest.source);
        let expected_id = pack_id_for(&manifest.language, source.as_deref());
        if id != expected_id {
            tracing::warn!(
                "language pack directory {id:?} ignored: its pack.toml declares \
                 language {:?} from {:?}, whose directory should be named {expected_id:?}",
                manifest.language,
                manifest.source
            );
            continue;
        }
        let mut files = Vec::with_capacity(manifest.modules.len());
        for module in &manifest.modules {
            match std::fs::read(dir.join(format!("{module}.toml"))) {
                Ok(bytes) => files.push((format!("{module}.toml"), bytes)),
                Err(e) => {
                    tracing::warn!("language pack {id}: {module}.toml unreadable ({e})");
                }
            }
        }
        let layers = match ritornello_i18n::validate(&manifest, &files) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("language pack {id} ignored: {e}");
                continue;
            }
        };
        let installed_at = read_installed_at(&dir).unwrap_or(0);
        out.push(InstalledPack { id, manifest, layers, installed_at });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Test helper, shared by every test that needs a pack on disk: writes one
/// the way an install leaves it under `root` -- ours when `repo` is `None`, a
/// third party's (`ritornello-xlang-<lang>-<h12>`) otherwise -- with its
/// `installed-at` file when `installed_at` is given and one `<module>.toml`
/// per `(module, body)`. Returns its id.
#[cfg(test)]
pub(crate) fn write_test_pack(
    root: &Path,
    lang: &str,
    repo: Option<&str>,
    installed_at: Option<u64>,
    modules: &[(&str, &str)],
) -> String {
    let id = pack_id_for(lang, repo);
    let source = format!("https://github.com/{}", repo.unwrap_or(crate::update::release::REPO));
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    let names: Vec<String> = modules.iter().map(|(m, _)| format!("{m:?}")).collect();
    let manifest = format!(
        "language = \"{lang}\"\nversion = \"1.0.0\"\nsource = \"{source}\"\nmodules = [{}]\n",
        names.join(", ")
    );
    std::fs::write(dir.join("pack.toml"), manifest).unwrap();
    for (module, body) in modules {
        std::fs::write(dir.join(format!("{module}.toml")), body).unwrap();
    }
    if let Some(at) = installed_at {
        std::fs::write(dir.join(INSTALLED_AT), at.to_string()).unwrap();
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents(language: &str, modules: &[(&str, &str)]) -> crate::langpack::archive::PackContents {
        let manifest = PackManifest {
            language: language.to_string(),
            version: "0.2.0-beta.2".to_string(),
            source: "https://github.com/skerdudou/ritornello".to_string(),
            modules: modules.iter().map(|(m, _)| m.to_string()).collect(),
        };
        let files: Vec<(String, Vec<u8>)> =
            modules.iter().map(|(m, b)| (format!("{m}.toml"), b.as_bytes().to_vec())).collect();
        let layers = ritornello_i18n::validate(&manifest, &files).unwrap();
        crate::langpack::archive::PackContents { manifest, layers, files }
    }

    #[test]
    fn an_installed_pack_is_found_again_by_the_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let c = contents("fr", &[("core", "standby = \"VEILLE\"\n")]);
        install(dir.path(), &pack_id("fr"), &c).unwrap();
        let found = inventory(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ritornello-lang-fr");
        assert_eq!(found[0].manifest.version, "0.2.0-beta.2");
        assert_eq!(found[0].layers[0].1.get("standby"), Some("VEILLE"));
    }

    /// **F6 of the whole-branch review.** A directory hand-placed under a
    /// name that is not `pack_id` of its own declared language is skipped,
    /// not listed under its own bare name: before this test, `inventory`
    /// only checked `valid_pack_id` (a shape check, not an identity check),
    /// so `french/` declaring `language = "fr"` was listed as `french`,
    /// producing a second row for a language `ritornello-lang-fr` already
    /// covers -- and `store::remove(root, "french")` (what pressing Remove
    /// on that row would call) never matches the directory `store::remove`
    /// looks for when it is instead asked for `ritornello-lang-fr`, so
    /// nothing on the page could ever clear it.
    #[test]
    fn a_directory_named_for_something_other_than_its_own_language_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &pack_id("fr"), &contents("fr", &[("core", "k = \"v\"\n")])).unwrap();
        // A second, hand-placed directory: same language, wrong name.
        std::fs::rename(dir.path().join("ritornello-lang-fr"), dir.path().join("french")).unwrap();

        let found = inventory(dir.path());

        assert!(found.is_empty(), "a directory misnamed for its own declared language must not be listed at all: {found:?}");
    }

    /// Reinstalling replaces, and leaves nothing of the previous version
    /// behind -- a module dropped between two versions of a pack must not
    /// keep answering from the older copy.
    #[test]
    fn reinstalling_drops_a_module_the_new_version_no_longer_carries() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &pack_id("fr"),
            &contents("fr", &[("core", "k = \"v\"\n"), ("radio", "k = \"v\"\n")])).unwrap();
        install(dir.path(), &pack_id("fr"),
            &contents("fr", &[("core", "k = \"v\"\n")])).unwrap();
        let found = inventory(dir.path());
        assert_eq!(found[0].layers.len(), 1, "radio.toml must be gone, not merely unreferenced");
        assert!(!dir.path().join("ritornello-lang-fr/radio.toml").exists());
    }

    #[test]
    fn removing_a_pack_takes_its_directory_and_answers_whether_it_was_there() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &pack_id("fr"), &contents("fr", &[("core", "k = \"v\"\n")])).unwrap();
        assert!(remove(dir.path(), &pack_id("fr")).unwrap(), "it was there");
        assert!(inventory(dir.path()).is_empty());
        assert!(!remove(dir.path(), &pack_id("fr")).unwrap(), "it was not there the second time");
    }

    /// **The property §7.3 turns on.** A removal touches the pack's own
    /// directory and nothing else -- above all not a neighbouring pack, or
    /// any other file under the packs root that no install ever wrote.
    #[test]
    fn removing_a_pack_never_touches_a_neighbour_or_a_stray_file() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &pack_id("fr"), &contents("fr", &[("core", "k = \"v\"\n")])).unwrap();
        install(dir.path(), &pack_id("de"), &contents("de", &[("core", "k = \"v\"\n")])).unwrap();
        std::fs::write(dir.path().join("a-note-from-the-operator.txt"), "mine").unwrap();
        remove(dir.path(), &pack_id("fr")).unwrap();
        assert!(dir.path().join("ritornello-lang-de/core.toml").exists(), "the neighbour survives");
        assert!(dir.path().join("a-note-from-the-operator.txt").exists(), "a stray file survives");
    }

    /// A hostile id is refused by `install` and `remove` before either ever
    /// forms a path from it -- not merely rejected by chance because the
    /// path it would have formed happens not to exist.
    ///
    /// `".."` is the id that matters most: joined onto a real packs root
    /// (`/etc/ritornello/language-packs`), it resolves to `/etc/ritornello`,
    /// which holds every other component's configuration too. A sibling
    /// directory next to the temporary packs root stands in for it here --
    /// built to exist, so a bug that stopped refusing would actually destroy
    /// something and the test would actually notice.
    #[test]
    fn a_hostile_id_is_refused_by_install_and_remove_and_touches_nothing() {
        const HOSTILE: &[&str] = &["..", "a/b", "../../etc/ritornello/locales", ".", ""];
        for id in HOSTILE {
            let workspace = tempfile::tempdir().unwrap();
            let root = workspace.path().join("packs");
            std::fs::create_dir_all(&root).unwrap();
            let sibling = workspace.path().join("sibling-locales");
            std::fs::create_dir_all(&sibling).unwrap();
            std::fs::write(sibling.join("fr.toml"), "k = \"v\"\n").unwrap();

            let c = contents("fr", &[("core", "k = \"v\"\n")]);
            assert!(install(&root, id, &c).is_err(), "{id:?} should be refused by install");
            assert!(remove(&root, id).is_err(), "{id:?} should be refused by remove");

            assert!(sibling.join("fr.toml").exists(), "{id:?}: the sibling survives install and remove");
            assert!(root.exists(), "{id:?}: the packs root itself survives");
        }
    }

    /// A root that does not exist, or holds junk, is a normal state on a
    /// device that has never installed a pack -- never a panic, never an
    /// error the caller has to handle.
    #[test]
    fn an_absent_or_junk_root_yields_an_empty_inventory() {
        assert!(inventory(std::path::Path::new("/nonexistent/ritornello/packs")).is_empty());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("loose.toml"), "k = \"v\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("no-manifest-here")).unwrap();
        std::fs::create_dir_all(dir.path().join("bad-manifest")).unwrap();
        std::fs::write(dir.path().join("bad-manifest/pack.toml"), "this is not toml =\n").unwrap();
        assert!(inventory(dir.path()).is_empty());
    }

    /// A regionalised language code is a pack id the rest of the product
    /// already allows (`ritornello_core::status::locales::valid_locale`,
    /// `ritornello_i18n::pack::valid_language` both accept `pt-BR`,
    /// `zh_Hant`), so it must install and be found again, not be refused at
    /// the last step by a directory-name check narrower than the language
    /// grammar that let the pack get this far.
    #[test]
    fn a_regionalised_language_code_installs_and_is_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let c = contents("pt-BR", &[("core", "standby = \"PARADO\"\n")]);
        install(dir.path(), &pack_id("pt-BR"), &c).unwrap();
        let found = inventory(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ritornello-lang-pt-BR");
        assert_eq!(found[0].layers[0].1.get("standby"), Some("PARADO"));

        let c = contents("zh_Hant", &[("core", "standby = \"待機\"\n")]);
        install(dir.path(), &pack_id("zh_Hant"), &c).unwrap();
        let found = inventory(dir.path());
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|p| p.id == "ritornello-lang-zh_Hant"));
    }

    /// The reverse of `pack_id`, round-tripped -- and `None` for a component
    /// name that is not one of this module's own pack ids, since a stray
    /// name must not be silently treated as naming some language.
    #[test]
    fn language_of_reverses_pack_id_and_refuses_a_stranger() {
        assert_eq!(language_of(&pack_id("fr")), Some("fr"));
        assert_eq!(language_of(&pack_id("pt-BR")), Some("pt-BR"));
        assert_eq!(language_of("radio"), None);
        assert_eq!(language_of("ritornello-plugin-radio"), None);
    }

    #[test]
    fn a_third_party_pack_id_is_a_bare_name_the_official_routes_never_read_as_a_language() {
        let id = third_party_pack_id("pt-BR", "Some.One/Very.Long-Repository_Name");
        assert!(ritornello_i18n::valid_pack_id(&id), "{id}");
        assert!(id.len() <= 64);
        assert_eq!(language_of(&id), None);
        assert_eq!(id, third_party_pack_id("pt-BR", "some.one/very.long-repository_name"), "case-insensitive source");
        assert_ne!(id, third_party_pack_id("pt-BR", "other/repo"));
        // The longest language `valid_locale` admits still makes a valid id.
        let longest = third_party_pack_id(&"a".repeat(16), "o/r");
        assert!(ritornello_i18n::valid_pack_id(&longest) && longest.len() <= 64, "{longest}");
    }

    /// P1: the hash is last, so the language comes back whatever dashes it
    /// holds — and `any_language_of` reads both namespaces.
    #[test]
    fn a_third_party_id_gives_its_language_back() {
        let id = third_party_pack_id("pt-BR", "z/zed");
        assert_eq!(third_party_language_of(&id), Some("pt-BR"));
        assert_eq!(any_language_of(&id), Some("pt-BR"));
        assert_eq!(any_language_of(&pack_id("fr")), Some("fr"));
        assert_eq!(third_party_language_of(&pack_id("fr")), None, "ours is not a third party's");
        assert_eq!(any_language_of("radio"), None);
    }

    /// One refusal per part of the shape, each one a name every other part of
    /// which is sound. **[MUTATION]** drop any one check: its line goes red.
    #[test]
    fn a_name_that_only_looks_like_a_third_party_id_names_no_language() {
        assert_eq!(third_party_language_of("ritornello-xlang-fr-0123456789ab"), Some("fr"), "the control");
        assert_eq!(third_party_language_of("ritornello-lang-fr-0123456789ab"), None, "prefix");
        assert_eq!(third_party_language_of("ritornello-xlang-fr-0123456789a"), None, "eleven digits");
        assert_eq!(third_party_language_of("ritornello-xlang-fr-0123456789abc"), None, "thirteen digits");
        assert_eq!(third_party_language_of("ritornello-xlang-fr-0123456789AB"), None, "uppercase hex");
        assert_eq!(third_party_language_of("ritornello-xlang-fr-0123456789ag"), None, "not hex");
        assert_eq!(third_party_language_of("ritornello-xlang--0123456789ab"), None, "empty language");
        assert_eq!(third_party_language_of("ritornello-xlang-a.b-0123456789ab"), None, "not a language");
        assert_eq!(third_party_language_of("ritornello-xlang-0123456789ab"), None, "no language at all");
    }

    #[test]
    fn a_pack_id_follows_its_source() {
        assert_eq!(pack_id_for("fr", None), pack_id("fr"));
        assert_eq!(pack_id_for("fr", Some("skerdudou/ritornello")), pack_id("fr"));
        assert_eq!(pack_id_for("fr", Some("Skerdudou/Ritornello")), pack_id("fr"), "ours in any case");
        assert_eq!(pack_id_for("fr", Some("z/zed")), third_party_pack_id("fr", "z/zed"));
    }

    #[test]
    fn the_inventory_lists_a_third_party_pack_under_its_own_id_and_refuses_it_under_ours() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = contents("fr", &[("core", "k = \"v\"\n")]);
        c.manifest.source = "https://github.com/z/zed".into();
        install(dir.path(), &third_party_pack_id("fr", "z/zed"), &c).unwrap();
        assert_eq!(inventory(dir.path()).len(), 1);
        std::fs::rename(dir.path().join(third_party_pack_id("fr", "z/zed")), dir.path().join(pack_id("fr"))).unwrap();
        assert!(inventory(dir.path()).is_empty(), "a stranger's pack must not pass for ours");
    }

    /// The mirror: ours moved under a stranger's id is not theirs either.
    #[test]
    fn the_inventory_refuses_our_pack_under_a_third_party_id() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &pack_id("fr"), &contents("fr", &[("core", "k = \"v\"\n")])).unwrap();
        std::fs::rename(dir.path().join(pack_id("fr")), dir.path().join(third_party_pack_id("fr", "z/zed"))).unwrap();
        assert!(inventory(dir.path()).is_empty(), "our pack must not pass for a stranger's");
    }

    #[test]
    fn updating_a_pack_keeps_the_day_it_was_first_installed() {
        let dir = tempfile::tempdir().unwrap();
        let c = contents("fr", &[("core", "k = \"v\"\n")]);
        install(dir.path(), &pack_id("fr"), &c).unwrap();
        std::fs::write(dir.path().join(pack_id("fr")).join("installed-at"), "1000").unwrap();
        install(dir.path(), &pack_id("fr"), &c).unwrap();
        assert_eq!(inventory(dir.path())[0].installed_at, 1000);
    }

    /// A first install writes the day; a pack written before the file existed
    /// reads as the oldest there is.
    #[test]
    fn a_first_install_writes_the_day_and_a_pack_without_it_is_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &pack_id("fr"), &contents("fr", &[("core", "k = \"v\"\n")])).unwrap();
        assert!(inventory(dir.path())[0].installed_at > 1_000_000_000, "written at the first install");
        std::fs::remove_file(dir.path().join(pack_id("fr")).join("installed-at")).unwrap();
        assert_eq!(inventory(dir.path())[0].installed_at, 0);
    }

    /// A directory whose name is not a bare name is skipped rather than
    /// read: the inventory walks a directory the core wrote, but a device
    /// is a place where files arrive by other means too.
    #[test]
    fn a_directory_with_a_hostile_name_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("..evil")).unwrap();
        std::fs::write(
            dir.path().join("..evil/pack.toml"),
            "language = \"fr\"\nversion = \"1\"\nsource = \"x\"\nmodules = []\n",
        )
        .unwrap();
        assert!(inventory(dir.path()).is_empty());
    }
}
