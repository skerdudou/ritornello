//! The web-streaming source plugin, as a library so `main.rs` and the tests share
//! its embedded texts.

pub mod download;
pub mod entries;
pub mod jobs;
pub mod link;
pub mod netguard;
pub mod playfail;
pub mod reason;
pub mod settings;
pub mod state;
pub mod targets;
pub mod tool;
pub mod ytdlp;

pub const WEB_STREAMING_EN: &str = include_str!("locales/en.toml");

// Only compiled under `cargo test`: `ui_placeholder_js` serves nowhere at run
// time (only `build.rs`, through `include!`, and its own tests use it).
// Compiling it permanently would trigger a `dead_code` that `-D warnings`
// refuses.
#[cfg(test)]
mod placeholder;

#[cfg(test)]
mod tests {
    /// Every shipped language carries exactly the English keys, and the same
    /// named parameters on each: a key missing on one side would show English
    /// in the middle of a translation, because the catalogue falls back key by
    /// key without warning. The language list is derived from the tree, and
    /// the `assert!(!shipped.is_empty(), ...)` keeps that derivation honest.
    #[test]
    fn key_and_param_parity_between_the_embedded_en_and_every_shipped_language() {
        let en = ritornello_i18n::try_parse(crate::WEB_STREAMING_EN).unwrap();
        let deploy_locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = ritornello_i18n::shipped_language_packs(&deploy_locales, "web-streaming");
        assert!(!shipped.is_empty(), "no shipped language found for web-streaming under deploy/locales");
        for (lang, content) in shipped {
            let pack = ritornello_i18n::try_parse(&content)
                .unwrap_or_else(|e| panic!("{lang} pack for web-streaming is invalid TOML: {e}"));
            let mut en_keys: Vec<&String> = en.keys().collect();
            let mut pack_keys: Vec<&String> = pack.keys().collect();
            en_keys.sort();
            pack_keys.sort();
            assert_eq!(en_keys, pack_keys, "en/{lang} key sets diverge for web-streaming");

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
