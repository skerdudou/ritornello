//! Admin half: the single setting this source has — what it does when it is
//! arrived at.

use crate::state::{self, OnArrival};
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
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
    pub catalog: Arc<RwLock<Catalog>>,
    /// Root of the on-disk language packs, kept so a catalog can be rebuilt in
    /// any requested language — `Catalog::load` only parses a TOML file, so
    /// this costs nothing per request.
    pub locales_root: PathBuf,
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

    fn catalog(&self, lang: Option<&str>) -> serde_json::Value {
        match lang {
            // The language the plugin was started in: the catalog already
            // built, no work at all.
            None => serde_json::json!(self.catalog.read().unwrap().entries()),
            // A language explicitly asked for. Rebuilt rather than translated
            // from the current one: the on-disk pack is the authority, and
            // only `Catalog::load` knows how to layer it over the embedded
            // English.
            Some(l) => {
                let c = Catalog::load("cd", l, &self.locales_root, crate::CD_EN);
                serde_json::json!(c.entries())
            }
        }
    }

    async fn get_data(&self) -> serde_json::Value {
        // Served from the shared value rather than re-read from disk: it is
        // the one the Source half actually obeys, so the page cannot show a
        // setting that is not the one in force.
        serde_json::json!({ "on_arrival": *self.on_arrival.read().unwrap() })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let write: SettingWrite = serde_json::from_value(data).map_err(|e| {
            self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e.to_string())
        })?;
        // `update` and not `save`: the Source half writes the resume point
        // into this same file, and a state rebuilt here would erase it.
        //
        // The disk first, the shared value second. The other order would obey
        // a setting the file does not carry — a power cut in between, and the
        // device would come back on a setting the owner had changed.
        state::update(&self.state_path, |s| s.on_arrival = write.on_arrival).map_err(|e| {
            tracing::warn!("persisting the arrival setting: {e}");
            self.catalog.read().unwrap().get("save_failed").to_string()
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
        let admin = CdAdmin {
            state_path,
            on_arrival: Arc::new(RwLock::new(OnArrival::default())),
            catalog: Arc::new(RwLock::new(Catalog::load(
                "cd",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::CD_EN,
            ))),
            locales_root: PathBuf::from("/nonexistent"),
        };
        Fixture { admin, _dir: dir }
    }

    /// French pack shipped in the repository.
    fn fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/cd/fr.toml");
        std::fs::read_to_string(p).expect("shipped fr pack")
    }

    #[test]
    fn key_parity_between_the_embedded_en_and_the_fr_pack() {
        let en = ritornello_i18n::try_parse(crate::CD_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let mut en_keys: Vec<&String> = en.keys().collect();
        let mut fr_keys: Vec<&String> = fr.keys().collect();
        en_keys.sort();
        fr_keys.sort();
        assert_eq!(en_keys, fr_keys, "en/fr key sets diverge");
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
    async fn a_refusal_is_a_sentence_never_a_catalog_key() {
        // The page displays this text as is (same convention as the other
        // plugins): returning the bare key would put `bad_request` on screen.
        let mut f = fixture();
        let err = f.admin.set_data(serde_json::json!({ "on_arrival": 7 })).await.unwrap_err();
        assert!(!err.is_empty());
        assert_ne!(err, "bad_request", "the key must have been resolved");
        assert!(!err.contains("{detail}"), "the placeholder must have been filled: {err}");
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
