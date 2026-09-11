//! Embedded English catalog of the core, and the registry that stacks every
//! module's translation layers into one resolution chain (`own → common →
//! key`, see `registry`).

mod registry;
pub use registry::Registry;

/// Embedded English of the core (always-present base).
pub const EN: &str = include_str!("../locales/en.toml");

/// Builds the core's own resolved catalog for `locale`, with `fallback` as
/// its secondary language before English.
///
/// The single construction shared by the core's startup (`main.rs`) and
/// every later locale change (`Core::set_locale`), so the two never drift
/// onto two different chain orders — a fresh `Registry` is cheap enough to
/// build on every call (the disk root is read fresh either way, exactly
/// like `Catalog::load` before it), and seeding it here, uniformly, is what
/// lets the core's own embedded English and `common`'s be found through the
/// very same `announced` path a plugin's catalogue travels, rather than
/// `Registry` special-casing the core internally.
pub fn core_catalog(locale: &str, fallback: &str, root: &std::path::Path) -> ritornello_i18n::Catalog {
    let mut registry = Registry::new(root.to_path_buf());
    registry.insert_announced("core", core_module_layers());
    registry.insert_announced("common", common_module_layers());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_catalog_resolves_the_core_s_own_embedded_english() {
        let dir = tempfile::tempdir().unwrap();
        let cat = core_catalog("en", "en", dir.path());
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
        let cat = core_catalog("fr", "en", dir.path());
        assert_eq!(cat.get("standby"), "VEILLE");
    }
}
