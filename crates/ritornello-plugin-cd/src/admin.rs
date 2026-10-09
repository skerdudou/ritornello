//! Admin half: the two settings this source has — what it does when it is
//! arrived at, and what it does when a disc is inserted.

use crate::state::{self, OnArrival, OnInsertion};
use ritornello_plugin_sdk::AdminPlugin;
use ritornello_proto::Text;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Body of `SetData`, distinct from `State`: both fields are **mandatory** here,
/// with no `#[serde(default)]`. That default is right for `state::load`, which
/// completes a partial file, but wrong for a write — a `PUT {}` must be
/// rejected, not silently understood as "put it back to play nothing".
///
/// Same read/write separation as `mpd::admin::ConfigWrite` and
/// `radio::admin::Op::Save`, each carrying a type dedicated to the request.
#[derive(Debug, Deserialize)]
struct SettingWrite {
    on_arrival: OnArrival,
    on_insertion: OnInsertion,
}

pub struct CdAdmin {
    pub state_path: PathBuf,
    /// The live setting, shared with the Source half that reads it at every
    /// arrival. Written here **after** the disk write, never before: what is
    /// obeyed must be what is saved, otherwise a setting applied but not
    /// persisted would silently revert at the next restart.
    pub on_arrival: Arc<RwLock<OnArrival>>,
    /// Same contract as `on_arrival`, for the insertion setting.
    pub on_insertion: Arc<RwLock<OnInsertion>>,
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
        serde_json::json!({
            "on_arrival": *self.on_arrival.read().unwrap(),
            "on_insertion": *self.on_insertion.read().unwrap(),
        })
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
        state::update(&self.state_path, |s| {
            s.on_arrival = write.on_arrival;
            s.on_insertion = write.on_insertion;
        })
        .map_err(|e| {
            tracing::warn!("persisting the settings: {e}");
            Text::Keyed { key: "save_failed".into(), params: HashMap::new() }
        })?;
        *self.on_arrival.write().unwrap() = write.on_arrival;
        *self.on_insertion.write().unwrap() = write.on_insertion;
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
        let admin = CdAdmin {
            state_path,
            on_arrival: Arc::new(RwLock::new(OnArrival::default())),
            on_insertion: Arc::new(RwLock::new(OnInsertion::default())),
        };
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
    async fn set_data_persists_both_settings_and_replaces_the_shared_values() {
        let mut f = fixture();
        let body = serde_json::json!({ "on_arrival": "first_track", "on_insertion": "switch_and_play" });
        f.admin.set_data(body).await.unwrap();
        assert_eq!(*f.admin.on_insertion.read().unwrap(), OnInsertion::SwitchAndPlay);
        assert_eq!(state::load(&f.admin.state_path).on_insertion, OnInsertion::SwitchAndPlay);
        assert_eq!(f.admin.get_data().await["on_insertion"], "switch_and_play");
    }

    #[tokio::test]
    async fn set_data_accepts_the_four_insertion_values_and_only_those() {
        let mut f = fixture();
        for v in ["nothing", "play_if_active", "switch_and_play", "wake_switch_and_play"] {
            let r = f.admin.set_data(serde_json::json!({ "on_arrival": "nothing", "on_insertion": v })).await;
            assert!(r.is_ok(), "{v} must be accepted: {r:?}");
        }
        let r = f
            .admin
            .set_data(serde_json::json!({ "on_arrival": "nothing", "on_insertion": "explode" }))
            .await;
        assert!(r.is_err());
        // A refusal leaves the value in force untouched.
        assert_eq!(*f.admin.on_insertion.read().unwrap(), OnInsertion::WakeSwitchAndPlay);
    }

    #[tokio::test]
    async fn a_request_without_the_insertion_field_is_refused_not_defaulted() {
        // Both fields are mandatory on a write: no backward compatibility
        // before the final release, and a missing one must not silently reset
        // the owner's choice.
        let mut f = fixture();
        f.admin
            .set_data(serde_json::json!({ "on_arrival": "nothing", "on_insertion": "switch_and_play" }))
            .await
            .unwrap();
        assert!(f.admin.set_data(serde_json::json!({ "on_arrival": "nothing" })).await.is_err());
        assert_eq!(*f.admin.on_insertion.read().unwrap(), OnInsertion::SwitchAndPlay);
    }

    #[tokio::test]
    async fn saving_the_insertion_setting_keeps_the_other_fields_of_the_file() {
        let mut f = fixture();
        state::update(&f.admin.state_path, |s| {
            s.remembered = Some(state::Remembered { toc: "abcd1234".into(), track: 6 })
        })
        .unwrap();
        f.admin
            .set_data(serde_json::json!({ "on_arrival": "first_track", "on_insertion": "play_if_active" }))
            .await
            .unwrap();
        let reread = state::load(&f.admin.state_path);
        assert_eq!(reread.on_arrival, OnArrival::FirstTrack);
        assert_eq!(reread.on_insertion, OnInsertion::PlayIfActive);
        assert_eq!(reread.remembered.unwrap().track, 6);
    }

    #[tokio::test]
    async fn get_data_returns_the_setting_in_force() {
        let f = fixture();
        assert_eq!(f.admin.get_data().await,
            serde_json::json!({ "on_arrival": "nothing", "on_insertion": "nothing" })
        );
    }

    #[tokio::test]
    async fn set_data_persists_and_replaces_the_shared_value() {
        let mut f = fixture();
        let body = serde_json::json!({ "on_arrival": "last_track", "on_insertion": "nothing" });
        assert!(f.admin.set_data(body).await.is_ok());
        // What the page will show,
        assert_eq!(
            f.admin.get_data().await,
            serde_json::json!({ "on_arrival": "last_track", "on_insertion": "nothing" })
        );
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
            let r = f
                .admin
                .set_data(serde_json::json!({ "on_arrival": value, "on_insertion": "nothing" }))
                .await;
            assert!(r.is_ok(), "{value} must be accepted: {r:?}");
        }
        let r = f
            .admin
            .set_data(serde_json::json!({ "on_arrival": "eject_and_run", "on_insertion": "nothing" }))
            .await;
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
        f.admin
            .set_data(serde_json::json!({ "on_arrival": "first_track", "on_insertion": "nothing" }))
            .await
            .unwrap();
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
        let err = f
            .admin
            .set_data(serde_json::json!({ "on_arrival": 7, "on_insertion": "nothing" }))
            .await
            .unwrap_err();
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
        f.admin
            .set_data(serde_json::json!({ "on_arrival": "last_track", "on_insertion": "nothing" }))
            .await
            .unwrap();
        let reread = state::load(&f.admin.state_path);
        assert_eq!(reread.on_arrival, OnArrival::LastTrack);
        assert_eq!(reread.remembered.unwrap().track, 6, "the resume point was erased by a save");
    }
}
