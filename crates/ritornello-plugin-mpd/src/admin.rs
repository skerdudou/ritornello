use crate::config::Config;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub struct MpdAdmin {
    pub config_path: PathBuf,
    /// Copie en mémoire, distincte de celle qui a servi à ouvrir le socket
    /// TCP au démarrage : un changement n'y prend effet qu'au redémarrage du
    /// greffon (`restart_notice`), donc rien ici ne pilote la moitié réseau.
    /// Elle sert seulement à ce que `get_data` reflète le dernier
    /// enregistrement réussi sans relire le disque à chaque requête.
    pub config: RwLock<Config>,
    pub catalog: Arc<RwLock<Catalog>>,
}

#[async_trait::async_trait]
impl AdminPlugin for MpdAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => {
                Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string()))
            }
            "ui.css" => Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string())),
            _ => None,
        }
    }

    fn catalog(&self) -> serde_json::Value {
        let cat = self.catalog.read().unwrap();
        serde_json::json!(cat.entries())
    }

    async fn get_data(&self) -> serde_json::Value {
        let c = self.config.read().unwrap();
        serde_json::json!({ "listen": c.listen, "port": c.port })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let config: Config = serde_json::from_value(data).map_err(|e| {
            self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e.to_string())
        })?;
        // `enregistrer` valide puis écrit atomiquement ; dans les deux cas
        // d'échec, elle renvoie une **clé** de catalogue (`listen_empty`,
        // `port_zero`, `save_failed`), jamais un détail d'E/S brut. C'est ici,
        // et seulement ici, que la clé devient une phrase : la page Vue
        // affiche `error` tel quel, sans la retraduire (voir le rapport de la
        // moitié IHM) — renvoyer la clé nue la ferait apparaître littéralement
        // à l'écran.
        config
            .enregistrer(&self.config_path)
            .map_err(|cle| self.catalog.read().unwrap().get(&cle).to_string())?;
        *self.config.write().unwrap() = config;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        admin: MpdAdmin,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("mpd.toml");
        let catalog = Arc::new(RwLock::new(Catalog::load(
            "mpd",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::MPD_EN,
        )));
        Fixture {
            admin: MpdAdmin { config_path, config: RwLock::new(Config::default()), catalog },
            _dir: dir,
        }
    }

    #[test]
    fn asset_expose_ui_js_et_ui_css_et_rien_dautre() {
        let f = fixture();
        let (mime, corps) = f.admin.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!corps.is_empty());
        assert_eq!(f.admin.asset("ui.css").unwrap().0, "text/css");
        // Un chemin inconnu n'est pas une erreur : c'est un 404 côté cœur.
        assert!(f.admin.asset("../../../etc/passwd").is_none());
        assert!(f.admin.asset("index.html").is_none());
    }

    #[test]
    fn catalog_expose_les_cles_du_composant() {
        let f = fixture();
        let v = f.admin.catalog();
        assert!(v["btn_save"].is_string(), "le catalogue doit porter les cles du plugin");
    }

    /// Pack français livré dans le dépôt.
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/mpd/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::MPD_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }

    #[tokio::test]
    async fn get_data_renvoie_les_reglages_courants() {
        let f = fixture();
        let v = f.admin.get_data().await;
        assert_eq!(v["listen"], "0.0.0.0");
        assert_eq!(v["port"], 6600);
    }

    #[tokio::test]
    async fn set_data_valide_persiste_et_remplace_la_copie_en_memoire() {
        let mut f = fixture();
        let op = serde_json::json!({ "listen": "192.168.1.10", "port": 6601 });
        assert!(f.admin.set_data(op).await.is_ok());
        assert_eq!(f.admin.get_data().await, serde_json::json!({ "listen": "192.168.1.10", "port": 6601 }));
        assert_eq!(Config::charger(&f.admin.config_path).port, 6601);
    }

    #[tokio::test]
    async fn un_port_invalide_renvoie_une_phrase_de_catalogue_pas_la_cle_brute() {
        // La régression que ce test bloque : `admin.rs` propage directement
        // `Err("port_zero".into())` sans le résoudre via le catalogue. La page
        // Vue affiche `error` tel quel (pas de retraduction côté client), donc
        // l'utilisateur lirait littéralement "port_zero" à l'écran plutôt que
        // la phrase. Vérifié en faisant échouer ce test délibérément
        // (résolution retirée) avant de l'écrire pour de bon : voir le rapport
        // de tâche pour la preuve.
        let mut f = fixture();
        let op = serde_json::json!({ "listen": "0.0.0.0", "port": 0 });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "The port must be between 1 and 65535.");
        assert_ne!(err, "port_zero");
        // Rien n'a été écrit, et la copie en mémoire n'a pas bougé.
        assert!(!f.admin.config_path.exists());
        assert_eq!(f.admin.get_data().await["port"], 6600);
    }

    #[tokio::test]
    async fn une_adresse_vide_renvoie_une_phrase_de_catalogue() {
        let mut f = fixture();
        let op = serde_json::json!({ "listen": "", "port": 6600 });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "The listen address cannot be empty.");
    }

    #[tokio::test]
    async fn un_echec_decriture_renvoie_une_phrase_de_catalogue_pas_le_detail_io() {
        // Même régression qu'en generic-input et en radio :
        // `enregistrer(...).map_err(|e| e.to_string())` mettrait le détail I/O
        // brut dans le corps de la réponse. `config_path` vise ici un fichier
        // ordinaire comme s'il s'agissait d'un répertoire parent, pour faire
        // échouer l'écriture du temporaire sans toucher au disque réel.
        let mut f = fixture();
        let obstacle = f.admin.config_path.parent().unwrap().join("obstacle");
        std::fs::write(&obstacle, b"pas un repertoire").unwrap();
        f.admin.config_path = obstacle.join("mpd.toml");
        let op = serde_json::json!({ "listen": "0.0.0.0", "port": 6600 });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "Could not save the settings.");
    }

    #[tokio::test]
    async fn une_requete_mal_formee_renvoie_une_erreur_traduite() {
        // `listen` et `port` portent tous deux un `#[serde(default = ...)]` :
        // un objet incomplet désérialise avec succès (défauts), donc seul un
        // type de champ incompatible (ici `port` en chaîne, pas en nombre)
        // fait échouer `serde_json::from_value`.
        let mut f = fixture();
        let err =
            f.admin.set_data(serde_json::json!({ "listen": "0.0.0.0", "port": "beaucoup" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "message inattendu: {err}");
    }
}
