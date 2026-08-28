use crate::config::Config;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Corps de `SetData`, distinct de `Config` : les deux champs y sont
/// **obligatoires**, sans `#[serde(default = ...)]`. Ces défauts sont justes
/// pour `Config::charger`, qui complète un *fichier* partiel — mais faux pour
/// une écriture, où un `PUT {"port": 6601}` sans `listen` doit être refusé,
/// pas silencieusement compris comme « remettre `listen` à `0.0.0.0` ».
/// Même séparation lecture/écriture que `generic-input::admin::Op::Save` et
/// `radio::admin::Op::Save`, qui portent chacun un type dédié à la requête,
/// distinct du type de configuration chargé depuis le disque.
#[derive(Debug, Deserialize)]
struct EcritureConfig {
    listen: String,
    port: u16,
}

pub struct MpdAdmin {
    pub config_path: PathBuf,
    /// Copie en mémoire du dernier enregistrement réussi : `get_data` la rend
    /// sans relire le disque à chaque requête.
    pub config: RwLock<Config>,
    pub catalog: Arc<RwLock<Catalog>>,
    /// Par où la nouvelle configuration atteint la moitié réseau, qui se relie
    /// alors sans redémarrage (voir `session::ecouter`).
    ///
    /// **Envoyée après l'écriture disque et jamais avant** : ce qui est servi
    /// doit être ce qui est enregistré, et une reliaison sur une configuration
    /// que le fichier ne porte pas encore serait perdue au premier vrai
    /// redémarrage — l'appareil écouterait alors ailleurs qu'à l'endroit qu'il
    /// annonce.
    ///
    /// `Option` pour les tests, qui éprouvent la validation et l'écriture sans
    /// monter de socket. Un `None` ne fait rien, silencieusement : c'est
    /// exactement l'ancien comportement.
    pub rebind_tx: Option<tokio::sync::watch::Sender<Config>>,
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
        // `EcritureConfig`, pas `Config` : voir le commentaire sur le type,
        // un champ manquant doit refuser la requête (`bad_request`), pas se
        // faire compléter par un défaut de *chargement*.
        let ecriture: EcritureConfig = serde_json::from_value(data).map_err(|e| {
            self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e.to_string())
        })?;
        let config = Config { listen: ecriture.listen, port: ecriture.port };
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
        *self.config.write().unwrap() = config.clone();
        // La moitié réseau se relie toute seule. `send` échoue seulement si
        // plus personne n'écoute — le greffon s'arrête — et il n'y a alors rien
        // à relier ni rien à signaler à l'utilisateur : le fichier est écrit,
        // ce qu'il demandait est fait.
        if let Some(tx) = &self.rebind_tx {
            let _ = tx.send(config);
        }
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

    /// Un admin câblé à un canal de reliaison, pour les tests qui veulent voir
    /// ce que la moitié réseau reçoit.
    fn fixture_avec_rebind() -> (Fixture, tokio::sync::watch::Receiver<Config>) {
        let mut f = fixture();
        let (tx, rx) = tokio::sync::watch::channel(Config::default());
        f.admin.rebind_tx = Some(tx);
        (f, rx)
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
            admin: MpdAdmin {
                config_path,
                config: RwLock::new(Config::default()),
                catalog,
                rebind_tx: None,
            },
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
    async fn un_enregistrement_reussi_fait_relier_la_moitie_reseau() {
        // La demande du propriétaire : ne plus avoir à relancer le greffon à la
        // main. La page écrit le fichier **puis** pousse la configuration ; la
        // moitié réseau lie le nouveau couple adresse/port (voir
        // `session::ecouter`).
        let (mut f, mut rx) = fixture_avec_rebind();
        assert!(f.admin.set_data(serde_json::json!({ "listen": "127.0.0.1", "port": 6612 })).await.is_ok());
        assert!(rx.has_changed().unwrap(), "la moitie reseau doit etre prevenue");
        let c = rx.borrow_and_update().clone();
        assert_eq!((c.listen.as_str(), c.port), ("127.0.0.1", 6612));
        // Et ce qui est poussé est bien ce qui est sur disque : servir une
        // adresse que le fichier ne porte pas la ferait disparaître au premier
        // vrai redémarrage.
        assert_eq!(Config::charger(&f.admin.config_path), c);
    }

    #[tokio::test]
    async fn un_enregistrement_refuse_ne_fait_rien_relier() {
        // Le pendant : un port invalide ne doit surtout pas faire lâcher le
        // socket qui sert.
        let (mut f, rx) = fixture_avec_rebind();
        assert!(f.admin.set_data(serde_json::json!({ "listen": "0.0.0.0", "port": 0 })).await.is_err());
        assert!(!rx.has_changed().unwrap(), "rien ne doit etre pousse sur un refus");
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
        // `EcritureConfig` (le type du corps de `SetData`, distinct de
        // `Config`) n'a aucun `#[serde(default = ...)]` : un type de champ
        // incompatible (ici `port` en chaîne, pas en nombre) fait échouer
        // `serde_json::from_value`, tout comme un champ manquant (voir le
        // test suivant).
        let mut f = fixture();
        let err =
            f.admin.set_data(serde_json::json!({ "listen": "0.0.0.0", "port": "beaucoup" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "message inattendu: {err}");
    }

    #[tokio::test]
    async fn un_champ_manquant_est_refuse_plutot_que_complete_par_un_defaut() {
        // La régression trouvée en revue : désérialiser directement en
        // `Config` (qui porte `#[serde(default = ...)]` sur ses deux champs,
        // corrects pour *charger un fichier* partiel) aurait laissé un `PUT
        // {"port": 6601}` sans `listen` réussir silencieusement, en
        // persistant `listen = "0.0.0.0"` comme si l'opérateur l'avait
        // demandé — une remise à zéro de l'adresse d'écoute déguisée en
        // enregistrement réussi. Avec `EcritureConfig` (champs obligatoires),
        // ce corps doit être refusé comme mal formé, et rien ne doit changer
        // sur disque ni en mémoire.
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "listen": "192.168.1.10", "port": 6601 })).await.is_ok());
        let err = f.admin.set_data(serde_json::json!({ "port": 6601 })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "message inattendu: {err}");
        assert_eq!(f.admin.get_data().await["listen"], "192.168.1.10", "listen n'a pas du bouger");
    }
}
