use crate::bindings::Bindings;
use crate::devices::Hub;
use crate::presets;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Operations carried by `SetData`, discriminated by the `op` field.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Save { bindings: Bindings },
    Learn { device: String },
    CancelLearn,
    LoadPreset { device: String, preset: String },
    ImportPreset { device: String, content: String },
    Rescan,
}

pub struct GenericInputAdmin {
    pub bindings_path: PathBuf,
    pub presets_root: PathBuf,
    pub input_root: PathBuf,
    pub hub: Hub,
    pub catalog: Arc<RwLock<Catalog>>,
    /// Root of the on-disk language packs, kept so a catalog can be rebuilt in
    /// any requested language — `Catalog::load` only parses a TOML file, so
    /// this costs nothing per request.
    pub locales_root: PathBuf,
}

#[async_trait::async_trait]
impl AdminPlugin for GenericInputAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => Some((
                "text/javascript".to_string(),
                include_str!("../ui/dist/ui.js").to_string(),
            )),
            "ui.css" => Some((
                "text/css".to_string(),
                include_str!("../ui/dist/ui.css").to_string(),
            )),
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
                let c = Catalog::load("generic-input", l, &self.locales_root, crate::GENERIC_INPUT_EN);
                serde_json::json!(c.entries())
            }
        }
    }

    async fn get_data(&self) -> serde_json::Value {
        // No lock guard crosses an `.await` (there is none).
        let devices = self.hub.device_names();
        let bindings = self.hub.bindings.read().unwrap().clone();
        let learning = self.hub.learn.read().unwrap().snapshot();
        let presets = presets::list(&self.presets_root);
        serde_json::json!({
            "devices": devices,
            "bindings": bindings,
            "presets": presets,
            "learning": learning,
        })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let op: Op = serde_json::from_value(data).map_err(|e| {
            self.catalog
                .read()
                .unwrap()
                .get("bad_request")
                .replace("{detail}", &e.to_string())
        })?;
        match op {
            Op::Save { bindings } => {
                bindings.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                bindings.save(&self.bindings_path).map_err(|e| {
                    // Same split as in radio: the I/O detail goes to the log,
                    // not into the response body.
                    tracing::warn!("failed to save bindings: {e}");
                    self.catalog.read().unwrap().get("save_failed").to_string()
                })?;
                *self.hub.bindings.write().unwrap() = bindings;
                Ok(())
            }
            Op::Learn { device } => {
                self.hub.learn.write().unwrap().learn(&device);
                Ok(())
            }
            Op::CancelLearn => {
                self.hub.learn.write().unwrap().cancel();
                Ok(())
            }
            Op::LoadPreset { device, preset } => {
                // Nothing is persisted: the user saves afterwards.
                // Same validation on a copy as `ImportPreset`: the presets
                // directory is configurable and open to the operator, so
                // "shipped files, deemed valid" does not hold. Without this, an
                // invalid preset became active in memory and it was the next
                // "Save" that failed — on a table the UI itself had produced.
                let bindings = presets::load(&self.presets_root, &preset)
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                let mut candidate = self.hub.bindings.read().unwrap().clone();
                candidate.replace_device(&device, bindings);
                candidate.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                *self.hub.bindings.write().unwrap() = candidate;
                Ok(())
            }
            Op::ImportPreset { device, content } => {
                // Unlike `load_preset` (shipped files, deemed valid), a file
                // uploaded by the user may carry invalid bindings: we validate
                // on a copy before touching the shared table, and nothing is
                // persisted here either — only "Save" writes to disk.
                let bindings = presets::parse_preset(&content).map_err(|e| {
                    self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e)
                })?;
                let mut candidate = self.hub.bindings.read().unwrap().clone();
                candidate.replace_device(&device, bindings);
                candidate.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                *self.hub.bindings.write().unwrap() = candidate;
                Ok(())
            }
            Op::Rescan => {
                let n = self.hub.open_new_devices(&self.input_root);
                tracing::info!("rescan: {n} new device(s) opened");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, Device};
    use ritornello_proto::{Command, InputMessage};
    use tokio::sync::mpsc;

    struct Fixture {
        admin: GenericInputAdmin,
        _rx: mpsc::Receiver<InputMessage>,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let presets_root = dir.path().join("presets");
        std::fs::create_dir_all(&presets_root).unwrap();
        std::fs::write(
            presets_root.join("mce.toml"),
            "[[bindings]]\ncode = 115\ncmd = \"VolumeUp\"\n",
        )
        .unwrap();
        let input_root = dir.path().join("input");
        std::fs::create_dir_all(&input_root).unwrap();

        let bindings = Bindings {
            devices: vec![Device {
                name: "eHome".into(),
                bindings: vec![Binding::new(2, &Command::Select(1))],
            }],
        };
        let (tx, rx) = mpsc::channel(8);
        let hub = Hub::new(bindings, tx);
        hub.open
            .write()
            .unwrap()
            .insert(std::path::PathBuf::from("/dev/input/event0"), "eHome".into());
        let catalog = Arc::new(RwLock::new(Catalog::load(
            "generic-input",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::GENERIC_INPUT_EN,
        )));
        Fixture {
            admin: GenericInputAdmin {
                bindings_path: dir.path().join("input-bindings.toml"),
                presets_root,
                input_root,
                hub,
                catalog,
                locales_root: std::path::PathBuf::from("/nonexistent"),
            },
            _rx: rx,
            _dir: dir,
        }
    }

    #[test]
    fn asset_exposes_ui_js_and_ui_css_and_nothing_else() {
        let f = fixture();
        let (mime, body) = f.admin.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!body.is_empty());
        assert_eq!(f.admin.asset("ui.css").unwrap().0, "text/css");
        // An unknown path is not an error: it is a 404 on the core side.
        assert!(f.admin.asset("../../../etc/passwd").is_none());
        assert!(f.admin.asset("index.html").is_none());
    }

    #[test]
    fn catalog_exposes_the_component_keys() {
        let f = fixture();
        let v = f.admin.catalog(None);
        assert!(v["btn_save"].is_string(), "the catalog must carry the plugin's keys");
    }

    #[test]
    fn a_requested_language_is_honoured_whatever_the_current_one() {
        // The plugin is started in English; asking for another language must
        // rebuild, not return the current catalog. This is the whole basis of
        // the `immutable` answer served over HTTP.
        let mut f = fixture();
        let locales = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(locales.path().join("generic-input")).unwrap();
        std::fs::write(
            locales.path().join("generic-input/fr.toml"),
            "btn_save = \"Enregistrer\"\n",
        )
        .unwrap();
        f.admin.locales_root = locales.path().to_path_buf();
        let en = f.admin.catalog(None);
        let fr = f.admin.catalog(Some("fr"));
        assert_ne!(en, fr);
        assert_eq!(fr["btn_save"], "Enregistrer");
    }

    /// French pack shipped in the repository.
    fn fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/generic-input/fr.toml");
        std::fs::read_to_string(p).expect("shipped fr pack")
    }

    #[test]
    fn key_parity_between_the_embedded_en_and_the_fr_pack() {
        let en = ritornello_i18n::try_parse(crate::GENERIC_INPUT_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let mut en_keys: Vec<&String> = en.keys().collect();
        let mut fr_keys: Vec<&String> = fr.keys().collect();
        en_keys.sort();
        fr_keys.sort();
        assert_eq!(en_keys, fr_keys, "en/fr key sets diverge");
    }

    #[tokio::test]
    async fn get_data_returns_devices_bindings_presets_learning() {
        let f = fixture();
        let v = f.admin.get_data().await;
        assert_eq!(v["devices"], serde_json::json!(["eHome"]));
        assert_eq!(v["bindings"]["devices"][0]["name"], "eHome");
        assert_eq!(v["bindings"]["devices"][0]["bindings"][0]["cmd"], "Select");
        assert_eq!(v["bindings"]["devices"][0]["bindings"][0]["arg"], 1);
        assert_eq!(v["presets"], serde_json::json!(["mce"]));
        assert_eq!(v["learning"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn valid_save_persists_and_replaces_the_table() {
        let mut f = fixture();
        let op = serde_json::json!({
            "op": "save",
            "bindings": { "devices": [
                { "name": "USB Keyboard", "bindings": [{ "code": 57, "cmd": "PlayPause" }] }
            ]}
        });
        assert!(f.admin.set_data(op).await.is_ok());
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("USB Keyboard", 57),
            Some(Command::PlayPause)
        );
        assert_eq!(
            Bindings::load(&f.admin.bindings_path).resolve("USB Keyboard", 57),
            Some(Command::PlayPause)
        );
    }

    #[tokio::test]
    async fn a_write_failure_returns_a_catalog_sentence_not_the_io_detail() {
        // Same regression as in radio: `Bindings::save(...).map_err(|e|
        // e.to_string())` put the raw I/O detail in the response body.
        // `bindings_path` here targets an ordinary file as if it were a parent
        // directory, to make `create_dir_all` fail without touching the real
        // disk.
        let mut f = fixture();
        let obstacle = f.admin.presets_root.parent().unwrap().join("obstacle");
        std::fs::write(&obstacle, b"not a directory").unwrap();
        f.admin.bindings_path = obstacle.join("input-bindings.toml");
        let op = serde_json::json!({
            "op": "save",
            "bindings": { "devices": [
                { "name": "USB Keyboard", "bindings": [{ "code": 57, "cmd": "PlayPause" }] }
            ]}
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "the save failed");
    }

    #[tokio::test]
    async fn invalid_save_returns_a_translated_error_and_does_not_persist() {
        let mut f = fixture();
        let op = serde_json::json!({
            "op": "save",
            "bindings": { "devices": [
                { "name": "X", "bindings": [
                    { "code": 1, "cmd": "Select", "arg": 1 },
                    { "code": 1, "cmd": "Mute" }
                ]}
            ]}
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("code 1"), "unexpected message: {err}");
        assert!(!f.admin.bindings_path.exists());
        // the shared table is intact
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("eHome", 2),
            Some(Command::Select(1))
        );
    }

    #[tokio::test]
    async fn learn_then_cancel_learn() {
        let mut f = fixture();
        assert!(f
            .admin
            .set_data(serde_json::json!({ "op": "learn", "device": "eHome" }))
            .await
            .is_ok());
        assert_eq!(f.admin.get_data().await["learning"]["device"], "eHome");
        assert_eq!(
            f.admin.get_data().await["learning"]["captured"],
            serde_json::Value::Null
        );
        assert!(f.admin.set_data(serde_json::json!({ "op": "cancel_learn" })).await.is_ok());
        assert_eq!(f.admin.get_data().await["learning"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn load_preset_replaces_in_memory_without_persisting() {
        let mut f = fixture();
        let op = serde_json::json!({ "op": "load_preset", "device": "eHome", "preset": "mce" });
        assert!(f.admin.set_data(op).await.is_ok());
        let b = f.admin.hub.bindings.read().unwrap();
        assert_eq!(b.resolve("eHome", 115), Some(Command::VolumeUp));
        // the device's old bindings have been replaced
        assert_eq!(b.resolve("eHome", 2), None);
        drop(b);
        // nothing on disk
        assert!(!f.admin.bindings_path.exists());
    }

    #[tokio::test]
    async fn load_unknown_preset_returns_an_error() {
        let mut f = fixture();
        let op = serde_json::json!({ "op": "load_preset", "device": "eHome", "preset": "zzz" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("zzz"), "unexpected message: {err}");
    }

    #[tokio::test]
    async fn import_preset_replaces_in_memory_without_persisting() {
        let mut f = fixture();
        let content = "[[bindings]]\ncode = 3\ncmd = \"Mute\"\n";
        let op = serde_json::json!({ "op": "import_preset", "device": "eHome", "content": content });
        assert!(f.admin.set_data(op).await.is_ok());
        let b = f.admin.hub.bindings.read().unwrap();
        assert_eq!(b.resolve("eHome", 3), Some(Command::Mute));
        // the device's old bindings have been replaced
        assert_eq!(b.resolve("eHome", 2), None);
        drop(b);
        // nothing on disk
        assert!(!f.admin.bindings_path.exists());
    }

    #[tokio::test]
    async fn import_preset_invalid_toml_returns_a_translated_error_and_changes_nothing() {
        let mut f = fixture();
        let op = serde_json::json!({
            "op": "import_preset",
            "device": "eHome",
            "content": "this is not = toml [",
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.starts_with("invalid request:"), "unexpected message: {err}");
        assert!(!f.admin.bindings_path.exists());
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("eHome", 2),
            Some(Command::Select(1))
        );
    }

    #[tokio::test]
    async fn import_preset_invalid_bindings_returns_an_error_and_changes_nothing() {
        let mut f = fixture();
        let content = "[[bindings]]\ncode = 2\ncmd = \"Mute\"\n\n[[bindings]]\ncode = 2\ncmd = \"Stop\"\n";
        let op = serde_json::json!({ "op": "import_preset", "device": "eHome", "content": content });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("code 2"), "unexpected message: {err}");
        assert!(!f.admin.bindings_path.exists());
        // the shared table is intact (the device's old binding)
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("eHome", 2),
            Some(Command::Select(1))
        );
    }

    #[tokio::test]
    async fn rescan_without_a_device_succeeds() {
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "op": "rescan" })).await.is_ok());
    }

    #[tokio::test]
    async fn unknown_op_returns_an_error() {
        let mut f = fixture();
        let err = f.admin.set_data(serde_json::json!({ "op": "destroy" })).await.unwrap_err();
        assert!(err.starts_with("invalid request:"), "unexpected message: {err}");
        let err2 = f.admin.set_data(serde_json::json!({ "nothing": 1 })).await.unwrap_err();
        assert!(err2.starts_with("invalid request:"), "unexpected message: {err2}");
    }
}
