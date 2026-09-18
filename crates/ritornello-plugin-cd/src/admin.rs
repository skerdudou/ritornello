//! Admin half: the single setting this source has — what it does when it is
//! arrived at.

use crate::state::{self, OnArrival};
use ritornello_plugin_sdk::AdminPlugin;
use ritornello_proto::Text;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Body of `SetData`, distinct from `State`: the field is **mandatory** here,
/// with no `#[serde(default)]`. That default is right for `state::load`, which
/// completes a partial file, but wrong for a write — a `PUT {}` must be
/// rejected, not silently understood as "put it back to play nothing".
///
/// Same read/write separation as `mpd::admin::ConfigWrite` and
/// `radio::admin::Op::Save`, each carrying a type dedicated to the request.
#[derive(Debug, Deserialize)]
struct SettingWrite {
    on_arrival: OnArrival,
}

pub struct CdAdmin {
    pub state_path: PathBuf,
    /// The live setting, shared with the Source half that reads it at every
    /// arrival. Written here **after** the disk write, never before: what is
    /// obeyed must be what is saved, otherwise a setting applied but not
    /// persisted would silently revert at the next restart.
    pub on_arrival: Arc<RwLock<OnArrival>>,
}

#[async_trait::async_trait]
impl AdminPlugin for CdAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => {
                Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string()))
            }
            "ui.css" => {
                Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string()))
            }
            _ => None,
        }
    }

    async fn get_data(&self) -> serde_json::Value {
        // Served from the shared value rather than re-read from disk: it is
        // the one the Source half actually obeys, so the page cannot show a
        // setting that is not the one in force.
        serde_json::json!({ "on_arrival": *self.on_arrival.read().unwrap() })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), Text> {
        let write: SettingWrite = serde_json::from_value(data).map_err(|e| Text::Keyed {
            key: "bad_request".into(),
            params: HashMap::from([("detail".to_string(), e.to_string())]),
        })?;
        // `update` and not `save`: the Source half writes the resume point
        // into this same file, and a state rebuilt here would erase it.
        //
        // The disk first, the shared value second. The other order would obey
        // a setting the file does not carry — a power cut in between, and the
        // device would come back on a setting the owner had changed.
        state::update(&self.state_path, |s| s.on_arrival = write.on_arrival).map_err(|e| {
            tracing::warn!("persisting the arrival setting: {e}");
            Text::Keyed { key: "save_failed".into(), params: HashMap::new() }
        })?;
        *self.on_arrival.write().unwrap() = write.on_arrival;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        admin: CdAdmin,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("plugin-cd.json");
        let admin = CdAdmin { state_path, on_arrival: Arc::new(RwLock::new(OnArrival::default())) };
        Fixture { admin, _dir: dir }
    }

    /// **Generalized (task 15).** Was "en vs fr, key sets only" — a
    /// hardcoded language that stops covering a second one the moment it
    /// ships, and a comparison blind to a translation that renamed or
    /// dropped a `{named}` parameter. `shipped_language_packs` derives the
    /// language list from the tree; the `assert!(!shipped.is_empty(), ...)`
    /// below is what keeps that derivation honest instead of vacuously
    /// green on a broken discovery.
    #[test]
    fn key_and_param_parity_between_the_embedded_en_and_every_shipped_language() {
        let en = ritornello_i18n::try_parse(crate::CD_EN).unwrap();
        let deploy_locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = ritornello_i18n::shipped_language_packs(&deploy_locales, "cd");
        assert!(!shipped.is_empty(), "no shipped language found for cd under deploy/locales");
        for (lang, content) in shipped {
            let pack = ritornello_i18n::try_parse(&content)
                .unwrap_or_else(|e| panic!("{lang} pack for cd is invalid TOML: {e}"));
            let mut en_keys: Vec<&String> = en.keys().collect();
            let mut pack_keys: Vec<&String> = pack.keys().collect();
            en_keys.sort();
            pack_keys.sort();
            assert_eq!(en_keys, pack_keys, "en/{lang} key sets diverge for cd");

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

    #[tokio::test]
    async fn get_data_returns_the_setting_in_force() {
        let f = fixture();
        assert_eq!(f.admin.get_data().await, serde_json::json!({ "on_arrival": "nothing" }));
    }

    #[tokio::test]
    async fn set_data_persists_and_replaces_the_shared_value() {
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "on_arrival": "last_track" })).await.is_ok());
        // What the page will show,
        assert_eq!(f.admin.get_data().await, serde_json::json!({ "on_arrival": "last_track" }));
        // what the Source half obeys,
        assert_eq!(*f.admin.on_arrival.read().unwrap(), OnArrival::LastTrack);
        // and what survives a restart. The three must agree, and the
        // difference is not academic: obeying a setting the file does not
        // carry makes the device change its mind at the next reboot.
        assert_eq!(state::load(&f.admin.state_path).on_arrival, OnArrival::LastTrack);
    }

    #[tokio::test]
    async fn set_data_accepts_the_three_values_and_only_those() {
        let mut f = fixture();
        for value in ["nothing", "first_track", "last_track"] {
            let r = f.admin.set_data(serde_json::json!({ "on_arrival": value })).await;
            assert!(r.is_ok(), "{value} must be accepted: {r:?}");
        }
        let r = f.admin.set_data(serde_json::json!({ "on_arrival": "eject_and_run" })).await;
        assert!(r.is_err(), "an unknown value must be refused, not silently ignored");
        // And the refusal leaves the setting in force untouched: a rejected
        // request must not be a way to reset it.
        assert_eq!(*f.admin.on_arrival.read().unwrap(), OnArrival::LastTrack);
    }

    #[tokio::test]
    async fn a_request_without_the_field_is_refused_not_defaulted() {
        // The whole reason `SettingWrite` exists: `state::load`'s default
        // completes a partial file, which is right when reading. Applied to a
        // write it would turn a malformed request into "play nothing" — a
        // silent reset of the owner's choice.
        let mut f = fixture();
        f.admin.set_data(serde_json::json!({ "on_arrival": "first_track" })).await.unwrap();
        assert!(f.admin.set_data(serde_json::json!({})).await.is_err());
        assert_eq!(*f.admin.on_arrival.read().unwrap(), OnArrival::FirstTrack);
    }

    #[tokio::test]
    async fn a_refusal_travels_as_a_key_and_its_parameters_not_a_sentence() {
        // The plugin no longer resolves anything (no `Catalog` left): the
        // core does, from the announced catalog, at `PUT /plugins/cd/api/data`
        // (see `ritornello-core`'s `resolve_admin_text`). What this test
        // owns is the shape the plugin still controls — the key and the
        // `{detail}` parameter, unresolved.
        let mut f = fixture();
        let err = f.admin.set_data(serde_json::json!({ "on_arrival": 7 })).await.unwrap_err();
        match err {
            Text::Keyed { key, params } => {
                assert_eq!(key, "bad_request");
                assert!(params.get("detail").is_some_and(|d| !d.is_empty()), "{params:?}");
            }
            Text::Verbatim(s) => panic!("a bad request must be a key, not verbatim text: {s}"),
        }
    }

    #[tokio::test]
    async fn saving_the_setting_keeps_the_resume_point() {
        // The two halves write into the same file. This is the test that
        // fails if either one ever stops going through `state::update`.
        let mut f = fixture();
        state::update(&f.admin.state_path, |s| {
            s.remembered = Some(state::Remembered { toc: "abcd1234".into(), track: 6 })
        })
        .unwrap();
        f.admin.set_data(serde_json::json!({ "on_arrival": "last_track" })).await.unwrap();
        let reread = state::load(&f.admin.state_path);
        assert_eq!(reread.on_arrival, OnArrival::LastTrack);
        assert_eq!(reread.remembered.unwrap().track, 6, "the resume point was erased by a save");
    }
}
