use crate::bindings::Bindings;
use crate::devices::Hub;
use crate::presets;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Clés i18n substituées dans `index.html`. Deux tests les gardent alignées :
/// toutes présentes dans l'anglais embarqué, et aucun jeton `{{…}}` survivant
/// au rendu.
pub const PAGE_KEYS: &[&str] = &[
    "admin_title",
    "device_label",
    "btn_refresh",
    "col_action",
    "col_code",
    "btn_learn",
    "btn_clear",
    "preset_label",
    "btn_load_preset",
    "btn_import",
    "btn_export",
    "btn_save",
    "btn_cancel",
    "learning_msg",
    "learn_timeout",
    "saved",
    "save_error",
    "load_error",
    "no_device",
    "act_select_1",
    "act_select_2",
    "act_select_3",
    "act_select_4",
    "act_select_5",
    "act_select_6",
    "act_select_7",
    "act_select_8",
    "act_select_9",
    "act_next",
    "act_prev",
    "act_volume_up",
    "act_volume_down",
    "act_mute",
    "act_play_pause",
    "act_stop",
    "act_next_track",
    "act_prev_track",
    "act_eject",
    "act_source_cycle",
    "act_power",
];

/// Opérations portées par `SetData`, discriminées par le champ `op`.
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
}

#[async_trait::async_trait]
impl AdminPlugin for GenericInputAdmin {
    fn page(&self) -> String {
        let cat = self.catalog.read().unwrap();
        let mut html = include_str!("index.html").to_string();
        for key in PAGE_KEYS {
            html = html.replace(&format!("{{{{{key}}}}}"), cat.get(key));
        }
        html
    }

    async fn get_data(&self) -> serde_json::Value {
        // Aucune garde de verrou ne traverse un `.await` (il n'y en a aucun).
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
                bindings.save(&self.bindings_path).map_err(|e| e.to_string())?;
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
                // Rien n'est persisté : l'utilisateur enregistre ensuite.
                let bindings = presets::load(&self.presets_root, &preset)
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                self.hub.bindings.write().unwrap().replace_device(&device, bindings);
                Ok(())
            }
            Op::ImportPreset { device, content } => {
                // Contrairement à `load_preset` (fichiers livrés, réputés
                // valides), un fichier téléversé par l'utilisateur peut porter
                // des bindings invalides : on valide sur une copie avant de
                // toucher la table partagée, et rien n'est persisté ici non
                // plus — seul « Enregistrer » écrit sur disque.
                let bindings = presets::parse_preset(&content).map_err(|e| {
                    self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e)
                })?;
                let mut candidat = self.hub.bindings.read().unwrap().clone();
                candidat.replace_device(&device, bindings);
                candidat.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                *self.hub.bindings.write().unwrap() = candidat;
                Ok(())
            }
            Op::Rescan => {
                let n = self.hub.open_new_devices(&self.input_root);
                tracing::info!("rescan: {n} nouveau(x) peripherique(s) ouvert(s)");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, Device};
    use ritornello_proto::Command;
    use tokio::sync::mpsc;

    struct Fixture {
        admin: GenericInputAdmin,
        _rx: mpsc::Receiver<Command>,
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
            },
            _rx: rx,
            _dir: dir,
        }
    }

    #[test]
    fn page_substitue_tous_les_jetons() {
        let f = fixture();
        let html = f.admin.page();
        assert!(html.contains("input bindings"));
        assert!(!html.contains("{{"), "jeton non substitue dans la page");
    }

    #[test]
    fn page_utilise_le_catalogue_de_la_langue_courante() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "admin_title = \"touches\"\n",
        )
        .unwrap();
        let mut f = fixture();
        f.admin.catalog = Arc::new(RwLock::new(Catalog::load(
            "generic-input",
            "fr",
            dir.path(),
            crate::GENERIC_INPUT_EN,
        )));
        assert!(f.admin.page().contains("touches"));
    }

    #[test]
    fn toutes_les_cles_de_page_existent_dans_len_embarque() {
        let en = ritornello_i18n::try_parse(crate::GENERIC_INPUT_EN).unwrap();
        for key in PAGE_KEYS {
            assert!(en.contains_key(*key), "cle absente de en.toml: {key}");
        }
    }

    /// Pack français livré dans le dépôt.
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/generic-input/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::GENERIC_INPUT_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }

    #[test]
    fn la_page_expose_les_21_actions() {
        let f = fixture();
        let html = f.admin.page();
        for label in [
            "Preset 1", "Preset 9", "Next preset", "Previous preset", "Volume +", "Volume -",
            "Mute", "Play/pause", "Stop", "Next track", "Previous track", "Eject",
            "Change source", "Standby",
        ] {
            assert!(html.contains(label), "libelle absent de la page: {label}");
        }
    }

    #[tokio::test]
    async fn get_data_renvoie_devices_bindings_presets_learning() {
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
    async fn save_valide_persiste_et_remplace_la_table() {
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
    async fn save_invalide_renvoie_une_erreur_traduite_et_ne_persiste_pas() {
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
        assert!(err.contains("code 1"), "message inattendu: {err}");
        assert!(!f.admin.bindings_path.exists());
        // la table partagée est intacte
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("eHome", 2),
            Some(Command::Select(1))
        );
    }

    #[tokio::test]
    async fn learn_puis_cancel_learn() {
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
    async fn load_preset_remplace_en_memoire_sans_persister() {
        let mut f = fixture();
        let op = serde_json::json!({ "op": "load_preset", "device": "eHome", "preset": "mce" });
        assert!(f.admin.set_data(op).await.is_ok());
        let b = f.admin.hub.bindings.read().unwrap();
        assert_eq!(b.resolve("eHome", 115), Some(Command::VolumeUp));
        // les anciens bindings du périphérique ont été remplacés
        assert_eq!(b.resolve("eHome", 2), None);
        drop(b);
        // rien sur le disque
        assert!(!f.admin.bindings_path.exists());
    }

    #[tokio::test]
    async fn load_preset_inconnu_renvoie_une_erreur() {
        let mut f = fixture();
        let op = serde_json::json!({ "op": "load_preset", "device": "eHome", "preset": "zzz" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("zzz"), "message inattendu: {err}");
    }

    #[tokio::test]
    async fn import_preset_remplace_en_memoire_sans_persister() {
        let mut f = fixture();
        let content = "[[bindings]]\ncode = 3\ncmd = \"Mute\"\n";
        let op = serde_json::json!({ "op": "import_preset", "device": "eHome", "content": content });
        assert!(f.admin.set_data(op).await.is_ok());
        let b = f.admin.hub.bindings.read().unwrap();
        assert_eq!(b.resolve("eHome", 3), Some(Command::Mute));
        // les anciens bindings du périphérique ont été remplacés
        assert_eq!(b.resolve("eHome", 2), None);
        drop(b);
        // rien sur le disque
        assert!(!f.admin.bindings_path.exists());
    }

    #[tokio::test]
    async fn import_preset_toml_invalide_renvoie_une_erreur_traduite_et_ne_change_rien() {
        let mut f = fixture();
        let op = serde_json::json!({
            "op": "import_preset",
            "device": "eHome",
            "content": "ceci n'est pas = du toml [",
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.starts_with("invalid request:"), "message inattendu: {err}");
        assert!(!f.admin.bindings_path.exists());
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("eHome", 2),
            Some(Command::Select(1))
        );
    }

    #[tokio::test]
    async fn import_preset_bindings_invalides_renvoie_une_erreur_et_ne_change_rien() {
        let mut f = fixture();
        let content = "[[bindings]]\ncode = 2\ncmd = \"Mute\"\n\n[[bindings]]\ncode = 2\ncmd = \"Stop\"\n";
        let op = serde_json::json!({ "op": "import_preset", "device": "eHome", "content": content });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("code 2"), "message inattendu: {err}");
        assert!(!f.admin.bindings_path.exists());
        // la table partagée est intacte (ancien binding du périphérique)
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("eHome", 2),
            Some(Command::Select(1))
        );
    }

    #[tokio::test]
    async fn rescan_sans_peripherique_reussit() {
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "op": "rescan" })).await.is_ok());
    }

    #[tokio::test]
    async fn op_inconnue_renvoie_une_erreur() {
        let mut f = fixture();
        let err = f.admin.set_data(serde_json::json!({ "op": "detruire" })).await.unwrap_err();
        assert!(err.starts_with("invalid request:"), "message inattendu: {err}");
        let err2 = f.admin.set_data(serde_json::json!({ "rien": 1 })).await.unwrap_err();
        assert!(err2.starts_with("invalid request:"), "message inattendu: {err2}");
    }
}
