//! Stacking layers into one answer.
//!
//! A `Chain` is an ordered list of `Layer`s: the first layer that defines a
//! key wins, and an unknown key resolves to itself. `Catalog` is the
//! resolution actually used at runtime — a `Chain` of exactly four layers,
//! see `Catalog::load`.

use std::collections::HashMap;
use std::path::Path;

use crate::layer::Layer;

/// Common English vocabulary embedded in the crate — the last layer of
/// every `Catalog`, the floor beneath even a component's own embedded
/// English.
pub(crate) const COMMON_EN: &str = include_str!("locales/common_en.toml");

/// An ordered stack of layers. The first layer to define a key wins.
#[derive(Debug, Clone, Default)]
pub struct Chain(Vec<Layer>);

impl Chain {
    pub fn new(layers: Vec<Layer>) -> Chain {
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

/// The resolution actually used at runtime: `own` (the component) then
/// `common`, each itself a disk pack over the embedded English — four
/// layers, in priority order:
/// 1. disk pack for the component, at `<root>/<component>/<locale>.toml`
/// 2. the component's embedded English (`own_en`)
/// 3. disk pack for `common`, at `<root>/common/<locale>.toml`
/// 4. `common`'s embedded English (this crate's `locales/common_en.toml`)
pub struct Catalog {
    chain: Chain,
}

impl Catalog {
    /// Builds the catalog of a component for a given language. Never
    /// panics: an absent or invalid disk pack simply leaves that layer out,
    /// and an invalid embedded pack becomes an empty layer — either way,
    /// resolution falls through to the next layer.
    pub fn load(component: &str, locale: &str, root: &Path, own_en: &str) -> Catalog {
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

        Catalog { chain: Chain::new(layers) }
    }

    /// Resolves a key: `own` → `common` → the key itself.
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.chain.get(key)
    }

    /// Flat map of **all** known keys, `own` overriding `common` — the same
    /// priority order as `get`, but exposed as one block.
    pub fn entries(&self) -> HashMap<&str, &str> {
        self.chain.entries()
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
}
