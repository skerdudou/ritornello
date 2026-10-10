//! The plugin's modules, as a library so its tests and `main.rs` share them.
//!
//! What the plugin has in common with the root mount helper (the declared
//! roots, the mount options, the `/proc/mounts` table) lives in the
//! `ritornello-files-mount` crate, which ships as a component of its own.
//!
//! The modules specific to the plugin (`admin`, `state`) stay declared in
//! `main.rs`.

pub mod duration;
pub mod explore;
pub mod m3u;
pub mod mount;
pub mod playlist;
pub mod root_error;
pub mod health;
pub mod location;
pub mod scan;
pub mod smb;
pub mod store;
pub mod volumes;

// The declared roots are read by the root mount helper and by this plugin from
// one grammar, which lives in `ritornello-files-mount`; re-exported so the
// plugin's paths (`crate::roots`) do not change.
pub use ritornello_files_mount::roots;

// Only compiled under `cargo test`: nothing in this module is used at runtime
// in this crate. It is used by `build.rs` (separate compilation, via
// `include!`) and by its own tests. Compiling it continuously would trigger a
// `dead_code` that `-D warnings` would refuse.
#[cfg(test)]
mod placeholder;

/// Embedded English catalog, fallen back on when the requested locale is missing.
pub const FILES_EN: &str = include_str!("locales/en.toml");

#[cfg(test)]
mod tests {
    use crate::root_error::RootErrorText;
    use crate::roots::RootError;
    use ritornello_proto::Text;

    /// Lives here and not next to `RootError`: the catalog it checks against
    /// is the plugin's, and `ritornello-files-mount` (a root binary) carries none.
    #[test]
    fn every_refusal_names_a_key_that_exists_in_the_embedded_catalog() {
        // The plugin no longer resolves its own refusals (no `Catalog` left
        // — language-packs chantier, task 9): resolution moved to the core.
        // What this test still owns is that the key named here is not a
        // typo. `Catalog::get` used to fall back silently on an unknown key,
        // which is exactly the failure this test caught before there was
        // anything left here to resolve.
        let known = ritornello_i18n::try_parse(crate::FILES_EN).unwrap();
        let texts = [
            RootError::BadName { name: "x/y".into() }.text(),
            RootError::BadHost { host: "a,b".into() }.text(),
            RootError::BadShare { share: "a,b".into() }.text(),
            RootError::BadSubpath { subpath: "..".into() }.text(),
            RootError::DuplicateName { name: "nas".into() }.text(),
            RootError::RelativeLocalPath { path: "media/usb".into() }.text(),
        ];
        for t in &texts {
            match t {
                Text::Keyed { key, .. } => assert!(known.contains_key(key), "unknown key: {key}"),
                Text::Verbatim(s) => panic!("a root refusal must be a key, not verbatim text: {s}"),
            }
        }
        // And the interpolation parameter travels, ready for the core to
        // substitute — never a collage of strings here (the trap `AGENTS.md`
        // names for a number glued to a label, the same trap for a value).
        match (RootError::BadHost { host: "nas,uid=0".into() }).text() {
            Text::Keyed { params, .. } => {
                assert_eq!(params.get("host").map(String::as_str), Some("nas,uid=0"));
            }
            Text::Verbatim(_) => panic!("expected a keyed text"),
        }
    }

    /// **Generalized (task 15).** Was "en vs fr, key sets only" — a
    /// hardcoded language that stops covering a second one the moment it
    /// ships, and a comparison blind to a translation that renamed or
    /// dropped a `{named}` parameter (a key present on one side only would
    /// show English in the middle of French, without warning:
    /// `Catalog::load` silently falls back on the embedded one key by
    /// key). `shipped_language_packs` derives the language list from the
    /// tree; the `assert!(!shipped.is_empty(), ...)` below is what keeps
    /// that derivation honest instead of vacuously green on a broken
    /// discovery.
    #[test]
    fn key_and_param_parity_between_the_embedded_en_and_every_shipped_language() {
        let en = ritornello_i18n::try_parse(crate::FILES_EN).unwrap();
        let deploy_locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = ritornello_i18n::shipped_language_packs(&deploy_locales, "files");
        assert!(!shipped.is_empty(), "no shipped language found for files under deploy/locales");
        for (lang, content) in shipped {
            let pack = ritornello_i18n::try_parse(&content)
                .unwrap_or_else(|e| panic!("{lang} pack for files is invalid TOML: {e}"));
            let mut en_keys: Vec<&String> = en.keys().collect();
            let mut pack_keys: Vec<&String> = pack.keys().collect();
            en_keys.sort();
            pack_keys.sort();
            assert_eq!(en_keys, pack_keys, "en/{lang} key sets diverge for files");

            for (key, en_value) in &en {
                if let Some(translated) = pack.get(key) {
                    assert_eq!(
                        ritornello_i18n::params_in(en_value),
                        ritornello_i18n::params_in(translated),
                        "key {key}: {lang} translation's named parameters diverge from English"
                    );
                }
            }
        }
    }
}
