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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by task 6 (registry) and task 8 (update worker)")
)]
pub struct InstalledPack {
    /// Its directory name, which is also its component name.
    pub id: String,
    pub manifest: PackManifest,
    /// `(module, layer)`, sorted by module name.
    pub layers: Vec<(String, Layer)>,
}

/// The component name of the pack for `language`.
///
/// The same string three times over: the directory on disk, the row on the
/// components page, and the prefix of the published archive
/// (`ritornello-lang-fr-0.2.0.tar.gz`). One function so the three cannot
/// drift.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by task 6 (registry) and task 8 (update worker)")
)]
pub fn pack_id(language: &str) -> String {
    format!("ritornello-lang-{language}")
}

fn pack_dir(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

/// Writes a pack, replacing whatever was there.
///
/// **Replaces rather than merges**: the previous version's directory is
/// removed first, so a module a new version no longer carries stops
/// answering instead of lingering as a file nothing references. The manifest
/// is written last, so a directory holding a manifest is a directory whose
/// files are all already there.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by task 6 (registry) and task 8 (update worker)")
)]
pub fn install(root: &Path, id: &str, contents: &PackContents) -> std::io::Result<()> {
    let dir = pack_dir(root, id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in &contents.files {
        std::fs::write(dir.join(name), bytes)?;
    }
    let text = toml::to_string(&contents.manifest)
        .map_err(|e| std::io::Error::other(format!("serialising the pack manifest: {e}")))?;
    std::fs::write(dir.join("pack.toml"), text)?;
    Ok(())
}

/// Removes a pack's directory. `false` when there was nothing to remove --
/// a fact the caller reports rather than an error it handles.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by task 6 (registry) and task 8 (update worker)")
)]
pub fn remove(root: &Path, id: &str) -> std::io::Result<bool> {
    let dir = pack_dir(root, id);
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by task 6 (registry) and task 8 (update worker)")
)]
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
        if !ritornello_i18n::valid_pack_name(&id) {
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
    /// directory and nothing else -- above all not the operator's own
    /// locales root, which no install ever wrote to.
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
