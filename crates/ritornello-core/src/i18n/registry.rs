//! The core's registry of translation layers.
//!
//! One `Registry` holds, per module (the core itself, or one plugin), two
//! kinds of layers: the ones a module **announced** — a plugin's embedded
//! catalogue (`Announcement.catalog`), or the core's own embedded text,
//! folded in through the same path — and the ones found **on disk**, under
//! the packs root. `chain_for` stacks both kinds, for up to three languages,
//! into the one `Chain` the caller resolves keys against.
//!
//! **The disk tier is swept once, not read per call.** `Registry::sweep`
//! walks the pack root — one subdirectory per module, one `<lang>.toml` file
//! per language — and keeps what it finds in memory; `resweep` repeats the
//! walk and replaces that snapshot. `chain_for` itself therefore performs
//! **no I/O at all**: every module it might be asked about has already been
//! read once, at sweep time. This matters three times over — an HTTP route
//! must never block on disk, only a real sweep (not a guessed path) can ever
//! tell a later reader which languages exist at all, and the refresh gesture
//! this crate already documents ("an operator can edit a pack, and the
//! restart is what refreshes it") stays true instead of quietly becoming
//! "on every request".
//!
//! The disk read is separated from the stacking calculation on purpose, the
//! same split `status::locales::list_locales` already draws against
//! `parse_available_locales` and `audio_output::list_devices` against
//! `parse_device_list`: [`stack`] is pure and carries the tests below, and
//! [`sweep_disk`] (wrapped by `Registry::sweep`/`resweep`) is the I/O
//! envelope.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ritornello_i18n::{Chain, Layer, ModuleLayers};

/// Registry of every module's translation layers.
///
/// `disk` is populated by [`Registry::sweep`]/[`Registry::resweep`] — a walk
/// of the pack root, kept in memory until the next sweep. `announced` is
/// populated by `insert_announced` — called once per plugin announcement,
/// and once for the core's own embedded text and for `common`'s, so that
/// `chain_for` treats every module uniformly rather than special-casing the
/// core. Neither tier is read from the filesystem by `chain_for` itself.
#[derive(Debug, Default)]
pub struct Registry {
    root: PathBuf,
    disk: HashMap<String, ModuleLayers>,
    announced: HashMap<String, ModuleLayers>,
}

impl Registry {
    /// Sweeps `root` once and returns a registry holding what it found, with
    /// no announced module yet.
    pub fn sweep(root: PathBuf) -> Registry {
        let disk = sweep_disk(&root);
        Registry { root, disk, announced: HashMap::new() }
    }

    /// Repeats the walk of the pack root and replaces the disk tier —
    /// wholesale, not merged, so a pack removed since the last sweep is
    /// actually forgotten rather than lingering. The announced tier is
    /// untouched: it does not come from this root and a plugin's
    /// announcement is not re-read just because a locale changed.
    ///
    /// Called wherever the code already rebuilds a resolution on a real
    /// locale change (`Core::set_locale`) — the disk-edit-then-restart
    /// gesture this crate documents keeps working, and gains "or pick the
    /// language again" as an equivalent, cheaper trigger.
    pub fn resweep(&mut self) {
        self.disk = sweep_disk(&self.root);
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
    ///
    /// **Performs no I/O.** Both tiers it reads from — `disk` and
    /// `announced` — are already in memory; see the module doc.
    pub fn chain_for(&self, module: &str, chosen: &str, fallback: &str) -> Chain {
        stack(self.block(module, chosen), self.block(module, fallback), self.block(module, "en"))
    }

    /// Gathers one language block from the two tiers already in memory. No
    /// I/O and no calculation — `stack` does the latter.
    fn block(&self, module: &str, lang: &str) -> LanguageBlock {
        LanguageBlock {
            own_disk: self.disk_layer(module, lang),
            own_announced: self.announced_layer(module, lang),
            common_disk: self.disk_layer("common", lang),
            common_announced: self.announced_layer("common", lang),
        }
    }

    fn disk_layer(&self, module: &str, lang: &str) -> Option<Layer> {
        self.disk.get(module).and_then(|m| m.layer(lang)).cloned()
    }

    fn announced_layer(&self, module: &str, lang: &str) -> Option<Layer> {
        self.announced.get(module).and_then(|m| m.layer(lang)).cloned()
    }
}

/// I/O: walks `root`, one subdirectory per module, one `<lang>.toml` file per
/// language — a real directory listing, not a guessed path, which is what
/// lets a later reader enumerate the languages actually present rather than
/// only the ones it already knew to ask about. An unreadable root (absent,
/// no permission) yields an empty map rather than an error: the normal case
/// on a fresh install before any pack is dropped in.
fn sweep_disk(root: &Path) -> HashMap<String, ModuleLayers> {
    let mut out = HashMap::new();
    let Ok(module_dirs) = std::fs::read_dir(root) else {
        return out;
    };
    for module_dir in module_dirs.flatten() {
        if !module_dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let module = module_dir.file_name().to_string_lossy().into_owned();
        let mut layers = ModuleLayers::new(module.clone());
        if let Ok(files) = std::fs::read_dir(module_dir.path()) {
            for file in files.flatten() {
                let filename = file.file_name().to_string_lossy().into_owned();
                let Some(lang) = filename.strip_suffix(".toml") else {
                    continue;
                };
                if let Some(layer) = Layer::from_disk(&file.path()) {
                    layers.insert(lang.to_string(), layer);
                }
            }
        }
        out.insert(module, layers);
    }
    out
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
/// access and no lookup: `Registry::chain_for` is the thin wrapper that
/// gathers the three blocks from its in-memory tiers before calling this,
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

    // --- Registry itself: proving the sweep and `chain_for` are actually
    // wired together, rather than testing the ordering a second time. ---

    #[test]
    fn chain_for_reads_a_disk_pack_for_the_requested_module_and_language() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/nl.toml"), "play = \"Spelen\"\n").unwrap();
        let registry = Registry::sweep(dir.path().to_path_buf());
        let chain = registry.chain_for("radio", "nl", "en");
        assert_eq!(chain.get("play"), "Spelen");
    }

    #[test]
    fn chain_for_uses_a_layer_inserted_via_insert_announced() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
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
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        let mut m = ModuleLayers::new("radio");
        m.insert("en", Layer::from_map([("stop".to_string(), "announced-stop".to_string())].into()));
        registry.insert_announced("radio", m);
        registry.forget("radio");
        let chain = registry.chain_for("radio", "en", "en");
        assert_eq!(chain.get("play"), "disk-play", "the disk pack must survive forget");
        assert_eq!(chain.get("stop"), "stop", "the announced layer must be gone");
    }

    #[test]
    fn sweep_discovers_a_module_directory_it_was_never_told_about() {
        // The property Task 12 depends on: a language is found because a
        // file is actually sitting on disk, never because `chain_for` was
        // asked about that exact (module, lang) pair. A module named
        // "files" is never mentioned by name anywhere in this test.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        std::fs::write(dir.path().join("files/de.toml"), "browse = \"Durchsuchen\"\n").unwrap();
        let registry = Registry::sweep(dir.path().to_path_buf());
        assert_eq!(registry.chain_for("files", "de", "en").get("browse"), "Durchsuchen");
    }

    #[test]
    fn chain_for_performs_no_disk_i_o_once_swept() {
        // Discriminating proof that `chain_for` reads nothing: sweep while
        // the file exists, delete it, then resolve the same key. If
        // `chain_for` still touched the filesystem, this would now fall
        // through to the key itself instead of the swept value.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        let pack = dir.path().join("radio/nl.toml");
        std::fs::write(&pack, "play = \"Spelen\"\n").unwrap();
        let registry = Registry::sweep(dir.path().to_path_buf());
        std::fs::remove_file(&pack).unwrap();
        assert_eq!(
            registry.chain_for("radio", "nl", "en").get("play"),
            "Spelen",
            "chain_for must answer from the swept snapshot, not re-read the now-missing file"
        );
    }

    #[test]
    fn resweep_picks_up_a_pack_written_after_the_first_sweep() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        assert_eq!(registry.chain_for("radio", "nl", "en").get("play"), "play", "nothing on disk yet");
        std::fs::write(dir.path().join("radio/nl.toml"), "play = \"Spelen\"\n").unwrap();
        assert_eq!(
            registry.chain_for("radio", "nl", "en").get("play"),
            "play",
            "the pack must stay invisible before a resweep"
        );
        registry.resweep();
        assert_eq!(registry.chain_for("radio", "nl", "en").get("play"), "Spelen", "and appear right after one");
    }

    #[test]
    fn resweep_forgets_a_pack_removed_from_disk() {
        // Wholesale replacement, not a merge: a pack an operator deleted
        // must actually disappear, not linger from the previous sweep.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        let pack = dir.path().join("radio/nl.toml");
        std::fs::write(&pack, "play = \"Spelen\"\n").unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        assert_eq!(registry.chain_for("radio", "nl", "en").get("play"), "Spelen");
        std::fs::remove_file(&pack).unwrap();
        registry.resweep();
        assert_eq!(registry.chain_for("radio", "nl", "en").get("play"), "play", "the removed pack must be gone");
    }
}
