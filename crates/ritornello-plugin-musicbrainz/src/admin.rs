//! Dorsal d'admin du greffon musicbrainz : sert la page Vue livrée dans
//! `ui/dist`, et applique les trois actions qu'elle peut envoyer sur le
//! magasin de motifs ICY (voir `motifs.rs`) — le même magasin que la boucle
//! `metadata` lit et écrit pour sonder et découper les flux radio.
//!
//! Un greffon `metadata` ne reçoit **jamais** de trame `SetLocale` : cette
//! trame n'existe que pour `SourcePlugin` (voir `ritornello_proto`). Le
//! catalogue chargé ici est donc figé à la langue passée au lancement du
//! greffon — un changement de langue de l'appareil ne se voit sur cette page
//! qu'après un redémarrage du greffon. Même limite que la page du greffon MPD
//! (`ritornello-plugin-mpd::admin`).

use crate::motifs::{Magasin, Motif};
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as MagasinLock;

/// Ce que la page envoie. Structure dédiée et champs **obligatoires**, comme
/// `EcritureConfig` du greffon MPD : `Entree` (le type persisté) a des
/// `#[serde(default)]` pour relire un fichier d'une version antérieure, et
/// les réutiliser ici ferait qu'un champ oublié par la page passerait pour un
/// choix délibéré plutôt que pour une requête mal formée à refuser.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Ecriture {
    /// Poser un motif à la main sur une station. Toujours `Origine::Manuel` :
    /// c'est `pose_manuel` qui porte cette règle, la page n'a pas à l'envoyer.
    Pose { url: String, motif: MotifEcrit },
    Supprime { url: String },
    Vide,
}

/// Même forme externe que `motifs::Motif` (étiquetage externe : l'objet
/// `{"separe": {...}}` ou la chaîne nue `"ne_pas_decouper"`), mais un type à
/// part : celui-ci est le contrat d'écriture de la page, l'autre est le
/// format persisté du magasin. Les deux évoluent pour des raisons
/// différentes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MotifEcrit {
    Separe { separateur: String, artiste_en_premier: bool },
    NePasDecouper,
}

impl From<MotifEcrit> for Motif {
    fn from(m: MotifEcrit) -> Self {
        match m {
            MotifEcrit::Separe { separateur, artiste_en_premier } => {
                // `titre_au_milieu: false` : la page ne propose **pas** la forme
                // `Artiste - Titre - Album` dans son jeu fermé, et c'est
                // assumé — ce motif ne s'obtient que par un sondage, jamais à la
                // main. Un utilisateur qui voudrait le forcer supprime l'entrée
                // et laisse resonder.
                Motif::Separe { separateur, artiste_en_premier, titre_au_milieu: false }
            }
            MotifEcrit::NePasDecouper => Motif::NePasDecouper,
        }
    }
}

pub struct MusicBrainzAdmin {
    magasin: Arc<MagasinLock<Magasin>>,
    chemin_etat: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
}

impl MusicBrainzAdmin {
    pub fn new(magasin: Arc<MagasinLock<Magasin>>, chemin_etat: PathBuf, catalog: Arc<RwLock<Catalog>>) -> Self {
        Self { magasin, chemin_etat, catalog }
    }

    /// Résout une clé de catalogue en la phrase de la langue courante.
    fn traduit(&self, cle: &str) -> String {
        self.catalog.read().unwrap().get(cle).to_string()
    }
}

#[async_trait::async_trait]
impl AdminPlugin for MusicBrainzAdmin {
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
        let magasin = self.magasin.read().await;
        // Copie triée : le magasin conserve l'ordre d'insertion, seule la
        // page a besoin d'un ordre. `Option<String>` ordonne `None` avant
        // tout `Some` ; comparer `b` à `a` (plutôt que `a` à `b`) donne donc
        // le plus récent en premier et les stations jamais servies en
        // dernier, sans repasser par un tri à deux étages.
        let mut stations = magasin.entrees().to_vec();
        stations.sort_by(|a, b| b.dernier_usage.cmp(&a.dernier_usage));
        serde_json::json!({ "stations": stations })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        // `Ecriture`, pas un type calqué sur `Entree` : voir le commentaire
        // sur le type. Un champ manquant ou une action inconnue doit refuser
        // la requête, pas se faire compléter par un défaut de *chargement*.
        let ecriture: Ecriture =
            serde_json::from_value(data).map_err(|e| {
                // Par le catalogue, comme tous les autres refus de cette
                // méthode : une chaîne anglaise en dur n'est pas « une phrase
                // traduite », elle est juste une clé déguisée — un utilisateur
                // en français verrait de l'anglais. Le catalogue du greffon
                // n'avait pas cette clé (oubli de mon brief), elle a été ajoutée
                // sur le modèle exact de celle du greffon mpd.
                self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e.to_string())
            })?;

        let mut magasin = self.magasin.write().await;
        match ecriture {
            Ecriture::Pose { url, motif } => {
                // Validation **avant** toute écriture : un séparateur vide ou
                // sans espace de chaque côté couperait un nom composé en
                // deux (« Jean-Michel Jarre »). La page valide déjà pour un
                // retour immédiat, mais le dorsal reste l'autorité.
                if let MotifEcrit::Separe { separateur, .. } = &motif {
                    if separateur.is_empty() {
                        return Err(self.traduit("separator_empty"));
                    }
                    if !(separateur.starts_with(' ') && separateur.ends_with(' ')) {
                        return Err(self.traduit("separator_no_space"));
                    }
                }
                magasin.pose_manuel(&url, motif.into());
            }
            Ecriture::Supprime { url } => {
                // Un refus, pas un succès silencieux : la page afficherait
                // « fait » sur un geste sans effet.
                if magasin.entree(&url).is_none() {
                    return Err(self.traduit("unknown_station"));
                }
                magasin.supprime(&url);
            }
            Ecriture::Vide => {
                // Rien à effacer : un « tout vider » sur un magasin déjà vide
                // ne doit pas déclencher une écriture disque pour rien — et
                // donc pas risquer un refus `save_failed` sur un geste qui ne
                // changerait de toute façon rien.
                if magasin.est_vide() {
                    return Ok(());
                }
                magasin.vide_tout();
            }
        }
        // Aucune méthode du magasin n'écrit seule sur le disque : c'est
        // délibéré (voir `motifs.rs`), pour qu'une écriture ne se cache pas
        // derrière un nom qui n'en parle pas. C'est donc ici, et seulement
        // ici, que la mutation devient persistante.
        magasin.enregistre(&self.chemin_etat).map_err(|e| {
            tracing::warn!("could not save ICY patterns: {e}");
            self.traduit("save_failed")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motifs::Origine;

    struct Fixture {
        admin: MusicBrainzAdmin,
        chemin_etat: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let chemin_etat = dir.path().join("motifs.json");
        let catalog = Arc::new(RwLock::new(Catalog::load(
            "musicbrainz",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::MUSICBRAINZ_EN,
        )));
        Fixture {
            admin: MusicBrainzAdmin::new(
                Arc::new(MagasinLock::new(Magasin::default())),
                chemin_etat.clone(),
                catalog,
            ),
            chemin_etat,
            _dir: dir,
        }
    }

    #[test]
    fn les_actifs_inconnus_ne_sont_pas_servis() {
        let f = fixture();
        let (mime, corps) = f.admin.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!corps.is_empty());
        assert_eq!(f.admin.asset("ui.css").unwrap().0, "text/css");
        // Un chemin inconnu n'est pas une erreur : c'est un 404 côté cœur.
        // Servir autre chose ouvrirait une route de lecture arbitraire.
        assert!(f.admin.asset("../../../etc/passwd").is_none());
        assert!(f.admin.asset("index.html").is_none());
    }

    #[test]
    fn catalog_expose_les_cles_du_composant() {
        let f = fixture();
        let v = f.admin.catalog();
        assert!(v["title"].is_string(), "le catalogue doit porter les cles du plugin");
    }

    /// Pack français livré dans le dépôt.
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/musicbrainz/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::MUSICBRAINZ_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }

    #[tokio::test]
    async fn poser_un_motif_le_rend_manuel_et_le_persiste() {
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "pose",
            "url": "http://exemple/flux.mp3",
            "motif": { "separe": { "separateur": " - ", "artiste_en_premier": true } }
        });
        assert!(f.admin.set_data(op).await.is_ok());

        let data = f.admin.get_data().await;
        assert_eq!(data["stations"][0]["url"], "http://exemple/flux.mp3");
        assert_eq!(data["stations"][0]["origine"], "manuel");
        // `titre_au_milieu` figure dans la forme sérialisée : le champ est
        // additif (`serde(default)`), donc la page qui l'ignore continue de
        // lire, et celle qui n'envoie que les deux autres continue d'écrire.
        // Figé ici parce que c'est le contrat que la page consomme.
        assert_eq!(
            data["stations"][0]["motif"],
            serde_json::json!({
                "separe": {
                    "separateur": " - ",
                    "artiste_en_premier": true,
                    "titre_au_milieu": false
                }
            })
        );

        // Persisté sur disque, pas seulement en mémoire : `enregistre` a été
        // appelé après la mutation, comme le contrat l'exige.
        let relu = Magasin::charge(&f.chemin_etat);
        assert_eq!(relu.entree("http://exemple/flux.mp3").unwrap().origine, Origine::Manuel);
    }

    #[tokio::test]
    async fn un_separateur_sans_espaces_est_refuse_par_une_phrase_pas_une_cle() {
        // Le contrat du SDK, et la règle réelle : sans espaces autour,
        // `Jean-Michel Jarre` se ferait couper en deux.
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "pose",
            "url": "http://f",
            "motif": { "separe": { "separateur": "-", "artiste_en_premier": true } }
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("space"), "doit etre la phrase du catalogue : {err}");
        assert!(!err.contains("separator_no_space"), "jamais la cle brute : {err}");
        // Rien n'a du etre pose.
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn un_separateur_vide_est_refuse_par_une_phrase_distincte() {
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "pose",
            "url": "http://f",
            "motif": { "separe": { "separateur": "", "artiste_en_premier": true } }
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "the separator cannot be empty");
        assert_ne!(err, "separator_empty");
    }

    #[tokio::test]
    async fn supprimer_une_station_inconnue_est_un_refus_et_non_un_succes_muet() {
        let mut f = fixture();
        let op = serde_json::json!({ "action": "supprime", "url": "http://inconnue" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "no entry for that stream");
        assert_ne!(err, "unknown_station");
    }

    #[tokio::test]
    async fn supprimer_une_station_connue_reussit_et_persiste() {
        let mut f = fixture();
        f.admin
            .set_data(serde_json::json!({
                "action": "pose", "url": "http://f", "motif": "ne_pas_decouper"
            }))
            .await
            .unwrap();
        assert!(f.admin.set_data(serde_json::json!({ "action": "supprime", "url": "http://f" })).await.is_ok());
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
        assert!(Magasin::charge(&f.chemin_etat).entree("http://f").is_none());
    }

    #[tokio::test]
    async fn vider_efface_toutes_les_stations_et_persiste() {
        let mut f = fixture();
        f.admin
            .set_data(serde_json::json!({ "action": "pose", "url": "http://f", "motif": "ne_pas_decouper" }))
            .await
            .unwrap();
        assert!(f.admin.set_data(serde_json::json!({ "action": "vide" })).await.is_ok());
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
        assert!(Magasin::charge(&f.chemin_etat).est_vide());
    }

    #[tokio::test]
    async fn vider_un_magasin_deja_vide_necrit_pas_sur_le_disque() {
        // Le raccourci de `Ecriture::Vide` : rien a effacer, donc rien a
        // ecrire. Prouve par l'absence du fichier d'etat, que `enregistre`
        // aurait cree.
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "action": "vide" })).await.is_ok());
        assert!(!f.chemin_etat.exists(), "aucune ecriture ne devait avoir lieu sur un magasin deja vide");
    }

    #[tokio::test]
    async fn get_data_trie_par_dernier_usage_decroissant() {
        // Écrit directement le fichier d'état plutôt que d'espacer des appels
        // à `succes()` dans le temps réel (qui produiraient des horodatages à
        // la même seconde, donc un ordre non observable) : c'est exactement
        // la forme que `Magasin::enregistre` produit elle-même (voir le test
        // d'aller-retour de `motifs.rs`), pas un JSON inventé.
        let f = fixture();
        let brut = serde_json::json!({
            "stations": [
                { "url": "http://b", "motif": "ne_pas_decouper", "origine": "deviation_apprise",
                  "dernier_usage": "2024-01-01T00:00:00Z", "titres_decoupes": 5 },
                { "url": "http://a", "motif": { "separe": { "separateur": " - ", "artiste_en_premier": true } },
                  "origine": "standard_confirme", "dernier_usage": "2026-01-01T00:00:00Z", "titres_decoupes": 10 },
                { "url": "http://c", "motif": "ne_pas_decouper", "origine": "manuel",
                  "dernier_usage": null, "titres_decoupes": 0 }
            ]
        });
        std::fs::write(&f.chemin_etat, serde_json::to_string(&brut).unwrap()).unwrap();
        let magasin = Magasin::charge(&f.chemin_etat);
        let admin = MusicBrainzAdmin::new(
            Arc::new(MagasinLock::new(magasin)),
            f.chemin_etat.clone(),
            Arc::new(RwLock::new(Catalog::load(
                "musicbrainz",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::MUSICBRAINZ_EN,
            ))),
        );

        let data = admin.get_data().await;
        let urls: Vec<&str> = data["stations"].as_array().unwrap().iter().map(|s| s["url"].as_str().unwrap()).collect();
        assert_eq!(urls, vec!["http://a", "http://b", "http://c"], "plus recent d'abord, jamais sondee en dernier");
    }

    #[tokio::test]
    async fn une_ecriture_malformee_est_rejetee() {
        // Champ manquant, action inconnue : refus, pas un défaut appliqué.
        let mut f = fixture();
        let err = f.admin.set_data(serde_json::json!({ "action": "pose", "motif": "ne_pas_decouper" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "message inattendu: {err}");

        let err = f.admin.set_data(serde_json::json!({ "action": "efface_tout" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "message inattendu: {err}");

        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty(), "rien n'a du etre applique");
    }

    #[tokio::test]
    async fn un_echec_decriture_renvoie_une_phrase_de_catalogue_pas_le_detail_io() {
        // Même régression qu'en mpd, generic-input et radio :
        // `enregistrer(...).map_err(|e| e.to_string())` mettrait le détail
        // I/O brut dans le corps de la réponse. `chemin_etat` vise ici un
        // fichier ordinaire comme s'il s'agissait d'un répertoire parent,
        // pour faire échouer l'écriture du temporaire sans toucher au disque
        // réel.
        let mut f = fixture();
        let obstacle = f.chemin_etat.parent().unwrap().join("obstacle");
        std::fs::write(&obstacle, b"pas un repertoire").unwrap();
        f.admin.chemin_etat = obstacle.join("motifs.json");
        let op = serde_json::json!({ "action": "pose", "url": "http://f", "motif": "ne_pas_decouper" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "could not write the pattern file");
    }
}
