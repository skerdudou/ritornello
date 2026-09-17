//! One language's contribution, and nothing else.
//!
//! A layer holds **only the keys its language defines**. It deliberately has
//! no notion of a floor: stacking is what produces a complete answer (see
//! `chain`), and a layer that carried English in its holes could not be
//! stacked at all — the English would shadow every language below it.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layer(HashMap<String, String>);

impl Layer {
    /// Pure parse of a flat TOML pack (`key = "value"`). The error is returned
    /// to the caller that wants to log it.
    pub fn parse(s: &str) -> Result<Layer, toml::de::Error> {
        toml::from_str(s).map(Layer)
    }

    /// Reads a pack from disk.
    ///
    /// File **absent**: `None`, silently — the normal case, most components
    /// have no pack for most languages. Any other error — permission denied,
    /// invalid UTF-8, invalid TOML — is also `None` but **traced**: a pack the
    /// operator meant to install must not disappear without a log line.
    pub fn from_disk(path: &Path) -> Option<Layer> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!("i18n pack {} ignored (read failed): {e}", path.display());
                return None;
            }
        };
        match Layer::parse(&text) {
            Ok(l) => Some(l),
            Err(e) => {
                tracing::warn!("i18n pack {} ignored (invalid TOML): {e}", path.display());
                None
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Exposed for the announcement, which carries layers as data.
    pub fn as_map(&self) -> &HashMap<String, String> {
        &self.0
    }

    pub fn from_map(m: HashMap<String, String>) -> Layer {
        Layer(m)
    }
}

/// One module's layers, indexed by language. A "module" is the granularity
/// at which a language pack ships: the core, or one plugin.
///
/// Companion of `Layer`, defined here rather than alongside its first
/// consumer because task 4 (the announcement) needs it before task 12 (the
/// coverage count) does, and task 4 ships first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleLayers {
    module: String,
    layers: HashMap<String, Layer>,
}

impl ModuleLayers {
    pub fn new(module: impl Into<String>) -> ModuleLayers {
        ModuleLayers { module: module.into(), layers: HashMap::new() }
    }

    /// The module's own name (`"core"`, `"radio"`, `"common"`…). Added for
    /// task 12's completeness count, which reports **per module** and needs
    /// to name each one — the field already existed, it just had no reader
    /// outside this file until now.
    pub fn name(&self) -> &str {
        &self.module
    }

    pub fn insert(&mut self, lang: impl Into<String>, layer: Layer) {
        self.layers.insert(lang.into(), layer);
    }

    pub fn layer(&self, lang: &str) -> Option<&Layer> {
        self.layers.get(lang)
    }

    pub fn languages(&self) -> impl Iterator<Item = &str> {
        self.layers.keys().map(String::as_str)
    }
}

/// Pure parse of a flat TOML pack into a raw map. Every component's parity
/// test (`key_and_param_parity_between_the_embedded_en_and_every_shipped_language`,
/// one per plugin and one for `common`, see `shipped_language_packs` below)
/// calls this directly to compare English and each shipped language's key
/// sets without building a full `Catalog`.
pub fn try_parse(s: &str) -> Result<HashMap<String, String>, toml::de::Error> {
    toml::from_str(s)
}

/// Every `<lang>.toml` file shipped for `module` under `deploy_locales_dir`
/// (a test passes `deploy/locales`), discovered from the tree rather than
/// named — the language codes come from the directory listing itself, not
/// from a literal `"fr"` a caller happens to know about today.
///
/// Task 15 exists to generalize each component's key-parity test beyond
/// "en vs fr": a hardcoded language stops covering a second one the moment
/// it ships, **while still passing** — the exact hazard a caller must
/// additionally guard against by asserting this is non-empty (see this
/// crate's own callers): an empty result here means the discovery broke,
/// not that every language is covered.
///
/// Returns `(lang, raw TOML text)` pairs, unparsed: the caller decides how
/// to react to an invalid pack (this project traces and skips one on disk
/// at runtime, but a parity test wants to fail loudly on one it ships).
/// A module directory that does not exist yields an empty `Vec`, silently —
/// the normal case for the four plugins with no locale directory at all
/// (`ouifm-metas`, `radiofrance-metas`, `nrj-metas`, `console`).
pub fn shipped_language_packs(deploy_locales_dir: &Path, module: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(deploy_locales_dir.join(module)) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(lang) = name.strip_suffix(".toml") else {
            continue;
        };
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            out.push((lang.to_string(), content));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_flat_key_value_pairs() {
        let l = Layer::parse("play = \"Play\"\nstop = \"Stop\"\n").unwrap();
        assert_eq!(l.get("play"), Some("Play"));
        assert_eq!(l.get("stop"), Some("Stop"));
    }

    #[test]
    fn parse_rejects_invalid_toml() {
        assert!(Layer::parse("this is not toml =").is_err());
    }

    #[test]
    fn get_of_an_undefined_key_is_none() {
        // The absence of a floor is the whole point: a layer must say "I
        // don't know" rather than invent an answer, so `Chain` can ask the
        // next layer instead of being shadowed.
        let l = Layer::parse("play = \"Play\"\n").unwrap();
        assert_eq!(l.get("stop"), None);
    }

    #[test]
    fn from_disk_of_an_absent_file_is_none_silently() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Layer::from_disk(&dir.path().join("nl.toml")), None);
    }

    #[test]
    fn from_disk_of_invalid_toml_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fr.toml");
        std::fs::write(&path, "this is not valid").unwrap();
        assert_eq!(Layer::from_disk(&path), None);
    }

    #[test]
    fn from_disk_of_a_valid_pack_is_some() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fr.toml");
        std::fs::write(&path, "play = \"Lecture\"\n").unwrap();
        let l = Layer::from_disk(&path).unwrap();
        assert_eq!(l.get("play"), Some("Lecture"));
    }

    #[test]
    fn is_empty_reflects_key_count() {
        assert!(Layer::default().is_empty());
        assert!(!Layer::parse("play = \"Play\"\n").unwrap().is_empty());
    }

    #[test]
    fn as_map_and_from_map_round_trip() {
        let l = Layer::parse("play = \"Play\"\n").unwrap();
        let rebuilt = Layer::from_map(l.as_map().clone());
        assert_eq!(rebuilt, l);
    }

    #[test]
    fn module_layers_exposes_its_own_name() {
        let m = ModuleLayers::new("radio");
        assert_eq!(m.name(), "radio");
    }

    #[test]
    fn module_layers_holds_one_layer_per_language() {
        let mut m = ModuleLayers::new("radio");
        m.insert("fr", Layer::parse("play = \"Lecture\"\n").unwrap());
        m.insert("nl", Layer::parse("play = \"Spelen\"\n").unwrap());
        assert_eq!(m.layer("fr").and_then(|l| l.get("play")), Some("Lecture"));
        assert_eq!(m.layer("nl").and_then(|l| l.get("play")), Some("Spelen"));
        assert_eq!(m.layer("de"), None);
        let mut langs: Vec<&str> = m.languages().collect();
        langs.sort();
        assert_eq!(langs, ["fr", "nl"]);
    }

    // --- shipped_language_packs: tree discovery for the generalized parity
    // check (task 15) ---

    #[test]
    fn shipped_language_packs_discovers_every_lang_toml_under_the_module_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "play = \"Lecture\"\n").unwrap();
        std::fs::write(dir.path().join("radio/nl.toml"), "play = \"Spelen\"\n").unwrap();
        let mut found = shipped_language_packs(dir.path(), "radio");
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let langs: Vec<&str> = found.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(langs, ["fr", "nl"]);
        assert_eq!(found[0].1, "play = \"Lecture\"\n");
    }

    /// **[MUTATION]** barrier the review of this chantier's earlier tasks
    /// found and fixed twice: a discovery that silently returns nothing must
    /// not be indistinguishable, from a caller's assertion, from "every
    /// language is covered". This pins the *other* half — a directory that
    /// genuinely has nothing (or does not exist) really does yield empty —
    /// so a caller's `assert!(!shipped.is_empty(), ...)` is checking a real
    /// fact about the tree, not a constant this function always returns.
    #[test]
    fn shipped_language_packs_of_an_absent_module_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(shipped_language_packs(dir.path(), "console").is_empty());
    }

    #[test]
    fn shipped_language_packs_only_reads_dot_toml_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "play = \"Lecture\"\n").unwrap();
        std::fs::write(dir.path().join("radio/README.md"), "not a pack\n").unwrap();
        let found = shipped_language_packs(dir.path(), "radio");
        assert_eq!(found.len(), 1, "the non-.toml file must be ignored");
        assert_eq!(found[0].0, "fr");
    }
}
