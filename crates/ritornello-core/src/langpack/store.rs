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
}

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
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in &contents.files {
        crate::update::write_atomic(&dir.join(name), bytes)?;
    }
    let text = toml::to_string(&contents.manifest)
        .map_err(|e| std::io::Error::other(format!("serialising the pack manifest: {e}")))?;
    crate::update::write_atomic(&dir.join("pack.toml"), text.as_bytes())?;
    Ok(())
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
        // declares -- `pack_id(manifest.language)` -- and not merely be some
        // bare name `valid_pack_id` happens to accept. Without this, a
        // hand-placed directory under a different name (`french/` declaring
        // `language = "fr"`) is listed as a second row for a language the
        // release's own pack already covers, and pressing Remove on it
        // reaches `store::remove(root, &that_row_id)`, which looks up a
        // directory named after the *row's* id -- not this one -- so it
        // answers `Ok(false)` and nothing happens: a row nobody can ever
        // clear from the page. `docs/interface.md` documents the id-named
        // convention for what an install writes; this is that same
        // convention enforced for what the sweep reads back.
        let expected_id = pack_id(&manifest.language);
        if id != expected_id {
            tracing::warn!(
                "language pack directory {id:?} ignored: its pack.toml declares \
                 language {:?}, whose directory should be named {expected_id:?}",
                manifest.language
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
        out.push(InstalledPack { id, manifest, layers });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
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
