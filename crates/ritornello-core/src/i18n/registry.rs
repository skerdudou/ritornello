//! The core's registry of translation layers.
//!
//! One `Registry` holds, per module (the core itself, or one plugin), two
//! kinds of layers, strongest first: what an **installed language pack**
//! carries, under its own root (`Registry.packs_root`); and what a module
//! **announced** — a plugin's embedded catalogue (`Announcement.catalog`),
//! or the core's own embedded text, folded in through the same path.
//! `chain_for` stacks both kinds, for up to three languages, into the one
//! `Chain` the caller resolves keys against.
//!
//! **The pack tier is never written by anything but an install.** One
//! directory per pack, under `Registry.packs_root`; only that root is ever
//! touched by `crate::langpack::store::install`/`remove`. There used to be a
//! second, operator-owned root that outranked every pack — hand-written
//! `<module>/<lang>.toml` files an update could never overwrite. The owner
//! judged that layer not worth its weight and removed it: nothing reads
//! `/etc/ritornello/locales` any more, and an archive that carries it is no
//! longer an allowed shape (`update::archive::ETC_PREFIXES`).
//!
//! **The packs root is swept once, not read per call.** `Registry::sweep`
//! inventories the packs root (`crate::langpack::store::inventory`), keeping
//! what it finds in memory; `resweep_async` repeats that walk and replaces
//! the snapshot. `chain_for` itself performs **no I/O at all**: every module
//! it might be asked about has already been read once, at sweep time. This
//! matters three times over — an HTTP route must never block on disk, only
//! a real sweep (not a guessed path) can ever tell a later reader which
//! languages exist at all, and the refresh gesture this crate already
//! documents ("an operator can install a pack, and the restart is what
//! refreshes it") stays true instead of quietly becoming "on every
//! request".
//!
//! The pack read is separated from the stacking calculation on purpose, the
//! same split `status::locales::list_locales` already draws against
//! `parse_available_locales` and `audio_output::list_devices` against
//! `parse_device_list`: [`Registry::sources_for`] and [`Registry::languages_of`]
//! are the pure calculation — the one place the four tiers (own and
//! `common`'s, times pack/announced) and their strongest-first order are
//! enumerated — and `crate::langpack::store::inventory` (wrapped by
//! `Registry::sweep`/`resweep_async`) is the I/O envelope. Most of the tests
//! below drive that pure half through a real `Registry` and `chain_for`;
//! `sources_for_lists_the_tiers_strongest_first` calls `sources_for`
//! directly to pin the order it fixes.

use std::collections::HashMap;
use std::path::PathBuf;

use ritornello_i18n::{Chain, Layer, ModuleLayers};

/// Registry of every module's translation layers.
///
/// `packs` is populated by [`Registry::sweep`]/[`Registry::resweep_async`] —
/// every installed language pack, strongest first among packs of the same
/// language (a fact livraison 1 never exercises, since it installs at most
/// one pack per language, all of them ours). `announced` is populated by
/// `insert_announced` — called once per plugin announcement, and once for
/// the core's own embedded text and for `common`'s, so that `chain_for`
/// treats every module uniformly rather than special-casing the core.
/// Neither `packs` nor `announced` is read from the filesystem by
/// `chain_for` itself.
#[derive(Debug, Default)]
pub struct Registry {
    packs_root: PathBuf,
    /// The installed packs, strongest first. Livraison 1 puts at most one
    /// pack per language here, all of them ours; livraison 2 is the single
    /// place that will order several.
    packs: Vec<crate::langpack::store::InstalledPack>,
    announced: HashMap<String, ModuleLayers>,
}

impl Registry {
    /// Sweeps the packs root once and returns a registry holding what it
    /// found, with no announced module yet.
    pub fn sweep(packs_root: PathBuf) -> Registry {
        let packs = crate::langpack::store::inventory(&packs_root);
        Registry { packs_root, packs, announced: HashMap::new() }
    }

    /// Every installed language pack, in the order `sources_for` consults
    /// them.
    pub fn installed_packs(&self) -> &[crate::langpack::store::InstalledPack] {
        &self.packs
    }

    /// Repeats the walk of the packs root and replaces the pack tier —
    /// wholesale, not merged, so a pack removed since the last sweep is
    /// actually forgotten rather than lingering. The announced tier is
    /// untouched: it does not come from that root, and a plugin's
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
    /// brief read of `packs_root`, and the write lock is then taken only
    /// long enough to move the freshly swept list into `packs` — a plain
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
        if let Some(packs) = Self::walk(shared).await {
            let mut w = shared.write().await;
            w.packs = packs;
        }
    }

    /// The walk half of [`Registry::resweep_async`]: reads `packs_root` (the
    /// only touch of `shared`, and only ever a read — it coexists with any
    /// number of concurrent readers, never with a writer holding exclusive
    /// access) and then does the directory walk and TOML parse
    /// (`crate::langpack::store::inventory`) in `tokio::task::spawn_blocking`,
    /// off the async runtime's worker threads. Returns `None` if the
    /// blocking task panicked (`JoinError`) rather than returning a fresh,
    /// empty list: the caller then skips the swap and leaves the previous
    /// `packs` snapshot as is — the same "leave what was there" posture
    /// `inventory` already takes for a root that cannot be read at all —
    /// rather than losing every plugin's pack-sourced text over one bad
    /// sweep.
    async fn walk(shared: &crate::i18n::Shared) -> Option<Vec<crate::langpack::store::InstalledPack>> {
        let packs_root = { shared.read().await.packs_root.clone() };
        match tokio::task::spawn_blocking(move || crate::langpack::store::inventory(&packs_root)).await {
            Ok(packs) => Some(packs),
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
    /// was uninstalled. Its installed packs, if any, are untouched: the next
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
    /// it (own beats common, pack beats announced, within one language).
    ///
    /// **Membership** comes from the *announced* tier alone, never from the
    /// pack sweep: a module qualifies only if `self.announced` holds an
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
    /// **A language present only in a pack still shows up.** Both the
    /// module's own installed pack and its announced layer are merged for
    /// every language either one defines — an installed pack grows the
    /// count exactly as an announced language would, since a user reading
    /// the completeness line cannot tell which tier a translation came from
    /// and should not have to.
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
    /// pack tier, `common`'s announced tier, `common`'s pack tier — and
    /// counts the language the moment any one of those four is non-empty,
    /// borrowed rather than cloned (`ModuleLayers::layer` returns `Option<&
    /// Layer>`; `Layer::is_empty` reads its map's length, nothing more).
    pub fn union_languages(&self) -> Vec<String> {
        let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut modules: Vec<&str> = self
            .announced
            .iter()
            .filter(|(name, layers)| name.as_str() != "common" && layers.languages().next().is_some())
            .map(|(name, _)| name.as_str())
            .collect();
        modules.sort_unstable();
        for module in modules {
            for lang in self.languages_of(module) {
                if self.sources_for(module, &lang).iter().any(|l| !l.is_empty()) {
                    set.insert(lang);
                }
            }
        }
        let mut out: Vec<String> = set.into_iter().collect();
        out.sort();
        out
    }

    /// Builds one module's merged view: every language either its own
    /// tiers or `common`'s define, each language's `Layer` built from every
    /// source `sources_for` returns — own pack(s), own announced, then
    /// `common`'s pack(s), `common`'s announced — folded in reverse of that
    /// order (weakest first) so `HashMap::extend`'s last-write-wins
    /// overwrite reproduces `sources_for`'s own strongest-first priority.
    fn merge_with_common(&self, module: &str) -> ModuleLayers {
        let mut out = ModuleLayers::new(module);
        for lang in self.languages_of(module) {
            let mut merged: HashMap<String, String> = HashMap::new();
            for l in self.sources_for(module, &lang).into_iter().rev() {
                merged.extend(l.as_map().clone());
            }
            out.insert(lang.clone(), Layer::from_map(merged));
        }
        out
    }

    /// The core's own installed languages — `en` (always) plus every
    /// language an installed pack carries for `core` — for the fallback
    /// candidate list (`status::locales::LocaleResponse::fallback_candidates`):
    /// the owner's arbitration reserves a fallback to what is guaranteed to
    /// resolve everywhere, never a plugin-only language, which is exactly
    /// the narrower set `modules_with_text`'s union is not.
    ///
    /// Reads `self.packs` and `self.announced` — the in-memory snapshots
    /// `Registry::sweep`/`resweep_async` and `insert_announced` already
    /// built — rather than a live `std::fs::read_dir` of the packs root.
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
    /// **Only `core`'s own tiers are consulted — never `common`'s.**
    /// `languages_of("core")` walks every source `sources_for` knows about,
    /// `common`'s included, so the filter below is load-bearing: it keeps a
    /// language only when `core`'s own installed language pack or `core`'s
    /// own announced layer actually carries it, which is what excludes a
    /// language that only `common` speaks. The result no longer equals
    /// "`en` alone" the moment a pack ships a second language for `core`, by
    /// design: `crate::i18n::core_module_layers` never inserts anything but
    /// `"en"` into `core`'s announced layer, so that tier never has a second
    /// language to contribute, but the pack tier routinely does (see
    /// `a_core_language_carried_by_a_pack_is_offered_as_a_fallback`, below).
    pub fn core_languages(&self) -> Vec<String> {
        let mut out = vec!["en".to_string()];
        let mut rest: Vec<String> = self
            .languages_of("core")
            .into_iter()
            .filter(|l| l != "en")
            // A language `common` alone carries is not a language the core
            // can be relied on to speak: the owner's rule reserves a
            // fallback to what resolves everywhere.
            .filter(|l| {
                self.announced_layer("core", l).is_some_and(|x| !x.is_empty())
                    || self.pack_layers("core", l).any(|x| !x.is_empty())
            })
            .collect();
        rest.sort();
        out.extend(rest);
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
    /// even tried. Within one language, the historical order holds: pack
    /// before announced, the module's own vocabulary before `common`'s.
    ///
    /// `chosen`, `fallback` and `en` may coincide (typically `fallback` is
    /// itself `"en"` until a device has a real fallback setting); the
    /// resulting duplicate layers are harmless, `Chain::get` only ever
    /// needs the first match.
    ///
    /// **Performs no I/O.** Both tiers it reads from — `packs` and
    /// `announced` — are already in memory; see the module doc.
    pub fn chain_for(&self, module: &str, chosen: &str, fallback: &str) -> Chain {
        let mut layers = self.sources_for(module, chosen);
        layers.extend(self.sources_for(module, fallback));
        layers.extend(self.sources_for(module, "en"));
        Chain::new(layers)
    }

    /// Every layer that can answer for `module` in `lang`, **strongest
    /// first**: an installed pack, then the announced (embedded) text — each
    /// of those for `module` itself before `common`'s.
    ///
    /// **The one place the tiers are enumerated.** They used to be listed at
    /// four sites — the chain, the merge, the language union and the core's
    /// own list — each restating the same order in its own words. That is
    /// the shape this repository has been bitten by six times: a rule
    /// changed at three of its sites out of four, silently. Everything that
    /// needs to know where text can come from goes through here.
    fn sources_for(&self, module: &str, lang: &str) -> Vec<Layer> {
        let mut out = Vec::new();
        out.extend(self.pack_layers(module, lang));
        out.extend(self.announced_layer(module, lang));
        out.extend(self.pack_layers("common", lang));
        out.extend(self.announced_layer("common", lang));
        out
    }

    /// Every language some source defines something for `module`, sorted and
    /// deduplicated. The companion of `sources_for`: that one answers "what
    /// can speak", this one "in which languages".
    fn languages_of(&self, module: &str) -> Vec<String> {
        let mut langs: Vec<String> = [self.announced.get(module), self.announced.get("common")]
            .into_iter()
            .flatten()
            .flat_map(|m| m.languages())
            .map(str::to_string)
            .collect();
        for p in &self.packs {
            if p.layers.iter().any(|(m, _)| m == module || m == "common") {
                langs.push(p.manifest.language.clone());
            }
        }
        langs.sort();
        langs.dedup();
        langs
    }

    /// One pack's layer for a module, if that pack carries both this
    /// language and this module. Several packs can each answer, so this
    /// returns an iterator rather than the single `Option<Layer>` the other
    /// two source accessors do.
    fn pack_layers(&self, module: &str, lang: &str) -> impl Iterator<Item = Layer> + '_ {
        let module = module.to_string();
        let lang = lang.to_string();
        self.packs.iter().filter_map(move |p| {
            if p.manifest.language != lang {
                return None;
            }
            p.layers.iter().find(|(m, _)| *m == module).map(|(_, l)| l.clone())
        })
    }

    fn announced_layer(&self, module: &str, lang: &str) -> Option<Layer> {
        self.announced.get(module).and_then(|m| m.layer(lang)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // The four tests below each isolate ONE boundary of the order
    // `sources_for` now fixes in the single place it is enumerated: a test
    // that only checked the final answer without controlling every other
    // layer would not tell us which boundary, if any, actually held. They
    // used to build a `LanguageBlock` by hand and call the now-deleted
    // `stack` directly; routing them through a real `Registry` and
    // `chain_for` instead exercises the same boundaries end to end, through
    // the one code path `sources_for`/`languages_of` collapsed the four
    // former call sites into.

    /// A small helper: writes a one-module, one-language pack directly under
    /// `packs_root`, in the exact shape `crate::langpack::store::inventory`
    /// reads back (`pack.toml` plus one `<module>.toml`) — the same
    /// convention every test below that needs an installed pack uses,
    /// rather than each one re-deriving the manifest TOML by hand.
    fn write_pack(packs_root: &Path, lang: &str, module: &str, body: &str) {
        let dir = packs_root.join(format!("ritornello-lang-{lang}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.toml"),
            format!("language = \"{lang}\"\nversion = \"0.2.0\"\nsource = \"x\"\nmodules = [\"{module}\"]\n"),
        )
        .unwrap();
        std::fs::write(dir.join(format!("{module}.toml")), body).unwrap();
    }

    #[test]
    fn within_one_language_a_pack_beats_announced() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "fr", "radio", "k = \"from-pack\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut radio = ModuleLayers::new("radio");
        radio.insert("fr", Layer::from_map([("k".to_string(), "from-announced".to_string())].into()));
        registry.insert_announced("radio", radio);
        assert_eq!(registry.chain_for("radio", "fr", "fr").get("k"), "from-pack");
    }

    #[test]
    fn within_one_language_the_module_s_own_vocabulary_beats_common() {
        // Deliberately own_announced (the module's *weaker* tier) against
        // common_pack (common's *stronger* tier): if this still resolves
        // to the module's own value, "own beats common" holds regardless
        // of which tier either side happens to use — not just in the case
        // where own also happens to be a pack.
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "fr", "common", "k = \"common-pack\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut radio = ModuleLayers::new("radio");
        radio.insert("fr", Layer::from_map([("k".to_string(), "own-announced".to_string())].into()));
        registry.insert_announced("radio", radio);
        assert_eq!(registry.chain_for("radio", "fr", "fr").get("k"), "own-announced");
    }

    #[test]
    fn the_chosen_language_wins_over_specificity_even_against_a_more_specific_fallback() {
        // The one test that proves the chantier's central arbitration.
        // `fallback`'s layer here is the module's own installed pack — the
        // single most specific layer that exists anywhere in this order —
        // while `chosen` only has common's announced layer, the least
        // specific of all four. If `chosen` still wins, the language
        // genuinely dominates specificity.
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "nl", "radio", "k = \"fallback-own-pack\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut common = ModuleLayers::new("common");
        common.insert("fr", Layer::from_map([("k".to_string(), "chosen-common-announced".to_string())].into()));
        registry.insert_announced("common", common);
        assert_eq!(registry.chain_for("radio", "fr", "nl").get("k"), "chosen-common-announced");
    }

    #[test]
    fn the_fallback_language_beats_english() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "nl", "radio", "k = \"fallback-value\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut radio = ModuleLayers::new("radio");
        radio.insert("en", Layer::from_map([("k".to_string(), "english-value".to_string())].into()));
        registry.insert_announced("radio", radio);
        // "de" is the chosen language, and carries nothing at all — the
        // pure equivalent of the deleted test's empty `chosen` block.
        assert_eq!(registry.chain_for("radio", "de", "nl").get("k"), "fallback-value");
    }

    #[test]
    fn an_unknown_key_still_resolves_to_itself() {
        // The safety net `Chain::get` already carries must survive being
        // reached through a registry with nothing in it at all.
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::sweep(dir.path().join("packs"));
        assert_eq!(registry.chain_for("radio", "fr", "en").get("nope"), "nope");
    }

    /// The resolution order, stated once as a test rather than four times as
    /// a comment: own-pack beats own-announced beats common-pack beats
    /// common-announced, inside one language. This is what `sources_for`
    /// must keep when it becomes the single site the tiers are listed at.
    #[test]
    fn sources_for_lists_the_tiers_strongest_first() {
        let packs = tempfile::tempdir().unwrap();
        // One real pack covering two modules, `radio` and `common` — the
        // shape a language pack actually has (`modules` is a list): the
        // point here is one pack answering for both, not two packs.
        let dir = packs.path().join("ritornello-lang-fr");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.toml"),
            "language = \"fr\"\nversion = \"0.2.0\"\nsource = \"x\"\nmodules = [\"radio\", \"common\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("radio.toml"), "k = \"own-pack\"\n").unwrap();
        std::fs::write(dir.join("common.toml"), "k = \"common-pack\"\n").unwrap();
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut radio = ModuleLayers::new("radio");
        radio.insert("fr", Layer::from_map([("k".to_string(), "own-announced".to_string())].into()));
        registry.insert_announced("radio", radio);
        let mut common = ModuleLayers::new("common");
        common.insert("fr", Layer::from_map([("k".to_string(), "common-announced".to_string())].into()));
        registry.insert_announced("common", common);

        let layers = registry.sources_for("radio", "fr");
        let got: Vec<&str> = layers.iter().filter_map(|l| l.get("k")).collect();
        assert_eq!(got, vec!["own-pack", "own-announced", "common-pack", "common-announced"]);
    }

    // --- The pack tier: a second, separate root, ranked above the
    // announced (embedded) one. The operator's own hand-written layer that
    // used to outrank both is gone (the owner's decision, 2026-09-23): see
    // this module's own doc. ---

    /// The partial-override property: a pack that redefines one key must
    /// not make the rest of its module fall back to English. Stated here
    /// because it is what makes layers stack rather than replace, and
    /// livraison 2's third-party packs depend on it entirely.
    #[test]
    fn a_pack_that_defines_one_key_does_not_hide_announced_text_for_the_rest() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "fr", "radio", "a = \"pack-a\"\nb = \"pack-b\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut radio = ModuleLayers::new("radio");
        radio.insert(
            "fr",
            Layer::from_map(
                [("a".to_string(), "announced-a".to_string()), ("c".to_string(), "announced-c".to_string())].into(),
            ),
        );
        registry.insert_announced("radio", radio);
        let chain = registry.chain_for("radio", "fr", "en");
        assert_eq!(chain.get("a"), "pack-a", "the pack wins the key both tiers define");
        assert_eq!(chain.get("b"), "pack-b", "the pack answers for a key announced never had");
        assert_eq!(chain.get("c"), "announced-c", "announced still answers for what the pack left alone");
    }

    /// **The regression the spec named in advance.** The fallback control
    /// offers the core's languages, and the core's French comes from a
    /// pack. A `core_languages` that consulted only an announced tier would
    /// empty that control in silence.
    #[test]
    fn a_core_language_carried_by_a_pack_is_offered_as_a_fallback() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "fr", "core", "k = \"v\"\n");
        let registry = Registry::sweep(packs.path().to_path_buf());
        assert_eq!(registry.core_languages(), vec!["en".to_string(), "fr".to_string()]);
    }

    /// A pack for a plugin the device does not have stays invisible: it
    /// creates no ghost language and no hole in any count, and it is
    /// already in place the day that plugin is installed. This is what
    /// makes "a pack carries every module" free rather than costly.
    #[test]
    fn a_pack_for_an_absent_module_creates_no_language_and_no_hole() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "de", "mpd", "k = \"v\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut core = ModuleLayers::new("core");
        core.insert("en", Layer::from_map([("k".to_string(), "v".to_string())].into()));
        registry.insert_announced("core", core);
        assert_eq!(registry.union_languages(), vec!["en".to_string()], "no ghost German");
    }

    /// [MUTATION] The language filter in `pack_layers`, isolated from the
    /// module filter that sits beside it. The test above
    /// (`a_pack_for_an_absent_module_creates_no_language_and_no_hole`) does
    /// not discriminate against `pack_layers` dropping its
    /// `p.manifest.language != lang` guard: its German pack never carries
    /// the "core" module at all, so `p.layers.iter().find(|(m, _)| *m ==
    /// module)` already returns `None` before the language check would ever
    /// run — measured by actually deleting that guard and re-running this
    /// file's suite, which stayed green. Here the German pack *does* carry
    /// "core", so only the language guard stands between an English
    /// resolution and a German pack answering it.
    #[test]
    fn a_pack_never_answers_for_a_language_it_does_not_carry() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "de", "core", "k = \"v\"\n");
        let registry = Registry::sweep(packs.path().to_path_buf());
        let chain = registry.chain_for("core", "en", "en");
        assert_eq!(chain.get("k"), "k", "a pack declaring German must never answer an English request");
    }

    // --- Registry itself: proving the sweep and `chain_for` are actually
    // wired together, rather than testing the ordering a second time. ---

    #[test]
    fn chain_for_reads_an_installed_pack_for_the_requested_module_and_language() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "nl", "radio", "play = \"Spelen\"\n");
        let registry = Registry::sweep(packs.path().to_path_buf());
        let chain = registry.chain_for("radio", "nl", "en");
        assert_eq!(chain.get("play"), "Spelen");
    }

    #[test]
    fn chain_for_uses_a_layer_inserted_via_insert_announced() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().join("packs"));
        let mut m = ModuleLayers::new("radio");
        m.insert("en", Layer::from_map([("play".to_string(), "Play".to_string())].into()));
        registry.insert_announced("radio", m);
        let chain = registry.chain_for("radio", "en", "en");
        assert_eq!(chain.get("play"), "Play");
    }

    #[test]
    fn forget_removes_the_announced_layer_but_not_the_installed_pack() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "en", "radio", "play = \"pack-play\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        let mut m = ModuleLayers::new("radio");
        m.insert("en", Layer::from_map([("stop".to_string(), "announced-stop".to_string())].into()));
        registry.insert_announced("radio", m);
        registry.forget("radio");
        let chain = registry.chain_for("radio", "en", "en");
        assert_eq!(chain.get("play"), "pack-play", "the installed pack must survive forget");
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
        let mut registry = Registry::sweep(dir.path().join("packs"));
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
        let mut registry = Registry::sweep(dir.path().join("packs"));
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
        let mut registry = Registry::sweep(dir.path().join("packs"));
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")])]));
        registry.insert_announced("console", ModuleLayers::new("console"));
        let names: Vec<String> = registry.modules_with_text().into_iter().map(|m| m.name().to_string()).collect();
        assert_eq!(names, vec!["radio".to_string()], "console announced Some({{}}) and must still be excluded");
    }

    #[test]
    fn modules_with_text_never_lists_common_as_a_module_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().join("packs"));
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
        let mut registry = Registry::sweep(dir.path().join("packs"));
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("loading", "Loading")])]));
        registry.insert_announced("common", module_layers("common", &[("en", &[("loading", "Loading")]), ("fr", &[("loading", "Chargement")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("fr").and_then(|l| l.get("loading")), Some("Chargement"));
    }

    #[test]
    fn modules_with_text_includes_a_language_that_exists_only_in_a_pack() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "de", "radio", "play = \"Spielen\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("de").and_then(|l| l.get("play")), Some("Spielen"));
    }

    #[test]
    fn modules_with_text_own_layer_wins_over_common_within_one_language() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().join("packs"));
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "own-play")])]));
        registry.insert_announced("common", module_layers("common", &[("en", &[("play", "common-play")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("en").and_then(|l| l.get("play")), Some("own-play"));
    }

    /// [MUTATION] The other half of `merge_with_common`'s documented
    /// priority ("pack beats announced, within one language"), which
    /// `modules_with_text_own_layer_wins_over_common_within_one_language`
    /// above does not touch — that test only ever exercises the
    /// own-vs-common axis, with both sides on the *announced* tier. A
    /// silent reordering of `sources_for`'s four-source list (`merge_with_common`
    /// folds whatever it returns) that swapped `own_announced` and `own_pack`
    /// would pass every other test in this file and still be caught by
    /// nothing without this one.
    #[test]
    fn modules_with_text_own_pack_beats_own_announced_within_one_language() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "en", "radio", "play = \"pack-play\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "announced-play")])]));
        let radio = registry.modules_with_text().into_iter().find(|m| m.name() == "radio").unwrap();
        assert_eq!(radio.layer("en").and_then(|l| l.get("play")), Some("pack-play"));
    }

    #[test]
    fn modules_with_text_is_sorted_by_module_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::sweep(dir.path().join("packs"));
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("k", "v")])]));
        registry.insert_announced("cd", module_layers("cd", &[("en", &[("k", "v")])]));
        registry.insert_announced("core", module_layers("core", &[("en", &[("k", "v")])]));
        let names: Vec<String> = registry.modules_with_text().into_iter().map(|m| m.name().to_string()).collect();
        assert_eq!(names, vec!["cd".to_string(), "core".to_string(), "radio".to_string()]);
    }

    // --- core_languages: the fallback candidate list (task 12's F-1/F-2) ---

    #[test]
    fn core_languages_always_includes_en_even_with_nothing_installed() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::sweep(dir.path().join("packs"));
        assert_eq!(registry.core_languages(), vec!["en".to_string()]);
    }

    #[test]
    fn core_languages_picks_up_an_installed_pack_sorted_after_en() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "nl", "core", "play = \"Spelen\"\n");
        write_pack(packs.path(), "fr", "core", "play = \"Lecture\"\n");
        let registry = Registry::sweep(packs.path().to_path_buf());
        assert_eq!(registry.core_languages(), vec!["en".to_string(), "fr".to_string(), "nl".to_string()]);
    }

    #[test]
    fn core_languages_ignores_a_plugin_only_language() {
        // The narrower half of the union/fallback split: a language only
        // "radio" translates must not leak into the fallback candidates,
        // which the owner reserves to what the core itself ships.
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "de", "radio", "play = \"Spielen\"\n");
        let registry = Registry::sweep(packs.path().to_path_buf());
        assert_eq!(registry.core_languages(), vec!["en".to_string()]);
    }

    #[test]
    fn core_languages_reads_the_swept_snapshot_not_a_live_directory() {
        // Discriminating proof for the reason this method exists at all:
        // a pack written to disk *after* the sweep must stay invisible
        // until a resweep, exactly like `chain_for`'s own no-I/O guarantee
        // — a fallback candidate list built from a live read could
        // otherwise offer a language `chain_for` cannot resolve yet.
        let packs = tempfile::tempdir().unwrap();
        let registry = Registry::sweep(packs.path().to_path_buf());
        write_pack(packs.path(), "de", "core", "play = \"Spielen\"\n");
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
        let mut registry = Registry::sweep(dir.path().join("packs"));
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]));
        assert!(registry.union_languages().contains(&"de".to_string()));
    }

    // `union_languages_excludes_a_module_never_announced` (the disk-tier
    // version of this property — a module with only a hand-written file
    // must not leak its language) was deleted with the operator layer:
    // `a_pack_for_an_absent_module_creates_no_language_and_no_hole`, above,
    // already proves the surviving equivalent — a pack for a module nobody
    // announced must not leak its language into `union_languages` either.

    #[test]
    fn union_languages_matches_the_expensive_computation_it_replaces() {
        // The equivalence this accessor exists to preserve, checked
        // directly rather than only through the HTTP route: own tiers,
        // pack tiers, and `common`'s own two tiers, all contributing
        // distinct languages, core-only and plugin-only alike.
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "nl", "radio", "play = \"Spelen\"\n");
        write_pack(packs.path(), "it", "common", "ok = \"Ok\"\n");
        let mut registry = Registry::sweep(packs.path().to_path_buf());
        registry.insert_announced("core", module_layers("core", &[("en", &[("k", "v")])]));
        registry.insert_announced("radio", module_layers("radio", &[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]));
        registry.insert_announced("common", module_layers("common", &[("en", &[("ok", "Ok")]), ("es", &[("ok", "Vale")])]));

        let cheap = registry.union_languages();
        let expensive = ritornello_i18n::union_of_languages(&registry.modules_with_text());
        assert_eq!(cheap, expensive);
        // Not a vacuous match: every source tier contributed something the
        // other three did not (nl from radio's pack, it from common's
        // pack, es from common's announced tier, de from radio's announced
        // tier), so an implementation that silently dropped one source
        // would diverge from `expensive`, not merely return an empty list
        // either side agrees on.
        for lang in ["en", "de", "nl", "it", "es"] {
            assert!(cheap.contains(&lang.to_string()), "{lang} missing from {cheap:?}");
        }
    }

    // `sweep_discovers_a_module_directory_it_was_never_told_about` (the
    // disk-tier version: a directory walk finds a module nobody asked
    // about) was deleted with the operator layer — `sweep_disk`, what it
    // exercised, is gone. The surviving equivalent, that a *pack's* module
    // is discovered by the walk rather than by being asked about, is
    // already proved by `a_core_language_carried_by_a_pack_is_offered_as_a_
    // fallback`: nothing ever calls `insert_announced("core", …)` there
    // either, yet the pack's language is found.

    #[test]
    fn chain_for_performs_no_disk_i_o_once_swept() {
        // Discriminating proof that `chain_for` reads nothing: sweep while
        // the pack exists, delete it, then resolve the same key. If
        // `chain_for` still touched the filesystem, this would now fall
        // through to the key itself instead of the swept value.
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "nl", "radio", "play = \"Spelen\"\n");
        let registry = Registry::sweep(packs.path().to_path_buf());
        std::fs::remove_dir_all(packs.path().join("ritornello-lang-nl")).unwrap();
        assert_eq!(
            registry.chain_for("radio", "nl", "en").get("play"),
            "Spelen",
            "chain_for must answer from the swept snapshot, not re-read the now-missing pack"
        );
    }

    // `resweep_async_forgets_a_pack_removed_from_disk` (the disk-tier
    // version: wholesale HashMap replacement, file removal vs. directory
    // removal) was deleted with the `disk` field itself. The surviving
    // fact — a resweep replaces rather than merges — is proved for `packs`
    // (a `Vec`, not a `HashMap`, so it is not the same code path) by
    // `resweep_async_forgets_a_language_pack_removed_from_disk`, just below.

    /// `w.packs = packs` and `w.packs.extend(packs)` are **not**
    /// distinguished by a shrinking inventory the way a `HashMap`
    /// assignment vs. merge would be: `packs` is a `Vec`, so an `extend`
    /// after removal still fails closed rather than open in the failure
    /// this test drives — the stale `InstalledPack` stays in the list,
    /// `pack_layers` still finds its layer, and the removed pack's text
    /// keeps resolving exactly as if nothing had happened. Measured:
    /// temporarily changing `resweep_async`'s `w.packs = packs` to
    /// `w.packs.extend(packs)` left every other test in this file green,
    /// this one included the length alone would not have caught it — hence
    /// the assertion below reads a resolved key through `chain_for`, never
    /// `installed_packs().len()`.
    #[tokio::test]
    async fn resweep_async_forgets_a_language_pack_removed_from_disk() {
        let packs = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(packs.path().join("ritornello-lang-fr")).unwrap();
        std::fs::write(
            packs.path().join("ritornello-lang-fr/pack.toml"),
            "language = \"fr\"\nversion = \"0.2.0\"\nsource = \"x\"\nmodules = [\"radio\"]\n",
        )
        .unwrap();
        std::fs::write(packs.path().join("ritornello-lang-fr/radio.toml"), "play = \"Lecture\"\n").unwrap();

        let shared: crate::i18n::Shared =
            std::sync::Arc::new(tokio::sync::RwLock::new(Registry::sweep(packs.path().to_path_buf())));
        assert_eq!(
            shared.read().await.chain_for("radio", "fr", "en").get("play"),
            "Lecture",
            "the installed pack must resolve before the removal"
        );

        std::fs::remove_dir_all(packs.path().join("ritornello-lang-fr")).unwrap();
        Registry::resweep_async(&shared).await;

        assert_eq!(
            shared.read().await.chain_for("radio", "fr", "en").get("play"),
            "play",
            "the removed pack's text must no longer resolve, not merely be absent from a count"
        );
    }

    /// Functional coverage for `resweep_async`: a pack installed after the
    /// first sweep must be picked up, through the async/`Shared` path
    /// `Core::set_locale` actually uses. Correctness of the *result*, not
    /// of the locking discipline — see
    /// `walk_completes_while_a_reader_holds_the_registry`, below, for the
    /// structural proof of that.
    #[tokio::test]
    async fn resweep_async_picks_up_a_pack_written_after_the_first_sweep() {
        let packs = tempfile::tempdir().unwrap();
        let shared: crate::i18n::Shared =
            std::sync::Arc::new(tokio::sync::RwLock::new(Registry::sweep(packs.path().to_path_buf())));
        assert_eq!(
            shared.read().await.chain_for("radio", "nl", "en").get("play"),
            "play",
            "nothing installed yet"
        );

        write_pack(packs.path(), "nl", "radio", "play = \"Spelen\"\n");
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
    /// `packs_root`), so it must complete even while a reader holds the
    /// registry for the whole test — a read guard taken here and never
    /// dropped until the function returns. No sleep, no poll loop: either
    /// `walk` asks for the write lock at some point, in which case this
    /// deadlocks (turned into a clean failure by the `timeout` below, a
    /// safety net against a hung test run, not a timing assertion), or it
    /// does not, in which case it returns regardless of how long the held
    /// guard lives.
    ///
    /// This is what discriminates against the regression task 4's review
    /// named and task 5 made reachable: moving the walk back under the
    /// write lock (fold `walk` and `resweep_async` back into the one
    /// single write-locked call `Core::set_locale` used to
    /// make) is exactly what would make this test hang instead of return.
    #[tokio::test]
    async fn walk_completes_while_a_reader_holds_the_registry() {
        let packs = tempfile::tempdir().unwrap();
        write_pack(packs.path(), "nl", "radio", "play = \"Spelen\"\n");
        let shared: crate::i18n::Shared =
            std::sync::Arc::new(tokio::sync::RwLock::new(Registry::sweep(packs.path().to_path_buf())));

        // Held for the rest of the test: a real writer (the swap half of
        // `resweep_async`) could never be granted the lock while this is
        // alive. The walk must not care.
        let _read_guard = shared.read().await;

        let installed = tokio::time::timeout(std::time::Duration::from_secs(5), Registry::walk(&shared))
            .await
            .expect("the walk must never need to wait on a guard the test itself holds")
            .expect("the blocking task must not panic on a readable root");

        assert_eq!(
            installed
                .iter()
                .find(|p| p.manifest.language == "nl")
                .and_then(|p| p.layers.iter().find(|(m, _)| *m == "radio"))
                .and_then(|(_, l)| l.get("play")),
            Some("Spelen"),
            "the walk must still have read the real pack, not a stand-in"
        );
    }
}
