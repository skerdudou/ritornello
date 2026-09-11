//! Embedded English catalog of the core, and the registry that stacks every
//! module's translation layers into one resolution chain (`own → common →
//! key`, see `registry`).

mod registry;
pub use registry::Registry;

use std::collections::HashMap;
use std::sync::Arc;

/// Embedded English of the core (always-present base).
pub const EN: &str = include_str!("../locales/en.toml");

/// One `Registry`, shared between the HTTP `AppState` and the core's own
/// `select!` loop — the same `Arc` in both places, exactly like
/// `AppState::admin_backends`. There is one registry per process: the core's
/// own catalog and every plugin's are resolved out of the very same tiers,
/// which is the whole point of holding them centrally instead of letting
/// each reader keep its own copy.
pub type Shared = Arc<tokio::sync::RwLock<Registry>>;

/// Sweeps `root` once and seeds the core's own module and `common`'s — the
/// construction used exactly once, at startup (`main.rs`). Plugin modules
/// are added afterwards, one `insert_announced` per announcement, as they
/// arrive.
pub fn seeded_registry(root: std::path::PathBuf) -> Registry {
    let mut registry = Registry::sweep(root);
    seed_core_and_common(&mut registry);
    registry
}

/// Seeds the registry's `core` and `common` modules with their embedded
/// English, treating the core exactly like a plugin that "announced" its
/// own catalogue — the uniform path `Registry::chain_for` relies on. Kept
/// separate from `seeded_registry` so a caller that already holds a
/// `Registry` (none does yet, but a future one might) can reseed without
/// resweeping the disk.
fn seed_core_and_common(registry: &mut Registry) {
    registry.insert_announced("core", core_module_layers());
    registry.insert_announced("common", common_module_layers());
}

/// Builds the core's own resolved catalog for `locale`, with `fallback` as
/// its secondary language before English.
///
/// The single construction shared by the core's startup (`main.rs`) and
/// every later locale change (`Core::set_locale`), so the two never drift
/// onto two different chain orders. Takes the already-built, already-seeded
/// `Registry` — it performs no I/O of its own (see `Registry::chain_for`'s
/// doc); the sweep happens once at startup and again wherever the caller
/// already rebuilds on a real locale change.
pub fn core_catalog(registry: &Registry, locale: &str, fallback: &str) -> ritornello_i18n::Catalog {
    ritornello_i18n::Catalog::from_chain(registry.chain_for("core", locale, fallback))
}

/// The core's own embedded English, folded into the shape `Registry`
/// expects from any module: one language ("en") mapping to one `Layer`.
fn core_module_layers() -> ritornello_i18n::ModuleLayers {
    let mut m = ritornello_i18n::ModuleLayers::new("core");
    match ritornello_i18n::Layer::parse(EN) {
        Ok(l) => m.insert("en", l),
        Err(e) => tracing::warn!("embedded core pack invalid: {e}"),
    }
    m
}

/// `common`'s embedded English, folded the same way — reusing
/// `ritornello_i18n::common_embedded` rather than re-parsing a copy of its
/// source text here.
fn common_module_layers() -> ritornello_i18n::ModuleLayers {
    let mut m = ritornello_i18n::ModuleLayers::new("common");
    m.insert("en", ritornello_i18n::common_embedded());
    m
}

/// Turns one plugin's announced catalogue (`Announcement.catalog`, once it
/// is known to be `Some`) into the `ModuleLayers` `Registry::insert_announced`
/// expects: language → key → value becomes language → `Layer`, via
/// `Layer::from_map`.
///
/// Takes the map directly rather than the whole `Announcement`, so the
/// caller decides what to do with `None` — see its doc for why `None` and
/// `Some({})` must not be conflated: `None` must never reach this function
/// at all, it means leaving the module absent from the registry, not
/// calling this with an empty map (`Some({})` is the case that legitimately
/// does reach here, and produces a `ModuleLayers` with zero languages,
/// which is a different, and correct, fact from the module being absent).
pub fn module_layers_from_catalog(name: &str, catalog: &HashMap<String, HashMap<String, String>>) -> ritornello_i18n::ModuleLayers {
    let mut m = ritornello_i18n::ModuleLayers::new(name);
    for (lang, kv) in catalog {
        m.insert(lang.clone(), ritornello_i18n::Layer::from_map(kv.clone()));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_catalog_resolves_the_core_s_own_embedded_english() {
        let dir = tempfile::tempdir().unwrap();
        let registry = seeded_registry(dir.path().to_path_buf());
        let cat = core_catalog(&registry, "en", "en");
        // The embedded pack is non-empty (core/settings.rs's own test pins
        // this fact for `EN` directly); this checks the same fact survives
        // the trip through `Registry`.
        assert_ne!(cat.get("audio_output"), "audio_output");
    }

    #[test]
    fn core_catalog_prefers_a_disk_pack_over_the_embedded_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let registry = seeded_registry(dir.path().to_path_buf());
        let cat = core_catalog(&registry, "fr", "en");
        assert_eq!(cat.get("standby"), "VEILLE");
    }

    #[test]
    fn module_layers_from_catalog_turns_every_language_into_a_layer() {
        let mut catalog = HashMap::new();
        catalog.insert("en".to_string(), HashMap::from([("play".to_string(), "Play".to_string())]));
        catalog.insert("fr".to_string(), HashMap::from([("play".to_string(), "Lecture".to_string())]));
        let layers = module_layers_from_catalog("radio", &catalog);
        assert_eq!(layers.layer("en").and_then(|l| l.get("play")), Some("Play"));
        assert_eq!(layers.layer("fr").and_then(|l| l.get("play")), Some("Lecture"));
    }

    #[test]
    fn module_layers_from_catalog_of_an_empty_map_is_a_module_with_no_languages() {
        // The `Some({})` half of the distinction the caller must preserve:
        // this function is never handed `None` at all (see its doc), but an
        // announced, genuinely empty catalogue must still produce a present
        // (if empty) `ModuleLayers`, not be indistinguishable from a module
        // that was never inserted.
        let layers = module_layers_from_catalog("console", &HashMap::new());
        assert_eq!(layers.languages().count(), 0);
    }
}
