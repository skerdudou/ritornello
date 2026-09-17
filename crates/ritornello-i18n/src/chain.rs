//! Stacking layers into one answer.
//!
//! A `Chain` is an ordered list of `Layer`s: the first layer that defines a
//! key wins, and an unknown key resolves to itself. `Chain::load_for_tests`
//! builds a single-language, four-layer `Chain` — a test fixture helper, not
//! the resolution production uses (`ritornello_core::i18n::Registry::
//! chain_for` is); see that constructor's own doc for the difference.

use std::collections::HashMap;
use std::path::Path;

use crate::layer::Layer;

/// Common English vocabulary embedded in the crate — the last layer of
/// every `Chain::load_for_tests`, the floor beneath even a component's own
/// embedded English.
pub(crate) const COMMON_EN: &str = include_str!("locales/common_en.toml");

/// The `common` module's embedded English, parsed once.
///
/// The exact content `Chain::load_for_tests` already uses as its own fourth layer,
/// exposed here because `ritornello_core::i18n::Registry` (task 4) needs to
/// place it explicitly within its own language-segregated stack — the
/// registry treats `common` like any other module, and its embedded layer
/// has to come from somewhere other than duplicating `COMMON_EN`'s content
/// or reaching into this crate's private constant.
pub fn common_embedded() -> Layer {
    match Layer::parse(COMMON_EN) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("embedded common pack invalid: {e}");
            Layer::default()
        }
    }
}

/// An ordered stack of layers. The first layer to define a key wins.
#[derive(Debug, Clone, Default)]
pub struct Chain(Vec<Layer>);

impl Chain {
    pub fn new(layers: Vec<Layer>) -> Chain {
        Chain(layers)
    }

    /// **Test-only fixture helper — not the resolution production uses.**
    /// `ritornello_core::i18n::Registry::chain_for` is: it stacks *three*
    /// languages (the chosen one, a device fallback, then English), each
    /// contributing up to four layers (disk pack and announced plugin text,
    /// for both the component and `common`) — twelve layers, at most, in
    /// strict language-then-tier order. This builds a `Chain` for **one**
    /// language only, four layers, in priority order:
    /// 1. disk pack for the component, at `<root>/<component>/<locale>.toml`
    /// 2. the component's embedded English (`own_en`)
    /// 3. disk pack for `common`, at `<root>/common/<locale>.toml`
    /// 4. `common`'s embedded English (this crate's `locales/common_en.toml`)
    ///
    /// It has **no fallback-locale tier at all**, and no `announced` tier
    /// (a plugin's own confided catalog, `Registry`'s exclusive concern) —
    /// a fixture built with this constructor cannot exercise, and will
    /// silently disagree with production about, a key that only a device's
    /// fallback language or an announced layer defines. It exists because a
    /// great many core-side tests need *a* plausible catalog to construct a
    /// `Wiring`/`AppState` without caring about resolution subtleties; reach
    /// for `Registry::chain_for` instead whenever a test's own point *is*
    /// resolution order, precedence, or a fallback language.
    ///
    /// Never panics: an absent or invalid disk pack simply leaves that layer
    /// out, and an invalid embedded pack becomes an empty layer — either way,
    /// resolution falls through to the next layer.
    pub fn load_for_tests(component: &str, locale: &str, root: &Path, own_en: &str) -> Chain {
        let embedded_own = match Layer::parse(own_en) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("embedded pack {component} invalid: {e}");
                Layer::default()
            }
        };
        let embedded_common = match Layer::parse(COMMON_EN) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("embedded common pack invalid: {e}");
                Layer::default()
            }
        };
        let disk_own = Layer::from_disk(&root.join(component).join(format!("{locale}.toml")));
        let disk_common = Layer::from_disk(&root.join("common").join(format!("{locale}.toml")));

        let mut layers = Vec::with_capacity(4);
        layers.extend(disk_own);
        layers.push(embedded_own);
        layers.extend(disk_common);
        layers.push(embedded_common);

        Chain(layers)
    }

    /// Resolves a key: the first layer that defines it wins. An unknown key
    /// resolves to itself (safety net — a visibly wrong text is easier to
    /// diagnose than a silently missing one).
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.0.iter().find_map(|layer| layer.get(key)).unwrap_or(key)
    }

    /// Flat map of every key known to any layer, with the same priority as
    /// `get`: built by walking the layers from **last to first**, so that
    /// each earlier layer's inserts overwrite the later layers' and the
    /// first layer is the one left standing.
    ///
    /// Used to ship the catalog to the browser (`GET /api/i18n`): the SPA
    /// resolves its keys client-side, which replaces the `{{key}}`
    /// substitution of old. The values remain **data** end to end: no
    /// character is dangerous, unlike raw substitution into JS source.
    pub fn entries(&self) -> HashMap<&str, &str> {
        let mut out = HashMap::new();
        for layer in self.0.iter().rev() {
            for (k, v) in layer.as_map() {
                out.insert(k.as_str(), v.as_str());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trap this whole design exists to avoid, and the one a naive
    /// implementation falls into: a flattened catalog already carries English in
    /// its holes, indistinguishable from a real translation. Stacking such a
    /// catalog over another would shadow the fallback with English and the screen
    /// would look like it works.
    ///
    /// A `Layer` therefore holds only what its language defines. Here `nl` defines
    /// one key out of two, `fr` defines both: the missing one must resolve to
    /// **French**, never to English.
    #[test]
    fn a_partial_layer_lets_the_next_language_answer() {
        let nl = Layer::parse("play = \"Spelen\"\n").unwrap();
        let fr = Layer::parse("play = \"Lecture\"\nstop = \"Arrêt\"\n").unwrap();
        let en = Layer::parse("play = \"Play\"\nstop = \"Stop\"\n").unwrap();
        let chain = Chain::new(vec![nl, fr, en]);
        assert_eq!(chain.get("play"), "Spelen");
        assert_eq!(chain.get("stop"), "Arrêt", "the fallback must answer, not English");
    }

    /// The safety net, unchanged from the previous design: an unknown key is
    /// returned as itself rather than empty. A visibly wrong text is easier to
    /// diagnose than a silently missing one.
    #[test]
    fn an_unknown_key_resolves_to_itself() {
        let chain = Chain::new(vec![Layer::parse("play = \"Play\"\n").unwrap()]);
        assert_eq!(chain.get("nope"), "nope");
    }

    /// `entries` must agree with `get` key for key: it is what feeds the browser's
    /// own resolver, and a divergence between the two would be invisible in Rust
    /// and visible only on screen.
    #[test]
    fn entries_agrees_with_get_on_every_key() {
        let chain = Chain::new(vec![
            Layer::parse("play = \"Spelen\"\n").unwrap(),
            Layer::parse("play = \"Lecture\"\nstop = \"Arrêt\"\n").unwrap(),
        ]);
        let e = chain.entries();
        for k in ["play", "stop"] {
            assert_eq!(e.get(k).copied(), Some(chain.get(k)), "divergence on {k}");
        }
    }

    // --- The couture task 15 exists to prove: the core and the browser are
    // two consumers of one stacked chain, not two chains. `entries_agrees_
    // with_get_on_every_key` (above) already proves the flattening step
    // agrees before interpolation; the test below carries the resolved
    // value through `interpolate` on the Rust side, against a **shared
    // fixture** — `tests/fixtures/resolver_parity.json` — that
    // `web/kit/src/i18n.test.ts` reads and runs through `createT`+
    // `interpolate` too.
    //
    // A first version of this test mirrored the fixture's cases by hand,
    // one literal per side, cross-referenced only in a comment. A review
    // round measured what that was actually worth: it changed this crate's
    // `interpolate` to render an unsupplied `{token}` empty instead of
    // leaving it visible, updated **only** the Rust-side expectation, and
    // got both suites green while the two resolvers rendered the same
    // input differently. A comment naming the other file is documentation;
    // it enforces nothing a compiler or a test runner checks. A **shared
    // fixture** does: one side's behaviour changing without the other's
    // forces an edit to the one file both read, and that edit is what a
    // reviewer — or a second implementer — actually sees.
    //
    // The eight cases cover the two shapes this chantier previously paid
    // for in only one language each: a parameter value carrying `{braces}`
    // of its own (`Radio {url}`, the interpolation defect this crate's own
    // `interpolate` was rewritten to survive) and a key or parameter named
    // after an `Object.prototype` member (`toString`, the defect a review
    // round found live in `createT`'s bracket lookup) — plus the unknown-key
    // and unmatched-brace safety nets, jointly asserted for the first time.
    #[test]
    fn resolver_parity_fixture_agrees_between_get_plus_interpolate_and_the_browser() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/resolver_parity.json"
        ))
        .expect("tests/fixtures/resolver_parity.json is valid JSON");
        let cases = fixture.as_array().expect("the fixture is a JSON array");
        // A fixture that failed to load, or was emptied by accident, must
        // not read as "every case passed" — the exact hazard a hardcoded
        // subject list has already produced twice on this branch
        // (`shipped_language_packs`'s own doc). The threshold is the
        // fixture's own current case count, not a loose lower bound: a
        // looser number would let a case be deleted — silently narrowing
        // what this test actually covers — without either side's minimum-
        // count guard ever noticing (task 15 re-review, N6a).
        assert!(cases.len() >= 8, "fixture has fewer cases than expected: {cases:?}");

        for case in cases {
            let name = case["name"].as_str().expect("case.name is a string");
            let catalog: HashMap<String, String> = case["catalog"]
                .as_object()
                .expect("case.catalog is an object")
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().expect("catalog values are strings").to_string()))
                .collect();
            let key = case["key"].as_str().expect("case.key is a string");
            let params: Vec<(String, String)> = case["params"]
                .as_object()
                .expect("case.params is an object")
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().expect("param values are strings").to_string()))
                .collect();
            let expected = case["expected"].as_str().expect("case.expected is a string");

            let chain = Chain::new(vec![Layer::from_map(catalog)]);
            let resolved = chain.get(key);
            let out = crate::interpolate(resolved, params.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            assert_eq!(out, expected, "fixture case {name:?} diverged on the Rust side");
        }
    }
}
