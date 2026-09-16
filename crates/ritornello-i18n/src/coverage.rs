//! The union of every language at least one module translates, and a
//! measured completeness for one candidate language — key-set arithmetic
//! over `ModuleLayers` already assembled in memory, never a declaration a
//! plugin makes about itself.
//!
//! Both functions here are pure and know nothing about disk vs. announced,
//! about `common`, or about I/O: the caller (`ritornello_core::i18n::
//! Registry::modules_with_text`) has already folded a module's own tiers
//! together with `common`'s vocabulary into one `ModuleLayers` per module —
//! see that method's own doc for what "folded" means and why `common`
//! never appears here as a module of its own.
//!
//! Four choices this module makes, spelled out because the task that added
//! it was asked to decide them rather than guess:
//!
//! 1. **The floor is each module's own English layer**, not the union of
//!    every language any module defines. English is the one layer every
//!    module is guaranteed to carry — the embedded pack shipped inside the
//!    binary, never installed — so it is the only layer that can serve as
//!    "everything this module could possibly say" without itself being a
//!    moving target.
//! 2. **A module with an empty English layer never enters the count.**
//!    Without this guard, an empty layer is *vacuously* a subset of
//!    anything, so a module with nothing to translate would read as
//!    trivially `Complete` in every language — inflating every ratio by a
//!    module that never had a floor to begin with. See
//!    `a_module_with_no_english_layer_at_all_is_left_out_of_the_count`.
//! 3. **`common` and a module's own vocabulary count once, not twice.**
//!    The `ModuleLayers` this module receives already has them folded into
//!    one layer per language (the caller's job); a key defined by both
//!    contributes exactly one entry to the key set either way, because a
//!    set has no notion of "twice".
//! 4. **A key with an empty string value is still a defined key.**
//!    `Layer::get` and `Chain::get` already treat an empty translation as a
//!    resolved value, never as a miss; key-set arithmetic that judged
//!    emptiness differently would disagree with the very resolution it is
//!    measuring.

use std::collections::HashSet;

use crate::ModuleLayers;

/// The union of every language at least one module defines something real
/// in. A language counts only if some module's layer for it is **not
/// empty** (`Layer::is_empty`): a module that carries a language key with
/// nothing behind it (`"de": {}`, a degenerate shape a well-formed
/// announcement should never produce, but this function does not trust
/// that) must not conjure that language into the list.
///
/// English is not special-cased: it shows up because a module that has any
/// text at all always carries a non-empty English layer (the SDK refuses a
/// catalog with none — see `ritornello_plugin_sdk::runtime`'s own test),
/// never because this function injects it. A caller whose input has no
/// English anywhere gets a union without English, which would point at a
/// bug in the caller, not a case worth papering over here.
///
/// Sorted, so the result is deterministic for callers and tests alike —
/// `HashSet` iteration order is not.
pub fn union_of_languages(modules: &[ModuleLayers]) -> Vec<String> {
    let mut set: HashSet<&str> = HashSet::new();
    for m in modules {
        for lang in m.languages() {
            if m.layer(lang).is_some_and(|l| !l.is_empty()) {
                set.insert(lang);
            }
        }
    }
    let mut out: Vec<String> = set.into_iter().map(str::to_string).collect();
    out.sort();
    out
}

/// One module's outcome for a single candidate language, measured against
/// its own English layer as the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleCoverage {
    /// The candidate language's layer defines every key the module's
    /// English layer does.
    Complete,
    /// The module has *some* layer for the candidate language, but it is
    /// missing at least one key English defines. Kept apart from
    /// `Complete` — three keys out of two hundred is not "translated" —
    /// and from `Absent` — it is not nothing, either.
    Partial,
    /// The module defines no layer at all for the candidate language.
    Absent,
}

/// The completeness of one candidate language, across every module that
/// was counted (see `coverage`'s own doc for which modules that is).
///
/// Deliberately does not hand the caller a bare `(done, total)` pair to
/// compare itself: `is_complete` already carries that comparison, and
/// `complete_count`/`total` exist only for the phrase that needs the
/// numbers spelled out ("core + 3 plugins out of 7" — a whole-sentence,
/// named-parameter catalog key, never a concatenation; see the task's own
/// brief). A caller that only needs to decide whether to show anything at
/// all never has to compute `done == total` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    /// One entry per module counted, `(module name, its status)`, in the
    /// order `coverage` received them — not a promised position (`"core"`
    /// first, say): a caller that wants a particular module looks it up by
    /// name.
    modules: Vec<(String, ModuleCoverage)>,
}

impl Coverage {
    /// True only if **every** module counted is `Complete`. An empty
    /// module list is never complete — there is nothing to be complete,
    /// and a caller that let the denominator go empty has a bug upstream,
    /// not a language to celebrate.
    pub fn is_complete(&self) -> bool {
        !self.modules.is_empty() && self.modules.iter().all(|(_, c)| *c == ModuleCoverage::Complete)
    }

    /// How many of the counted modules are `Complete` — the `{done}` of
    /// the phrase key.
    pub fn complete_count(&self) -> usize {
        self.modules.iter().filter(|(_, c)| *c == ModuleCoverage::Complete).count()
    }

    /// How many modules were counted at all — the `{total}` of the phrase
    /// key.
    pub fn total(&self) -> usize {
        self.modules.len()
    }

    /// Per-module detail, for a caller (task 14's summary line) that needs
    /// to single one module out by name rather than only by count — the
    /// example in the brief singles out `"core"`.
    pub fn modules(&self) -> &[(String, ModuleCoverage)] {
        &self.modules
    }
}

/// Measures `lang` against every module in `modules` that has a non-empty
/// English layer of its own — see the module doc's four numbered choices
/// for why English is the floor, why an empty-English module is left out
/// entirely rather than counted as trivially complete, and why a defined
/// key with an empty value still counts as defined.
pub fn coverage(modules: &[ModuleLayers], lang: &str) -> Coverage {
    let per_module = modules
        .iter()
        .filter_map(|m| {
            let english_keys: HashSet<&str> = m.layer("en").map(|l| l.keys().collect()).unwrap_or_default();
            if english_keys.is_empty() {
                // Choice 2: no floor, no entry — see the module doc.
                return None;
            }
            let status = match m.layer(lang) {
                None => ModuleCoverage::Absent,
                Some(l) => {
                    let lang_keys: HashSet<&str> = l.keys().collect();
                    if english_keys.is_subset(&lang_keys) {
                        ModuleCoverage::Complete
                    } else {
                        ModuleCoverage::Partial
                    }
                }
            };
            Some((m.name().to_string(), status))
        })
        .collect();
    Coverage { modules: per_module }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Layer;

    fn layer(pairs: &[(&str, &str)]) -> Layer {
        let source: String = pairs.iter().map(|(k, v)| format!("{k} = {v:?}\n")).collect();
        Layer::parse(&source).unwrap()
    }

    fn module(name: &str, langs: &[(&str, &[(&str, &str)])]) -> ModuleLayers {
        let mut m = ModuleLayers::new(name);
        for (lang, pairs) in langs {
            m.insert(*lang, layer(pairs));
        }
        m
    }

    // --- union_of_languages ---

    #[test]
    fn en_is_in_the_union_when_a_module_carries_it() {
        let core = module("core", &[("en", &[("play", "Play")])]);
        assert_eq!(union_of_languages(&[core]), vec!["en".to_string()]);
    }

    #[test]
    fn a_language_only_one_plugin_translates_is_in_the_union() {
        // The origin defect the whole chantier was opened to fix: a
        // language nobody but a single third-party plugin ships must not
        // disappear because the core, or most other modules, never heard
        // of it.
        let core = module("core", &[("en", &[("play", "Play")])]);
        let radio = module("radio", &[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]);
        let union = union_of_languages(&[core, radio]);
        assert!(union.contains(&"de".to_string()), "de must be in the union: {union:?}");
    }

    #[test]
    fn a_language_key_present_but_empty_does_not_enter_the_union() {
        let mut core = ModuleLayers::new("core");
        core.insert("en", layer(&[("play", "Play")]));
        core.insert("de", Layer::default()); // announced, but nothing confided
        assert_eq!(union_of_languages(&[core]), vec!["en".to_string()]);
    }

    #[test]
    fn the_union_is_sorted_and_deduplicated_across_modules() {
        let a = module("a", &[("fr", &[("k", "v")])]);
        let b = module("b", &[("fr", &[("k", "v")]), ("de", &[("k", "v")])]);
        assert_eq!(union_of_languages(&[a, b]), vec!["de".to_string(), "fr".to_string()]);
    }

    // --- coverage: denominator ---

    #[test]
    fn a_module_with_no_english_layer_at_all_is_left_out_of_the_count() {
        // The named test from the brief: "un module sans texte n'entre pas
        // au dénominateur". A module that never defines English at all
        // (the shape `ModuleLayers::new("console")` has, with nothing
        // inserted) must not appear in `Coverage::modules()`, must not
        // count toward `total()`, and — crucially — must not count as
        // trivially `Complete` either.
        let textless = ModuleLayers::new("console");
        let core = module("core", &[("en", &[("play", "Play")]), ("fr", &[("play", "Lecture")])]);
        let c = coverage(&[textless, core], "fr");
        assert_eq!(c.total(), 1, "the textless module must not enter the denominator");
        assert_eq!(c.modules().iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["core"]);
    }

    #[test]
    fn a_module_with_an_english_layer_but_zero_keys_is_also_left_out() {
        // The `Some({})` shape `module_layers_from_catalog` produces for a
        // plugin that announced but never called `.texts()`: an English
        // *entry* exists, but it is empty. Still no floor, still excluded.
        let mut m = ModuleLayers::new("console");
        m.insert("en", Layer::default());
        let c = coverage(&[m], "fr");
        assert_eq!(c.total(), 0);
    }

    // --- coverage: partial vs. complete ---

    #[test]
    fn a_module_whose_language_layer_covers_every_english_key_is_complete() {
        let m = module("radio", &[("en", &[("a", "A"), ("b", "B")]), ("fr", &[("a", "Ah"), ("b", "Beh")])]);
        let c = coverage(&[m], "fr");
        assert_eq!(c.modules(), &[("radio".to_string(), ModuleCoverage::Complete)]);
        assert!(c.is_complete());
        assert_eq!(c.complete_count(), 1);
        assert_eq!(c.total(), 1);
    }

    #[test]
    fn a_module_whose_language_layer_misses_a_key_is_partial_not_translated() {
        // Named in the brief: a plugin shipping "three keys out of two
        // hundred" must not read as translated. Here it is one key out of
        // two, but the boundary is the same: missing even one key is
        // Partial, never Complete.
        let m = module("radio", &[("en", &[("a", "A"), ("b", "B")]), ("fr", &[("a", "Ah")])]);
        let c = coverage(&[m], "fr");
        assert_eq!(c.modules(), &[("radio".to_string(), ModuleCoverage::Partial)]);
        assert!(!c.is_complete());
        assert_eq!(c.complete_count(), 0);
        assert_eq!(c.total(), 1);
    }

    #[test]
    fn a_module_with_no_layer_at_all_for_the_language_is_absent() {
        let m = module("radio", &[("en", &[("a", "A")])]);
        let c = coverage(&[m], "de");
        assert_eq!(c.modules(), &[("radio".to_string(), ModuleCoverage::Absent)]);
        assert!(!c.is_complete());
    }

    #[test]
    fn a_key_with_an_empty_value_still_counts_as_defined() {
        // Choice 4: presence in the key set is what matters, not whether
        // the translator wrote something non-empty.
        let m = module("radio", &[("en", &[("a", "A")]), ("fr", &[("a", "")])]);
        let c = coverage(&[m], "fr");
        assert_eq!(c.modules(), &[("radio".to_string(), ModuleCoverage::Complete)]);
    }

    #[test]
    fn english_measured_against_itself_is_always_complete() {
        let m = module("radio", &[("en", &[("a", "A"), ("b", "B")])]);
        let c = coverage(&[m], "en");
        assert_eq!(c.modules(), &[("radio".to_string(), ModuleCoverage::Complete)]);
    }

    #[test]
    fn is_complete_requires_every_module_not_just_one() {
        let complete = module("core", &[("en", &[("a", "A")]), ("fr", &[("a", "Ah")])]);
        let partial = module("radio", &[("en", &[("a", "A"), ("b", "B")]), ("fr", &[("a", "Ah")])]);
        let c = coverage(&[complete, partial], "fr");
        assert!(!c.is_complete());
        assert_eq!(c.complete_count(), 1);
        assert_eq!(c.total(), 2);
    }

    #[test]
    fn is_complete_of_an_empty_denominator_is_false_not_vacuously_true() {
        let c = coverage(&[], "fr");
        assert_eq!(c.total(), 0);
        assert!(!c.is_complete());
    }
}
