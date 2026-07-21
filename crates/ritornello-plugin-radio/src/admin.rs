use crate::config::Stations;
use ritornello_plugin_sdk::AdminPlugin;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct RadioAdmin {
    pub stations_path: PathBuf,
    pub stations: Arc<RwLock<Stations>>,
}

#[async_trait::async_trait]
impl AdminPlugin for RadioAdmin {
    fn page(&self) -> &'static str {
        include_str!("index.html")
    }

    async fn get_data(&self) -> serde_json::Value {
        serde_json::to_value(&*self.stations.read().await).unwrap_or(serde_json::Value::Null)
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let stations: Stations =
            serde_json::from_value(data).map_err(|e| format!("JSON invalide : {e}"))?;
        stations.validate().map_err(|e| e.to_string())?;
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
        RadioAdmin { stations_path: path, stations: Arc::new(RwLock::new(stations)) }
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
