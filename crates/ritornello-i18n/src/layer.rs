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
/// test (`key_parity_between_the_embedded_en_and_the_fr_pack`, one per
/// plugin and one for `common`) calls this directly to compare English and
/// French key sets without building a full `Catalog`.
pub fn try_parse(s: &str) -> Result<HashMap<String, String>, toml::de::Error> {
    toml::from_str(s)
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
}
