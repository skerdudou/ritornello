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
//! per language — and keeps what it finds in memory; `resweep_async`
//! repeats the
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
//! [`sweep_disk`] (wrapped by `Registry::sweep`/`resweep_async`) is the I/O
//! envelope.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ritornello_i18n::{Chain, Layer, ModuleLayers};

/// Registry of every module's translation layers.
///
/// `disk` is populated by [`Registry::sweep`]/[`Registry::resweep_async`] — a walk
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
    /// untouched: it does not come from this root, and a plugin's
    /// announcement is not re-read just because a locale changed. What
    /// `Core::set_locale` and `Core::set_fallback` call.
    ///
    /// The only resweep there is, and async on purpose. A synchronous
    /// `&mut self` twin existed until the final fix round of the
    /// language-packs chantier: it had no production caller left —
    /// `Core::set_locale` moved off it — and, called the way a shared
    /// registry forces (`shared.write().await.resweep()`), it ran the
    /// directory walk and the TOML parse while holding the write lock,
    /// blocking the tokio worker thread and every other reader of the same
    /// registry, `admin::admin_i18n`'s read lock included. Keeping a method
    /// whose only correct number of callers is zero is how that gets done
    /// twice, so it was deleted rather than annotated.
    ///
    /// Two phases, deliberately kept as two calls rather than inlined: the
    /// walk ([`Registry::walk`]) never touches `shared` for anything but a
    /// brief read of `root`, and the write lock is then taken only long
    /// enough to move the freshly swept map into `disk` — a plain
    /// assignment, no I/O, nothing that can block. A concurrent reader
    /// (`admin::admin_i18n`, or another `resweep_async`) is therefore never
    /// made to wait on the walk itself, only ever on that last, negligible
    /// assignment. The split is what
    /// `tests::walk_completes_while_a_reader_holds_the_registry` calls
    /// directly to prove that structurally, rather than by timing.
    ///
    /// Task 4's review named the blocking-under-lock gap this fixes, ahead
    /// of task 5; it went unfixed because task 5 is what first put a second,
    /// HTTP-reachable reader on the same write lock this blocks.
    pub async fn resweep_async(shared: &crate::i18n::Shared) {
        if let Some(disk) = Self::walk(shared).await {
            shared.write().await.disk = disk;
        }
    }

    /// The walk half of [`Registry::resweep_async`]: reads `root` (the only
    /// touch of `shared`, and only ever a read — it coexists with any
    /// number of concurrent readers, never with a writer holding exclusive
    /// access) and then does the directory walk and TOML parse
    /// (`sweep_disk`) in `tokio::task::spawn_blocking`, off the async
    /// runtime's worker threads. Returns `None` if the blocking task
    /// panicked (`JoinError`) rather than returning a fresh, empty map: the
    /// caller then skips the swap and leaves the previous `disk` snapshot as
    /// is — the same "leave what was there" posture `sweep_disk` already
    /// takes for a root that cannot be read at all — rather than losing
    /// every plugin's disk-sourced text over one bad sweep.
    async fn walk(shared: &crate::i18n::Shared) -> Option<HashMap<String, ModuleLayers>> {
        let root = shared.read().await.root.clone();
        match tokio::task::spawn_blocking(move || sweep_disk(&root)).await {
            Ok(disk) => Some(disk),
            Err(e) => {
                tracing::warn!("registry resweep task failed: {e}");
                None
            }
        }
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

    /// Whether `module` has an announced entry at all — `None` if it was
    /// never inserted (or was `forget`ten), `Some(&ModuleLayers)` if it was,
    /// **whatever that `ModuleLayers` holds**, including zero languages.
    ///
    /// This is the accessor that keeps the wire's `None` vs `Some({})`
    /// distinction alive once it reaches the registry: a plugin whose
    /// `Announcement.catalog` was `None` (a binary predating the field)
    /// must never call `insert_announced` at all — so `announced_module`
    /// answers `None` for it, exactly as for a module nobody has ever
    /// mentioned — while a plugin that announced `Some({})` (an up-to-date
    /// binary with no text of its own) calls `insert_announced` with an
    /// empty `ModuleLayers`, so `announced_module` answers `Some` with zero
    /// languages inside. Task 12's completeness denominator needs exactly
    /// this: counting the second case as the first would grow the
    /// denominator every time an old binary went unanswered for, instead of
    /// leaving it out because nothing was ever confided (see
    /// `Announcement.catalog`'s own doc, in `ritornello-proto`).
    ///
    /// **`#[cfg(test)]`, and that is the whole of its status.** No
    /// production code calls it: `Registry::modules_with_text` needs the
    /// same `None`-vs-`Some({})` distinction but reads `self.announced`
    /// directly, one entry at a time, while filtering by name. It used to
    /// be a `pub` method carrying `#[allow(dead_code)]`, which is a dead
    /// producer wearing a permission slip — the class the final fix round
    /// of the language-packs chantier set out to remove, its other member
    /// (`Registry::resweep`) deleted outright. This one is not deleted
    /// because it is *not* dead: it is the only way to tell "never
    /// announced" from "announced empty" from outside, and four guards on
    /// real production paths depend on that distinction — `hotplug`'s and
    /// the startup rendezvous' `if let Some(catalog)`, in both directions
    /// (`main.rs`). Compiling it only for tests says what it is instead of
    /// excusing what it is not.
    #[cfg(test)]
    pub fn announced_module(&self, module: &str) -> Option<&ModuleLayers> {
        self.announced.get(module)
    }

    /// Every module counted as "having text" — task 12's completeness
    /// denominator — as one merged `ModuleLayers` per module, `common`'s
    /// vocabulary folded into each one exactly as `chain_for` would resolve
    /// it (own beats common, disk beats announced, within one language).
    ///
    /// **Membership** comes from the *announced* tier alone, never from a
    /// disk sweep: a module qualifies only if `self.announced` holds an
    /// entry for it with at least one language — the same test
    /// `announced_module`'s own doc already draws between `None` (an old
    /// binary, or a module nobody ever mentioned) and `Some(non-empty)` (a
    /// plugin that actually confided text). A module whose announced entry
    /// is `Some({})` — the four plugins with legitimately no text of their
    /// own (`console`, `ouifm-metas`, `radiofrance-metas`, `nrj-metas` as
    /// of this writing; the brief this shipped against said three, which
    /// was already stale — see the task's own report) — is left out here
    /// too, exactly like a module never announced at all: both are "no
    /// text", and `ritornello_i18n::coverage` carries its own, second guard
    /// against the same fact for a module that reached it some other way
    /// (see that function's doc).
    ///
    /// **`common` never appears as an entry of its own.** It is vocabulary
    /// every module already draws on through `chain_for`; folding it into
    /// every module here is what makes a thin module — one whose own text
    /// is a handful of keys and whose display strings are otherwise all
    /// `common`'s ("Play", "Loading"…) — measure as translated once
    /// `common` and the module's own layer between them cover its English,
    /// rather than reading as untranslated because its own layer alone
    /// does not.
    ///
    /// **A language present only on disk still shows up.** Both the
    /// module's own disk pack and its announced layer are merged for every
    /// language either one defines — an operator-supplied `<lang>.toml`
    /// grows the count exactly as an announced language would, since a
    /// user reading the completeness line cannot tell which tier a
    /// translation came from and should not have to.
    ///
    /// Sorted by module name for a deterministic, testable order — see
    /// `Coverage::modules`'s own doc for why no particular position is
    /// promised beyond that.
    pub fn modules_with_text(&self) -> Vec<ModuleLayers> {
        let mut names: Vec<&str> = self
            .announced
            .iter()
            .filter(|(name, layers)| name.as_str() != "common" && layers.languages().next().is_some())
            .map(|(name, _)| name.as_str())
            .collect();
        names.sort();
        names.into_iter().map(|name| self.merge_with_common(name)).collect()
    }

    /// The same union `ritornello_i18n::union_of_languages(&self
    /// .modules_with_text())` computes — every language at least one
    /// counted module (the same membership as `modules_with_text`, above)
    /// actually translates — but **codes only**, without building a single
    /// merged `Layer`.
    ///
    /// Added for `status_json`'s own clamp on `/api/status.locale` (fix
    /// round 3, task 14 re-review, finding I): that route widened from
    /// `core_languages` to the union in fix round 2 (finding B) to stop
    /// silently narrowing a plugin-only chosen language back to `en`, but
    /// the union it reached for was `modules_with_text`'s — which exists to
    /// answer "what does each language *contain*" (`locale_json`'s own
    /// need, one `Coverage` per language) and pays for that by cloning
    /// every key of every source layer through `merge_with_common`'s
    /// `merged.extend(l.as_map().clone())`, four times per language, per
    /// module. `/api/status` never reads any of that content — it only
    /// ever asks "is this one code among them" — so it was paying
    /// `locale_json`'s own cost on the SPA's most-read route (`useMetrics
    /// .ts`: boot and bounded windows; `usePlugins.ts`: up to twenty reads
    /// after a single plugin toggle) for a question `core_languages` (the
    /// method this exact pattern already exists for, three lines below)
    /// answers by reading map keys alone.
    ///
    /// Mirrors `merge_with_common`'s own enumeration of where a
    /// module + language's text can live — its own announced tier, its own
    /// disk tier, `common`'s announced tier, `common`'s disk tier — and
    /// counts the language the moment any one of those four is non-empty,
    /// borrowed rather than cloned (`ModuleLayers::layer` returns `Option<&
    /// Layer>`; `Layer::is_empty` reads its map's length, nothing more).
    pub fn union_languages(&self) -> Vec<String> {
        let mut set: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (module, own) in
            self.announced.iter().filter(|(name, layers)| name.as_str() != "common" && layers.languages().next().is_some())
        {
            let module = module.as_str();
            let own_disk = self.disk.get(module);
            let common_announced = self.announced.get("common");
            let common_disk = self.disk.get("common");
            let mut langs: Vec<&str> = own
                .languages()
                .chain(own_disk.into_iter().flat_map(|m| m.languages()))
                .chain(common_announced.into_iter().flat_map(|m| m.languages()))
                .chain(common_disk.into_iter().flat_map(|m| m.languages()))
                .collect();
            langs.sort_unstable();
            langs.dedup();
            for lang in langs {
                let has_text = [
                    own.layer(lang),
                    own_disk.and_then(|m| m.layer(lang)),
                    common_announced.and_then(|m| m.layer(lang)),
                    common_disk.and_then(|m| m.layer(lang)),
                ]
                .into_iter()
                .flatten()
                .any(|l| !l.is_empty());
                if has_text {
                    set.insert(lang);
                }
            }
        }
        let mut out: Vec<String> = set.into_iter().map(str::to_string).collect();
        out.sort();
        out
    }

    /// Builds one module's merged view: every language either its own
    /// tiers or `common`'s define, each language's `Layer` built from up to
    /// four sources in `chain_for`'s own priority (last write wins here:
    /// common-announced, common-disk, own-announced, own-disk).
    fn merge_with_common(&self, module: &str) -> ModuleLayers {
        let mut out = ModuleLayers::new(module);
        let mut langs: Vec<&str> = self
            .announced
            .get(module)
            .into_iter()
            .flat_map(|m| m.languages())
            .chain(self.disk.get(module).into_iter().flat_map(|m| m.languages()))
            .chain(self.announced.get("common").into_iter().flat_map(|m| m.languages()))
            .chain(self.disk.get("common").into_iter().flat_map(|m| m.languages()))
            .collect();
        langs.sort_unstable();
        langs.dedup();
        for lang in langs {
            let mut merged: HashMap<String, String> = HashMap::new();
            for l in [
                self.announced_layer("common", lang),
                self.disk_layer("common", lang),
                self.announced_layer(module, lang),
                self.disk_layer(module, lang),
            ]
            .into_iter()
            .flatten()
            {
                merged.extend(l.as_map().clone());
            }
            out.insert(lang.to_string(), Layer::from_map(merged));
        }
        out
    }

    /// The core's own installed languages — `en` (always) plus every
    /// language `core`'s **already-swept** disk tier carries — for the
    /// fallback candidate list (`status::locales::LocaleResponse::
    /// fallback_candidates`): the owner's arbitration reserves a fallback
    /// to what is guaranteed to resolve everywhere, never a plugin-only
    /// language, which is exactly the narrower set `modules_with_text`'s
    /// union is not.
    ///
    /// Reads `self.disk` — the snapshot `Registry::sweep`/`resweep_async` already
    /// built — rather than a live `std::fs::read_dir` of the pack root.
    /// This replaced a route that read the two answers from two different
    /// places: `locale_json` used to call a live, disk-reading
    /// `list_locales` for `fallback_candidates` in the very same response
    /// that built `locales`/`completeness` from this registry's swept
    /// snapshot. The live read was not actually fresher in any way that
    /// mattered — `Registry::chain_for`, what a chosen fallback would
    /// *actually* resolve through, only ever sees post-sweep state — so the
    /// two could disagree, and a device could be offered a fallback
    /// candidate that silently did not resolve until the next sweep. Task
    /// 12's review named this and its sibling call site in
    /// `status::status_json` (task 12's own report, "F-1"/"F-2"); both now
    /// read this one method instead.
    ///
    /// Only `core`'s module directory is consulted — never `common`'s, and
    /// never the announced tier, which for `core` only ever contributes
    /// `en` in the first place (`crate::i18n::core_module_layers`), already
    /// covered by the unconditional prefix below.
    pub fn core_languages(&self) -> Vec<String> {
        let mut out = vec!["en".to_string()];
        if let Some(core) = self.disk.get("core") {
            let mut rest: Vec<&str> = core.languages().filter(|l| *l != "en").collect();
            rest.sort_unstable();
            out.extend(rest.into_iter().map(str::to_string));
        }
        out
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
    fn announced_module_distinguishes_never_inserted_from_inserted_empty() {
        // The discriminating property Task 12's denominator depends on: a
        // module that was never `insert_announced`d (an old binary, or one
        // never asked) must read `None`, never merely `Some` of an empty
        // `ModuleLayers` — the two facts are not interchangeable even
        // though both currently resolve zero keys through `chain_for`.
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        assert_eq!(registry.announced_module("console"), None, "never announced at all");

        registry.insert_announced("console", ModuleLayers::new("console"));
        let announced = registry.announced_module("console");
        assert!(announced.is_some(), "announced, even with nothing to say, must read Some");
        assert_eq!(announced.unwrap().languages().count(), 0, "and carry zero languages, not invent one");
    }

    // --- modules_with_text: task 12's denominator source ---

    fn module_layers(name: &str, langs: &[(&str, &[(&str, &str)])]) -> ModuleLayers {
        let mut m = ModuleLayers::new(name);
        for (lang, pairs) in langs {
            let source: String = pairs.iter().map(|(k, v)| format!("{k} = {v:?}\n")).collect();
            m.insert(*lang, Layer::parse(&source).unwrap());
        }
        m
    }

    #[test]
    fn modules_with_text_excludes_a_module_never_announced() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")])]));
        // "console" is never mentioned at all: `announced_module("console")`
        // would read `None`, exactly like an old binary.
        let names: Vec<String> = registry.modules_with_text().into_iter().map(|m| m.name().to_string()).collect();
        assert_eq!(names, vec!["radio".to_string()]);
    }

    #[test]
    fn modules_with_text_excludes_a_module_announced_empty() {
        // The `Some({})` half of the distinction: a plugin that connected
        // and announced, but confided nothing (the four legitimately
        // textless plugins), must not inflate the denominator either.
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")])]));
        registry.insert_announced("console", ModuleLayers::new("console"));
        let names: Vec<String> = registry.modules_with_text().into_iter().map(|m| m.name().to_string()).collect();
        assert_eq!(names, vec!["radio".to_string()], "console announced Some({{}}) and must still be excluded");
    }

    #[test]
    fn modules_with_text_never_lists_common_as_a_module_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")])]));
        registry.insert_announced("common", module_layers("common", &[("en", &[("loading", "Loading")])]));
        let names: Vec<String> = registry.modules_with_text().into_iter().map(|m| m.name().to_string()).collect();
        assert_eq!(names, vec!["radio".to_string()], "common must never appear as an entry of its own");
    }

    #[test]
    fn modules_with_text_folds_common_into_every_module_it_returns() {
        // A module whose own French is silent but whose English is
        // entirely covered by `common`'s vocabulary must still measure as
        // translated once `common`'s French is folded in — see
        // `merge_with_common`'s own doc for why this is not double
        // counting.
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("loading", "Loading")])]));
        registry.insert_announced("common", module_layers("common", &[("en", &[("loading", "Loading")]), ("fr", &[("loading", "Chargement")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("fr").and_then(|l| l.get("loading")), Some("Chargement"));
    }

    #[test]
    fn modules_with_text_includes_a_language_that_exists_only_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/de.toml"), "play = \"Spielen\"\n").unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("de").and_then(|l| l.get("play")), Some("Spielen"));
    }

    #[test]
    fn modules_with_text_own_layer_wins_over_common_within_one_language() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "own-play")])]));
        registry.insert_announced("common", module_layers("common", &[("en", &[("play", "common-play")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("en").and_then(|l| l.get("play")), Some("own-play"));
    }

    /// [MUTATION] The other half of `merge_with_common`'s documented
    /// priority ("disk beats announced, within one language"), which
    /// `modules_with_text_own_layer_wins_over_common_within_one_language`
    /// above does not touch — that test only ever exercises the
    /// own-vs-common axis, with both sides on the *announced* tier. A
    /// silent reordering of the four-source array (`merge_with_common`'s
    /// `for l in [...]`) that swapped `own_announced` and `own_disk` would
    /// pass every other test in this file and still be caught by nothing
    /// without this one.
    #[test]
    fn modules_with_text_own_disk_beats_own_announced_within_one_language() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/en.toml"), "play = \"disk-play\"\n").unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "announced-play")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("en").and_then(|l| l.get("play")), Some("disk-play"));
    }

    #[test]
    fn modules_with_text_is_sorted_by_module_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("k", "v")])]));
        registry.insert_announced("cd", module_layers("cd", &[("en", &[("k", "v")])]));
        registry.insert_announced("core", module_layers("core", &[("en", &[("k", "v")])]));
        let names: Vec<String> = registry.modules_with_text().into_iter().map(|m| m.name().to_string()).collect();
        assert_eq!(names, vec!["cd".to_string(), "core".to_string(), "radio".to_string()]);
    }

    // --- core_languages: the fallback candidate list (task 12's F-1/F-2) ---

    #[test]
    fn core_languages_always_includes_en_even_with_nothing_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::sweep(dir.path().to_path_buf());
        assert_eq!(registry.core_languages(), vec!["en".to_string()]);
    }

    #[test]
    fn core_languages_picks_up_a_swept_disk_pack_sorted_after_en() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/nl.toml"), "play = \"Spelen\"\n").unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "play = \"Lecture\"\n").unwrap();
        let registry = Registry::sweep(dir.path().to_path_buf());
        assert_eq!(registry.core_languages(), vec!["en".to_string(), "fr".to_string(), "nl".to_string()]);
    }

    #[test]
    fn core_languages_ignores_a_plugin_only_language() {
        // The narrower half of the union/fallback split: a language only
        // "radio" translates must not leak into the fallback candidates,
        // which the owner reserves to what the core itself ships.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/de.toml"), "play = \"Spielen\"\n").unwrap();
        let registry = Registry::sweep(dir.path().to_path_buf());
        assert_eq!(registry.core_languages(), vec!["en".to_string()]);
    }

    #[test]
    fn core_languages_reads_the_swept_snapshot_not_a_live_directory() {
        // Discriminating proof for the reason this method exists at all:
        // a pack written to disk *after* the sweep must stay invisible
        // until a resweep, exactly like `chain_for`'s own no-I/O guarantee
        // — a fallback candidate list built from a live read could
        // otherwise offer a language `chain_for` cannot resolve yet.
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::sweep(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/de.toml"), "play = \"Spielen\"\n").unwrap();
        assert_eq!(
            registry.core_languages(),
            vec!["en".to_string()],
            "a pack written after the sweep must not appear before a resweep"
        );
    }

    // --- union_languages: the cheap union (fix round 3, finding I) ---

    #[test]
    fn union_languages_includes_a_plugin_only_language() {
        // The exact case `core_languages` (just above) deliberately
        // excludes — the narrower half of the union/fallback split. Proves
        // this new accessor answers `locale_json`'s question
        // (`status_json`'s clamp needs it too, since fix round 2), not
        // `core_languages`'s.
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]));
        assert!(registry.union_languages().contains(&"de".to_string()));
    }

    #[test]
    fn union_languages_excludes_a_module_never_announced() {
        // Mirrors `modules_with_text_excludes_a_module_never_announced`:
        // membership comes from the announced tier alone, same as the
        // expensive path this accessor replaces.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("console")).unwrap();
        std::fs::write(dir.path().join("console/de.toml"), "k = \"v\"\n").unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")])]));
        // "console" has a disk pack but was never announced: `de` must not
        // leak in through it.
        assert!(!registry.union_languages().contains(&"de".to_string()));
    }

    #[test]
    fn union_languages_matches_the_expensive_computation_it_replaces() {
        // The equivalence this accessor exists to preserve, checked
        // directly rather than only through the HTTP route: own tiers,
        // disk tiers, and `common`'s own two tiers, all contributing
        // distinct languages, core-only and plugin-only alike.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/nl.toml"), "play = \"Spelen\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("common")).unwrap();
        std::fs::write(dir.path().join("common/it.toml"), "ok = \"Ok\"\n").unwrap();
        let mut registry = Registry::sweep(dir.path().to_path_buf());
        registry.insert_announced("core", module_layers("core", &[("en", &[("k", "v")])]));
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]));
        registry.insert_announced("common", module_layers("common", &[("en", &[("ok", "Ok")]), ("es", &[("ok", "Vale")])]));

        let cheap = registry.union_languages();
        let expensive = ritornello_i18n::union_of_languages(&registry.modules_with_text());
        assert_eq!(cheap, expensive);
        // Not a vacuous match: every source tier contributed something the
        // other three did not (nl from radio's disk pack, it from
        // common's disk pack, es from common's announced tier, de from
        // radio's announced tier), so an implementation that silently
        // dropped one source would diverge from `expensive`, not merely
        // return an empty list either side agrees on.
        for lang in ["en", "de", "nl", "it", "es"] {
            assert!(cheap.contains(&lang.to_string()), "{lang} missing from {cheap:?}");
        }
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

    /// Wholesale replacement, not a merge: a pack an operator deleted must
    /// actually disappear, not linger from the previous sweep. The mirror
    /// of `resweep_async_picks_up_a_pack_written_after_the_first_sweep`
    /// just below, and both go through the real path — the synchronous
    /// `resweep` these two facts used to be pinned against was deleted for
    /// having no production caller.
    #[tokio::test]
    async fn resweep_async_forgets_a_pack_removed_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        let pack = dir.path().join("radio/nl.toml");
        std::fs::write(&pack, "play = \"Spelen\"\n").unwrap();
        let shared: crate::i18n::Shared =
            std::sync::Arc::new(tokio::sync::RwLock::new(Registry::sweep(dir.path().to_path_buf())));
        assert_eq!(shared.read().await.chain_for("radio", "nl", "en").get("play"), "Spelen");
        std::fs::remove_file(&pack).unwrap();
        Registry::resweep_async(&shared).await;
        assert_eq!(
            shared.read().await.chain_for("radio", "nl", "en").get("play"),
            "play",
            "the removed pack must be gone"
        );
    }

    /// Functional coverage for `resweep_async`: the same fact
    /// `resweep_picks_up_a_pack_written_after_the_first_sweep` pins for the
    /// synchronous method, through the async/`Shared` path `Core::set_locale`
    /// actually uses. Correctness of the *result*, not of the locking
    /// discipline — see `walk_completes_while_a_reader_holds_the_registry`,
    /// below, for the structural proof of that.
    #[tokio::test]
    async fn resweep_async_picks_up_a_pack_written_after_the_first_sweep() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        let shared: crate::i18n::Shared =
            std::sync::Arc::new(tokio::sync::RwLock::new(Registry::sweep(dir.path().to_path_buf())));
        assert_eq!(
            shared.read().await.chain_for("radio", "nl", "en").get("play"),
            "play",
            "nothing on disk yet"
        );

        std::fs::write(dir.path().join("radio/nl.toml"), "play = \"Spelen\"\n").unwrap();
        Registry::resweep_async(&shared).await;

        assert_eq!(
            shared.read().await.chain_for("radio", "nl", "en").get("play"),
            "Spelen",
            "picked up after an async resweep"
        );
    }

    /// The structural property `resweep_async`'s fix rests on, proved
    /// deterministically rather than by timing: `Registry::walk` never
    /// needs exclusive access to `shared`, only ever a read (to learn
    /// `root`), so it must complete even while a reader holds the registry
    /// for the whole test — a read guard taken here and never dropped until
    /// the function returns. No sleep, no poll loop: either `walk` asks for
    /// the write lock at some point, in which case this deadlocks (turned
    /// into a clean failure by the `timeout` below, a safety net against a
    /// hung test run, not a timing assertion), or it does not, in which case
    /// it returns regardless of how long the held guard lives.
    ///
    /// This is what discriminates against the regression task 4's review
    /// named and task 5 made reachable: moving the walk back under the
    /// write lock (fold `walk` and `resweep_async` back into the one
    /// single write-locked call `Core::set_locale` used to
    /// make) is exactly what would make this test hang instead of return.
    #[tokio::test]
    async fn walk_completes_while_a_reader_holds_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/nl.toml"), "play = \"Spelen\"\n").unwrap();
        let shared: crate::i18n::Shared =
            std::sync::Arc::new(tokio::sync::RwLock::new(Registry::sweep(dir.path().to_path_buf())));

        // Held for the rest of the test: a real writer (the swap half of
        // `resweep_async`) could never be granted the lock while this is
        // alive. The walk must not care.
        let _read_guard = shared.read().await;

        let disk = tokio::time::timeout(std::time::Duration::from_secs(5), Registry::walk(&shared))
            .await
            .expect("the walk must never need to wait on a guard the test itself holds")
            .expect("the blocking task must not panic on a readable root");

        assert_eq!(
            disk.get("radio").and_then(|m| m.layer("nl")).and_then(|l| l.get("play")),
            Some("Spelen"),
            "the walk must still have read the real pack, not a stand-in"
        );
    }
}
