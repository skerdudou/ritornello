//! What the plugin and the root mount binary have in common.
//!
//! Two `[[bin]]` of the same crate do not share their modules: this library is
//! what guarantees that the privileged side and the side that writes the
//! configuration read exactly the same grammar.
//!
//! The modules specific to the plugin (`admin`, `mount`, `state`, `store`) stay
//! declared in `main.rs`: the mount binary has no use for them, and placing them
//! here would impose dependencies that a `oneshot` launched by systemd has no
//! reason to pull in.

pub mod duration;
pub mod explore;
pub mod m3u;
pub mod mount;
pub mod mount_options;
pub mod playlist;
pub mod roots;
pub mod health;
pub mod scan;
pub mod smb;
pub mod store;
pub mod volumes;

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
