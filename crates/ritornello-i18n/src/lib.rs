//! Shared i18n catalog for ritornello.
//!
//! Two independent layers per component:
//! - `own`: the component's embedded English (`en.toml`), overlaid by the
//!   external pack `<root>/<component>/<lang>.toml`.
//! - `common`: English embedded in this crate, overlaid by
//!   `<root>/common/<lang>.toml`.
//!
//! Resolution by key: `own` → `common` → the key itself (safety net).
//! Interpolation: the component does `catalog.get(key)` then
//! `str::replace("{n}", &n.to_string())` (no template engine).

use std::collections::HashMap;
use std::path::Path;

/// Common English vocabulary embedded in the crate.
const COMMON_EN: &str = include_str!("locales/common_en.toml");

/// Pure parse of a flat TOML pack (`key = "value"`). Returns the parse error
/// to the caller that wants to log it (loading of the base layers).
pub fn try_parse(s: &str) -> Result<HashMap<String, String>, toml::de::Error> {
    toml::from_str(s)
}

/// Overlays `base` with the TOML pack read from disk at `path`. File
/// **absent**: silent (the normal case — most components have no
/// pack for most languages). Any other error — permission denied,
/// invalid UTF-8, invalid TOML — leaves `base` unchanged but is **traced**:
/// a pack present that the operator meant to install must not disappear
/// without a log line.
fn overlay_from_disk(base: &mut HashMap<String, String>, path: &Path) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!("i18n pack {} ignored (read failed): {e}", path.display());
            return;
        }
    };
    match toml::from_str::<HashMap<String, String>>(&text) {
        Ok(ext) => base.extend(ext),
        Err(e) => tracing::warn!("i18n pack {} ignored (invalid TOML): {e}", path.display()),
    }
}

pub struct Catalog {
    own: HashMap<String, String>,
    common: HashMap<String, String>,
}

impl Catalog {
    /// Builds the catalog of a component for a given language.
    /// Starts from the embedded English (`own_en` for `own`, `COMMON_EN` for
    /// `common`), then layers on the external packs that are present and valid.
    /// Never panics: an absent or invalid pack leaves the English in place.
    pub fn load(component: &str, locale: &str, root: &Path, own_en: &str) -> Catalog {
        let mut own = match try_parse(own_en) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("embedded pack {component} invalid: {e}");
                HashMap::new()
            }
        };
        let mut common = match try_parse(COMMON_EN) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("embedded common pack invalid: {e}");
                HashMap::new()
            }
        };
        overlay_from_disk(&mut common, &root.join("common").join(format!("{locale}.toml")));
        overlay_from_disk(&mut own, &root.join(component).join(format!("{locale}.toml")));
        Catalog { own, common }
    }

    /// Resolves a key: `own` → `common` → the key itself.
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.own
            .get(key)
            .or_else(|| self.common.get(key))
            .map(String::as_str)
            .unwrap_or(key)
    }

    /// Flat map of **all** known keys, `own` overriding
    /// `common` — the same priority order as `get`, but exposed as one block.
    ///
    /// Used to ship the catalog to the browser (`GET /api/i18n`): the SPA
    /// resolves its keys client-side, which replaces the `{{key}}`
    /// substitution of old. The values remain **data** end to end:
    /// no character is dangerous, unlike raw substitution
    /// into JS source.
    pub fn entries(&self) -> HashMap<&str, &str> {
        let mut out: HashMap<&str, &str> =
            self.common.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        for (k, v) in &self.own {
            out.insert(k.as_str(), v.as_str());
        }
        out
    }
}

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
        let cat = Catalog::load("core", "en", dir.path(), "error = \"own-error\"\n");
        assert_eq!(cat.get("error"), "own-error");
    }

    #[test]
    fn an_external_pack_overrides_the_embedded_own() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "core", "fr.toml", "standby = \"VEILLE\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.get("standby"), "VEILLE");
    }

    #[test]
    fn an_external_pack_overrides_the_embedded_common() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "common", "fr.toml", "error = \"Erreur\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "");
        assert_eq!(cat.get("error"), "Erreur");
    }

    #[test]
    fn a_missing_key_falls_back_to_english_then_to_the_key_itself() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        // no fr pack: the embedded English is kept
        assert_eq!(cat.get("standby"), "STANDBY");
        // unknown key: the key itself is returned
        assert_eq!(cat.get("unknown"), "unknown");
    }

    #[test]
    fn invalid_toml_is_ignored_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "core", "fr.toml", "this = is not valid");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
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
        let cat = Catalog::load("core", "en", dir.path(), "error = \"own-error\"\nother = \"x\"\n");
        let e = cat.entries();
        assert_eq!(e.get("error").copied(), Some("own-error"));
        assert_eq!(e.get("other").copied(), Some("x"));
        // The common keys not redefined are present: the map is
        // complete, and it's what feeds `t()` on the browser side.
        assert!(e.len() > 1);
        assert!(e.keys().any(|k| *k == "play"), "the common vocabulary must be included");
    }

    /// French `common` pack shipped in the repo. Same parity invariant as
    /// for each component (see `core::settings::key_parity_between_the_embedded_en_and_the_fr_pack`),
    /// which was missing from the common layer: nothing flagged a key added
    /// to `common_en.toml` that had no French translation.
    fn common_fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/common/fr.toml");
        std::fs::read_to_string(p).expect("common fr pack shipped")
    }

    #[test]
    fn key_parity_between_the_embedded_common_and_the_fr_pack() {
        let en = try_parse(COMMON_EN).unwrap();
        let fr = try_parse(&common_fr_pack()).unwrap();
        let mut en_keys: Vec<&String> = en.keys().collect();
        let mut fr_keys: Vec<&String> = fr.keys().collect();
        en_keys.sort();
        fr_keys.sort();
        assert_eq!(en_keys, fr_keys, "common en/fr key sets diverge");
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
        let cat = Catalog::load("radio", "en", dir.path(), "");
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
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.entries().get("standby").copied(), Some("VEILLE"));
    }

}
