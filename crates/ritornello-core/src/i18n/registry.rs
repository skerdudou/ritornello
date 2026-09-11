//! The core's registry of translation layers.
//!
//! One `Registry` holds, per module (the core itself, or one plugin), two
//! kinds of layers: the ones a module **announced** — a plugin's embedded
//! catalogue (`Announcement.catalog`), or the core's own embedded text,
//! folded in through the same path — and the ones found **on disk**, under
//! the packs root, read fresh on every call. `chain_for` stacks both kinds,
//! for up to three languages, into the one `Chain` the caller resolves keys
//! against.
//!
//! The disk read is separated from the stacking calculation on purpose,
//! the same split `status::locales::list_locales` already draws against
//! `parse_available_locales` and `audio_output::list_devices` against
//! `parse_device_list`: [`stack`] is pure and carries the tests below, and
//! [`Registry::chain_for`] is the thin I/O envelope that reads the disk
//! packs and looks up the announced map before calling it.

use std::collections::HashMap;
use std::path::PathBuf;

use ritornello_i18n::{Chain, Layer, ModuleLayers};

/// Registry of every module's translation layers.
///
/// `announced` is populated by `insert_announced` — called once per plugin
/// announcement, and once for the core's own embedded text and for
/// `common`'s, so that `chain_for` treats every module uniformly rather
/// than special-casing the core. The disk root is read fresh on every
/// `chain_for` call, never cached here: an operator editing a pack on disk
/// is picked up the next time a chain is built, exactly like
/// `status::locales::list_locales` already reads the directory fresh on
/// every `/api/locale` request.
#[derive(Debug, Default)]
pub struct Registry {
    root: PathBuf,
    announced: HashMap<String, ModuleLayers>,
}

impl Registry {
    /// A registry with no announced module yet, reading disk packs under
    /// `root` (`<root>/<module>/<lang>.toml`).
    pub fn new(root: PathBuf) -> Registry {
        Registry { root, announced: HashMap::new() }
    }

    /// Records — or replaces — one module's announced layers.
    ///
    /// Called once per plugin announcement (its embedded catalogue,
    /// `Announcement.catalog` turned into `ModuleLayers` via
    /// `Layer::from_map`), and by the core itself for its own module and
    /// for `common`, so `chain_for` never has to know which caller a given
    /// module came from.
    pub fn insert_announced(&mut self, module: impl Into<String>, layers: ModuleLayers) {
        self.announced.insert(module.into(), layers);
    }

    /// Forgets a module's announced layers — a plugin that disconnected or
    /// was uninstalled. Its disk packs, if any, are untouched: the next
    /// `chain_for` call still finds them, only the announced tier is gone.
    ///
    /// `ritornello-core` has no `lib` target, so an item this crate's own
    /// production code never calls reads as dead code even though it is
    /// part of `Registry`'s public interface (this task's brief names it
    /// explicitly). No caller exists yet because the registration flow
    /// that would call it on a plugin's disconnection (`register.rs`) is
    /// wired in a later task — exercised here only by its own tests.
    #[allow(dead_code)]
    pub fn forget(&mut self, module: &str) {
        self.announced.remove(module);
    }

    /// Builds the resolution chain for `module`, in the fixed order the
    /// chantier turns on: the whole `chosen`-language block, then the
    /// whole `fallback`-language block, then the whole `en` block.
    ///
    /// **The chosen language wins over specificity, deliberately**:
    /// someone who asked for a language prefers a generic word in that
    /// language over a well-chosen word in the fallback — so `chosen`'s
    /// four layers, however sparse, are exhausted before `fallback`'s are
    /// even tried. Within one language, the historical order holds: disk
    /// before announced, the module's own vocabulary before `common`'s.
    ///
    /// `chosen`, `fallback` and `en` may coincide (typically `fallback` is
    /// itself `"en"` until a device has a real fallback setting); the
    /// resulting duplicate layers are harmless, `Chain::get` only ever
    /// needs the first match.
    pub fn chain_for(&self, module: &str, chosen: &str, fallback: &str) -> Chain {
        stack(self.block(module, chosen), self.block(module, fallback), self.block(module, "en"))
    }

    /// The I/O side of one language block: reads the two disk packs this
    /// language might have (`<root>/<module>/<lang>.toml`,
    /// `<root>/common/<lang>.toml`) and looks up the two announced layers
    /// already held in memory. No calculation here — `stack` does that.
    fn block(&self, module: &str, lang: &str) -> LanguageBlock {
        LanguageBlock {
            own_disk: self.disk_layer(module, lang),
            own_announced: self.announced_layer(module, lang),
            common_disk: self.disk_layer("common", lang),
            common_announced: self.announced_layer("common", lang),
        }
    }

    fn disk_layer(&self, module: &str, lang: &str) -> Option<Layer> {
        Layer::from_disk(&self.root.join(module).join(format!("{lang}.toml")))
    }

    fn announced_layer(&self, module: &str, lang: &str) -> Option<Layer> {
        self.announced.get(module).and_then(|m| m.layer(lang)).cloned()
    }
}

/// One language's four-layer contribution to a module's chain, gathered
/// but not yet stacked: `Registry::block` builds one of these per language
/// (`chosen`, `fallback`, `en`) before [`stack`] orders the three.
///
/// Field order matches the priority the whole task fixes for a single
/// language: disk before announced, the module's own vocabulary before
/// `common`'s. `Default` gives the empty block every test below starts
/// from, so each test only ever fills in the one or two layers its
/// boundary is about.
#[derive(Debug, Default, Clone)]
struct LanguageBlock {
    own_disk: Option<Layer>,
    own_announced: Option<Layer>,
    common_disk: Option<Layer>,
    common_announced: Option<Layer>,
}

impl LanguageBlock {
    fn into_layers(self) -> impl Iterator<Item = Layer> {
        [self.own_disk, self.own_announced, self.common_disk, self.common_announced].into_iter().flatten()
    }
}

/// Pure: stacks three already-gathered language blocks — `chosen`,
/// `fallback`, `en`, in that order — into one `Chain`. No filesystem
/// access and no lookup: `Registry::chain_for` is the thin I/O wrapper
/// that reads the disk packs and the announced map before calling this,
/// which is what lets the tests below build a block by hand, with
/// `Layer::parse`, and never touch a temporary directory.
///
/// The order between the three blocks is the chantier's central
/// arbitration — the chosen language wins over specificity — and is fixed
/// here, once, rather than left to each caller to get right.
fn stack(chosen: LanguageBlock, fallback: LanguageBlock, en: LanguageBlock) -> Chain {
    let layers = chosen.into_layers().chain(fallback.into_layers()).chain(en.into_layers()).collect();
    Chain::new(layers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(pairs: &[(&str, &str)]) -> Layer {
        let source: String = pairs.iter().map(|(k, v)| format!("{k} = {v:?}\n")).collect();
        Layer::parse(&source).unwrap()
    }

    // The four tests below each isolate ONE boundary of the order fixed by
    // `stack`'s doc: a test that only checked the final answer without
    // controlling every other layer would not tell us which boundary, if
    // any, actually held.

    #[test]
    fn within_one_language_disk_beats_announced() {
        let chosen = LanguageBlock {
            own_disk: Some(layer(&[("k", "from-disk")])),
            own_announced: Some(layer(&[("k", "from-announced")])),
            ..Default::default()
        };
        let chain = stack(chosen, LanguageBlock::default(), LanguageBlock::default());
        assert_eq!(chain.get("k"), "from-disk");
    }

    #[test]
    fn within_one_language_the_module_s_own_vocabulary_beats_common() {
        // Deliberately own_announced (the module's *weaker* tier) against
        // common_disk (common's *stronger* tier): if this still resolves
        // to the module's own value, "own beats common" holds regardless
        // of which tier either side happens to use — not just in the case
        // where own also happens to be on disk.
        let chosen = LanguageBlock {
            own_announced: Some(layer(&[("k", "own-announced")])),
            common_disk: Some(layer(&[("k", "common-disk")])),
            ..Default::default()
        };
        let chain = stack(chosen, LanguageBlock::default(), LanguageBlock::default());
        assert_eq!(chain.get("k"), "own-announced");
    }

    #[test]
    fn the_chosen_language_wins_over_specificity_even_against_a_more_specific_fallback() {
        // The one test that proves the chantier's central arbitration.
        // `fallback`'s layer here is the module's own disk pack — the
        // single most specific layer that exists anywhere in this order —
        // while `chosen` only has common's announced layer, the least
        // specific of all eight. If `chosen` still wins, the language
        // genuinely dominates specificity; if this test is wrong to pass,
        // step 5 (inverting chosen/fallback) must make it fail.
        let chosen =
            LanguageBlock { common_announced: Some(layer(&[("k", "chosen-common-announced")])), ..Default::default() };
        let fallback = LanguageBlock { own_disk: Some(layer(&[("k", "fallback-own-disk")])), ..Default::default() };
        let chain = stack(chosen, fallback, LanguageBlock::default());
        assert_eq!(chain.get("k"), "chosen-common-announced");
    }

    #[test]
    fn the_fallback_language_beats_english() {
        let fallback = LanguageBlock { own_disk: Some(layer(&[("k", "fallback-value")])), ..Default::default() };
        let en = LanguageBlock { own_announced: Some(layer(&[("k", "english-value")])), ..Default::default() };
        let chain = stack(LanguageBlock::default(), fallback, en);
        assert_eq!(chain.get("k"), "fallback-value");
    }

    #[test]
    fn an_unknown_key_still_resolves_to_itself() {
        // The safety net `Chain::get` already carries must survive being
        // reached through three empty blocks.
        let chain = stack(LanguageBlock::default(), LanguageBlock::default(), LanguageBlock::default());
        assert_eq!(chain.get("nope"), "nope");
    }

    // --- Registry itself: proving the I/O wrapper actually wires `stack` to
    // the disk and to the announced map, rather than testing the ordering a
    // second time. ---

    #[test]
    fn chain_for_reads_a_disk_pack_for_the_requested_module_and_language() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/nl.toml"), "play = \"Spelen\"\n").unwrap();
        let registry = Registry::new(dir.path().to_path_buf());
        let chain = registry.chain_for("radio", "nl", "en");
        assert_eq!(chain.get("play"), "Spelen");
    }

    #[test]
    fn chain_for_uses_a_layer_inserted_via_insert_announced() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::new(dir.path().to_path_buf());
        let mut m = ModuleLayers::new("radio");
        m.insert("en", Layer::from_map([("play".to_string(), "Play".to_string())].into()));
        registry.insert_announced("radio", m);
        let chain = registry.chain_for("radio", "en", "en");
        assert_eq!(chain.get("play"), "Play");
    }

    #[test]
    fn forget_removes_the_announced_layer_but_not_the_disk_pack() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/en.toml"), "play = \"disk-play\"\n").unwrap();
        let mut registry = Registry::new(dir.path().to_path_buf());
        let mut m = ModuleLayers::new("radio");
        m.insert("en", Layer::from_map([("stop".to_string(), "announced-stop".to_string())].into()));
        registry.insert_announced("radio", m);
        registry.forget("radio");
        let chain = registry.chain_for("radio", "en", "en");
        assert_eq!(chain.get("play"), "disk-play", "the disk pack must survive forget");
        assert_eq!(chain.get("stop"), "stop", "the announced layer must be gone");
    }
}
