use crate::config::Stations;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

pub struct RadioAdmin {
    pub stations_path: PathBuf,
    pub stations: Arc<AsyncRwLock<Stations>>,
    pub catalog: Arc<RwLock<Catalog>>,
}

#[async_trait::async_trait]
impl AdminPlugin for RadioAdmin {
    fn page(&self) -> String {
        let cat = self.catalog.read().unwrap();
        let mut html = include_str!("index.html").to_string();
        for key in [
            "admin_title",
            "col_num",
            "col_name",
            "col_url",
            "btn_add",
            "btn_save",
            "load_error_1",
            "load_error_2",
            "saved",
            "save_error",
        ] {
            html = html.replace(&format!("{{{{{key}}}}}"), cat.get(key));
        }
        html
    }

    async fn get_data(&self) -> serde_json::Value {
        serde_json::to_value(&*self.stations.read().await).unwrap_or(serde_json::Value::Null)
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let stations: Stations =
            serde_json::from_value(data).map_err(|e| format!("JSON invalide : {e}"))?;
        stations
            .validate()
            .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
        stations.save(&self.stations_path).map_err(|e| e.to_string())?;
        *self.stations.write().await = stations;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Station;

    fn admin(dir: &std::path::Path) -> RadioAdmin {
        let path = dir.join("stations.toml");
        let stations = Stations {
            stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }],
        };
        stations.save(&path).unwrap();
        RadioAdmin {
            stations_path: path,
            stations: Arc::new(AsyncRwLock::new(stations)),
            catalog: Arc::new(RwLock::new(Catalog::load(
                "radio",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::RADIO_EN,
            ))),
        }
    }

    #[test]
    fn page_substitue_les_jetons_avec_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "btn_save = \"Enregistrer\"\n").unwrap();
        let a = RadioAdmin {
            stations_path: dir.path().join("stations.toml"),
            stations: Arc::new(AsyncRwLock::new(Stations::default())),
            catalog: Arc::new(RwLock::new(Catalog::load("radio", "fr", dir.path(), crate::RADIO_EN))),
        };
        let html = a.page();
        assert!(html.contains("Enregistrer"));
        assert!(!html.contains("{{btn_save}}"));
    }

    #[tokio::test]
    async fn get_data_renvoie_les_stations() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let v = a.get_data().await;
        assert_eq!(v["stations"][0]["name"], "FIP");
    }

    #[tokio::test]
    async fn set_data_valide_persiste_et_met_a_jour() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let nouveau = serde_json::json!({ "stations": [{ "name": "Inter", "url": "http://inter", "preset": 2 }] });
        assert!(a.set_data(nouveau).await.is_ok());
        assert_eq!(a.stations.read().await.stations[0].name, "Inter");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "Inter");
    }

    #[tokio::test]
    async fn set_data_invalide_renvoie_erreur_et_ne_persiste_pas() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let mauvais = serde_json::json!({ "stations": [{ "name": "X", "url": "http://x", "preset": 12 }] });
        assert!(a.set_data(mauvais).await.is_err());
        // l'état partagé et le disque restent inchangés
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
    }
}
