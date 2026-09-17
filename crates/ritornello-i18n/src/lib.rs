//! Shared i18n catalog for ritornello.
//!
//! The model is two types, one built on the other:
//! - `Layer` (see `layer`): one language's contribution to one pack, holding
//!   **only** the keys that language defines — never English filled into
//!   its holes. That is what makes stacking possible: a layer that carried
//!   a floor of its own would shadow whatever pack sits below it.
//! - `Chain` (see `chain`): an ordered stack of layers. The first layer to
//!   define a key wins; the stacking is what produces the floor, never a
//!   layer by itself. `Chain::load_for_tests` builds one, single-language,
//!   four layers — a **test fixture helper**, not the resolution production
//!   uses: `ritornello_core::i18n::Registry::chain_for` is, stacking *three*
//!   languages (chosen, a device fallback, then English) through `Chain::
//!   new`, up to twelve layers, and consulting an `announced` tier (a
//!   plugin's own confided catalog) `Chain::load_for_tests` has no concept
//!   of at all. See that constructor's own doc for exactly what it omits.
//!
//! `ModuleLayers` (see `layer`) groups one module's layers by language; it
//! is data, not resolution, kept alongside `Layer` for the callers that
//! need to reason about a module's coverage across languages.
//!
//! `coverage` (see `coverage`) is the arithmetic that reasoning runs on: the
//! union of languages at least one module translates, and — for one
//! candidate language — which modules are complete, partial or absent.
//! Pure, key-set arithmetic over `ModuleLayers` already in memory; see its
//! own module doc for the choices behind "complete".
//!
//! Resolution by key: `own` (component) → `common` → the key itself (safety
//! net). Interpolation: the caller does `catalog.get(key)` then
//! `interpolate::interpolate` to fill its `{name}` tokens (no template
//! engine) — a single left-to-right pass, not the chained `str::replace`
//! folds that used to live at each call site (see `interpolate`'s module
//! doc for why that mattered).

mod chain;
mod coverage;
mod interpolate;
mod layer;

pub use chain::{common_embedded, Chain};
pub use coverage::{coverage, union_of_languages, Coverage, ModuleCoverage};
pub use interpolate::{interpolate, params_in};
pub use layer::{shipped_language_packs, try_parse, Layer, ModuleLayers};

// Only the crate's own tests (unmodified below) reach for the embedded
// common pack directly; outside `cfg(test)` nothing needs it by name.
#[cfg(test)]
pub(crate) use chain::COMMON_EN;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes `<root>/<subdir>/<file>` and returns the root TempDir.
    fn write(dir: &std::path::Path, subdir: &str, file: &str, content: &str) {
        let d = dir.join(subdir);
        std::fs::create_dir_all(&d).unwrap();
        let mut f = std::fs::File::create(d.join(file)).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn own_takes_priority_over_common() {
        let dir = tempfile::tempdir().unwrap();
        // own_en defines "error", common has it too: own must win.
        let cat = Chain::load_for_tests("core", "en", dir.path(), "error = \"own-error\"\n");
        assert_eq!(cat.get("error"), "own-error");
    }

    #[test]
    fn an_external_pack_overrides_the_embedded_own() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "core", "fr.toml", "standby = \"VEILLE\"\n");
        let cat = Chain::load_for_tests("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.get("standby"), "VEILLE");
    }

    #[test]
    fn an_external_pack_overrides_the_embedded_common() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "common", "fr.toml", "error = \"Erreur\"\n");
        let cat = Chain::load_for_tests("core", "fr", dir.path(), "");
        assert_eq!(cat.get("error"), "Erreur");
    }

    #[test]
    fn a_missing_key_falls_back_to_english_then_to_the_key_itself() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Chain::load_for_tests("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        // no fr pack: the embedded English is kept
        assert_eq!(cat.get("standby"), "STANDBY");
        // unknown key: the key itself is returned
        assert_eq!(cat.get("unknown"), "unknown");
    }

    #[test]
    fn invalid_toml_is_ignored_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "core", "fr.toml", "this = is not valid");
        let cat = Chain::load_for_tests("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.get("standby"), "STANDBY"); // fallback to English, no panic
    }

    #[test]
    fn try_parse_of_the_embedded_common_en_is_non_empty() {
        assert!(!try_parse(COMMON_EN).unwrap().is_empty());
    }

    #[test]
    fn try_parse_returns_err_on_invalid_toml() {
        assert!(try_parse("this is not toml =").is_err());
    }

    #[test]
    fn entries_merges_own_over_common() {
        let dir = tempfile::tempdir().unwrap();
        // `error` exists in the embedded common: `own` must take priority, as
        // in `get`.
        let cat = Chain::load_for_tests("core", "en", dir.path(), "error = \"own-error\"\nother = \"x\"\n");
        let e = cat.entries();
        assert_eq!(e.get("error").copied(), Some("own-error"));
        assert_eq!(e.get("other").copied(), Some("x"));
        // The common keys not redefined are present: the map is
        // complete, and it's what feeds `t()` on the browser side.
        assert!(e.len() > 1);
        assert!(e.keys().any(|k| *k == "play"), "the common vocabulary must be included");
    }

    /// **Generalized (task 15).** The pre-task-15 shape of this test named
    /// `fr` and compared key sets alone; both were hazards this project has
    /// already paid for once each (see `docs/plugins.md`'s language-packs
    /// chantier notes): a hardcoded language stops covering a second one
    /// the moment it ships **while still passing**, and a key-set-only
    /// comparison cannot see a translation that renamed or dropped a
    /// `{named}` parameter the English value still carries.
    ///
    /// `shipped_language_packs` derives the language list from
    /// `deploy/locales/common/` itself, so a language added there is
    /// covered automatically; the `assert!(!shipped.is_empty(), ...)` below
    /// is what keeps that derivation honest — an empty discovery must fail
    /// loudly, not be indistinguishable from "every language passed".
    #[test]
    fn key_and_param_parity_between_the_embedded_common_and_every_shipped_language() {
        let en = try_parse(COMMON_EN).unwrap();
        let deploy_locales =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = shipped_language_packs(&deploy_locales, "common");
        assert!(!shipped.is_empty(), "no shipped language found for common under deploy/locales");
        for (lang, content) in shipped {
            let pack = try_parse(&content)
                .unwrap_or_else(|e| panic!("{lang} pack for common is invalid TOML: {e}"));
            let mut en_keys: Vec<&String> = en.keys().collect();
            let mut pack_keys: Vec<&String> = pack.keys().collect();
            en_keys.sort();
            pack_keys.sort();
            assert_eq!(en_keys, pack_keys, "common en/{lang} key sets diverge");

            for (key, en_value) in &en {
                if let Some(translated) = pack.get(key) {
                    assert_eq!(
                        params_in(en_value),
                        params_in(translated),
                        "common key {key}: {lang} translation's named parameters diverge from English"
                    );
                }
            }
        }
    }

    /// Sanity for the generalization above: pins that this repository does
    /// ship at least the second language the whole chantier was built
    /// around, so `key_and_param_parity_between_the_embedded_common_and_every_shipped_language`
    /// is not vacuously satisfied by a discovery that happens to find zero
    /// languages in a differently-shaped tree.
    #[test]
    fn common_fr_pack_is_among_the_discovered_shipped_languages() {
        let deploy_locales =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = shipped_language_packs(&deploy_locales, "common");
        assert!(shipped.iter().any(|(lang, _)| lang == "fr"), "fr must be among the shipped languages");
    }

    #[test]
    fn the_plugin_ui_loading_keys_live_in_the_common_layer() {
        // These three keys are shown by the SPA shell
        // (`web/app/src/views/PluginView.ts`). They must live in
        // `common` — inherited by ALL catalogs — and not in the core's own:
        // the shell resolves them first in the **plugin's** catalog,
        // which is empty precisely when the plugin is unreachable, the very case
        // that produces `plugin_unavailable`.
        let dir = tempfile::tempdir().unwrap();
        // Catalog of a plugin whose `own` defines nothing: the keys
        // must still resolve, and never return the key itself.
        //
        // `plugin_unavailable_cause` joined the list: it's the variant that
        // names the cause of the refusal, and it's shown in exactly the same
        // case — an unreachable plugin, hence an empty plugin catalog.
        let cat = Chain::load_for_tests("radio", "en", dir.path(), "");
        for key in [
            "loading",
            "plugin_unavailable",
            "plugin_unavailable_cause",
            "plugin_contract_mismatch",
        ] {
            assert_ne!(cat.get(key), key, "key {key} absent from the common vocabulary");
            // `entries()` is what goes to the browser: the key must be
            // there, otherwise the SPA's `t()` falls back to the raw key.
            assert!(cat.entries().contains_key(key), "key {key} absent from entries()");
        }
    }

    #[test]
    fn entries_reflects_external_overrides() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "core", "fr.toml", "standby = \"VEILLE\"\n");
        let cat = Chain::load_for_tests("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.entries().get("standby").copied(), Some("VEILLE"));
    }

}
