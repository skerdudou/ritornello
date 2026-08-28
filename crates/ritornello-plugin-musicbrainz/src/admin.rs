//! Dorsal d'admin du greffon musicbrainz : sert la page Vue livrée dans
//! `ui/dist`, et apply les trois actions qu'elle peut send_frame sur le
//! store de patterns ICY (voir `patterns.rs`) — le même store que la boucle
//! `metadata` read et écrit pour sonder et découper les stream radio.
//!
//! Un greffon `metadata` ne reçoit **jamais** de trame `SetLocale` : cette
//! trame n'existe que pour `SourcePlugin` (voir `ritornello_proto`). Le
//! sources_catalog chargé ici est donc figé à la langue passée au lancement du
//! greffon — un changement de langue de l'appareil ne se voit sur cette page
//! qu'après un redémarrage du greffon. Même limite que la page du greffon MPD
//! (`ritornello-plugin-mpd::admin`).

use crate::patterns::{Store, Pattern};
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as MagasinLock;

/// Ce que la page envoie. Structure dédiée et champs **obligatoires**, comme
/// `EcritureConfig` du greffon MPD : `Entry` (le type persisté) a des
/// `#[serde(default)]` pour relire un fichier d'une version antérieure, et
/// les réutiliser ici ferait qu'un champ oublié par la page passerait pour un
/// choix délibéré plutôt que pour une requête mal formée à refuser.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Write {
    /// Poser un pattern à la main sur une station. Toujours `Origin::Manual` :
    /// c'est `set_manual` qui porte cette règle, la page n'a pas à l'send_frame.
    Set { url: String, pattern: WrittenPattern },
    Remove { url: String },
    Clear,
}

/// Même forme externe que `patterns::Pattern` (étiquetage externe : l'objet
/// `{"separe": {...}}` ou la chaîne nue `"ne_pas_decouper"`), mais un type à
/// part : celui-ci est le contrat d'écriture de la page, l'autre est le
/// format persisté du store. Les deux évoluent pour des raisons
/// différentes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WrittenPattern {
    Split {
        separator: String,
        artist_first: bool,
        /// `serde(default)` alors que les autres champs sont obligatoires, et
        /// c'est délibéré : une page qui ne connaît pas cette forme continue
        /// d'écrire, et l'absence vaut « non » — la forme courante.
        #[serde(default)]
        title_in_middle: bool,
    },
    DoNotSplit,
}

impl From<WrittenPattern> for Pattern {
    fn from(m: WrittenPattern) -> Self {
        match m {
            WrittenPattern::Split { separator, artist_first, title_in_middle } => {
                // `title_in_middle` est **reporté** et non remis à faux.
                //
                // La page ne l'*offre* pas dans son jeu fermé — cette forme ne
                // s'obtient que par un sondage — mais elle le **rejoue** quand
                // le formulaire a été ouvert sur une entrée qui la porte. Le
                // reposer à faux ici faisait qu'« Enregistrer » sans rien
                // changer dégradait le pattern : l'album se recollait au title dès
                // le track suivant, et comme l'entrée devenait `Manual`, plus
                // rien ne pouvait la réparer. Le geste destructeur n'était pas
                // « poser cette forme », c'était « enregistrer sans
                // modification ».
                Pattern::Split { separator, artist_first, title_in_middle }
            }
            WrittenPattern::DoNotSplit => Pattern::DoNotSplit,
        }
    }
}

pub struct MusicBrainzAdmin {
    store: Arc<MagasinLock<Store>>,
    state_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
}

impl MusicBrainzAdmin {
    pub fn new(store: Arc<MagasinLock<Store>>, state_path: PathBuf, catalog: Arc<RwLock<Catalog>>) -> Self {
        Self { store, state_path, catalog }
    }

    /// Résout une clé de sources_catalog en la phrase de la langue courante.
    fn translate(&self, key: &str) -> String {
        self.catalog.read().unwrap().get(key).to_string()
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
        let store = self.store.read().await;
        // Copie triée : le store conserve l'order d'insertion, seule la
        // page a besoin d'un order. `Option<String>` ordonne `None` avant
        // tout `Some` ; comparer `b` à `a` (plutôt que `a` à `b`) donne donc
        // le plus récent en premier et les stations jamais servies en
        // dernier, sans repasser par un tri à deux étages.
        let mut stations = store.entries().to_vec();
        stations.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        serde_json::json!({ "stations": stations })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        // `Write`, pas un type calqué sur `Entry` : voir le commentaire
        // sur le type. Un champ manquant ou une action inconnue doit refuser
        // la requête, pas se faire compléter par un défaut de *chargement*.
        let ecriture: Write =
            serde_json::from_value(data).map_err(|e| {
                // Par le sources_catalog, comme tous les autres refus de cette
                // méthode : une chaîne anglaise en dur n'est pas « une phrase
                // traduite », elle est juste une clé déguisée — un utilisateur
                // en français verrait de l'anglais. Le sources_catalog du greffon
                // n'avait pas cette clé (oubli de mon brief), elle a été ajoutée
                // sur le modèle exact de celle du greffon mpd.
                self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e.to_string())
            })?;

        let mut store = self.store.write().await;
        match ecriture {
            Write::Set { url, pattern } => {
                // Validation **avant** toute écriture : un séparateur clear ou
                // sans espace de chaque côté couperait un name composé en
                // deux (« Jean-Michel Jarre »). La page validated déjà pour un
                // retour immédiat, mais le dorsal reste l'autorité.
                if let WrittenPattern::Split { separator, .. } = &pattern {
                    // `trim()` et non `is_empty()` : un séparateur qui n'est
                    // que des espaces passait les deux contrôles — `" "`
                    // commence *et* finit par une espace, la même — et aurait
                    // découpé sur **chaque** espace de la chaîne annoncée.
                    // « Clear » est le bon mot pour lui : il ne porte rien.
                    if separator.trim().is_empty() {
                        return Err(self.translate("separator_empty"));
                    }
                    if !(separator.starts_with(' ') && separator.ends_with(' ')) {
                        return Err(self.translate("separator_no_space"));
                    }
                }
                store.set_manual(&url, pattern.into());
            }
            Write::Remove { url } => {
                // Un refus, pas un succès silencieux : la page afficherait
                // « fait » sur un geste sans effet.
                if store.entry(&url).is_none() {
                    return Err(self.translate("unknown_station"));
                }
                store.remove(&url);
            }
            Write::Clear => {
                // Rien à effacer : un « tout vider » sur un store déjà clear
                // ne doit pas déclencher une écriture disc pour rien — et
                // donc pas risquer un refus `save_failed` sur un geste qui ne
                // changerait de toute façon rien.
                if store.is_empty() {
                    return Ok(());
                }
                store.clear_all();
            }
        }
        // Aucune méthode du store n'écrit seule sur le disc : c'est
        // délibéré (voir `patterns.rs`), pour qu'une écriture ne se cache pas
        // derrière un name qui n'en parle pas. C'est donc ici, et seulement
        // ici, que la mutation devient persistante.
        store.save(&self.state_path).map_err(|e| {
            tracing::warn!("could not save ICY patterns: {e}");
            self.translate("save_failed")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::Origin;

    struct Fixture {
        admin: MusicBrainzAdmin,
        state_path: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("patterns.json");
        let catalog = Arc::new(RwLock::new(Catalog::load(
            "musicbrainz",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::MUSICBRAINZ_EN,
        )));
        Fixture {
            admin: MusicBrainzAdmin::new(
                Arc::new(MagasinLock::new(Store::default())),
                state_path.clone(),
                catalog,
            ),
            state_path,
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
        // Un path inconnu n'est pas une erreur : c'est un 404 côté cœur.
        // Servir autre chose ouvrirait une route de playback arbitraire.
        assert!(f.admin.asset("../../../etc/passwd").is_none());
        assert!(f.admin.asset("index.html").is_none());
    }

    #[test]
    fn catalog_expose_les_cles_du_composant() {
        let f = fixture();
        let v = f.admin.catalog();
        assert!(v["title"].is_string(), "le sources_catalog doit porter les cles du plugin");
    }

    /// Pack français livré dans le dépôt.
    fn fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/musicbrainz/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::MUSICBRAINZ_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
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
            "url": "http://exemple/stream.mp3",
            "pattern": { "separe": { "separator": " - ", "artist_first": true } }
        });
        assert!(f.admin.set_data(op).await.is_ok());

        let data = f.admin.get_data().await;
        assert_eq!(data["stations"][0]["url"], "http://exemple/stream.mp3");
        assert_eq!(data["stations"][0]["origin"], "manuel");
        // `title_in_middle` figure dans la forme sérialisée : le champ est
        // additif (`serde(default)`), donc la page qui l'ignore continue de
        // read, et celle qui n'envoie que les deux autres continue d'écrire.
        // Figé ici parce que c'est le contrat que la page consomme.
        assert_eq!(
            data["stations"][0]["pattern"],
            serde_json::json!({
                "separe": {
                    "separator": " - ",
                    "artist_first": true,
                    "title_in_middle": false
                }
            })
        );

        // Persisté sur disc, pas seulement en mémoire : `save` a été
        // appelé après la mutation, comme le contrat l'exige.
        let relu = Store::load(&f.state_path);
        assert_eq!(relu.entry("http://exemple/stream.mp3").unwrap().origin, Origin::Manual);
    }

    /// Un séparateur qui n'est **que** des espaces est refusé comme clear.
    ///
    /// `" "` passait les deux contrôles d'origin — il commence et finit par une
    /// espace, la même — et aurait découpé sur chaque espace de la chaîne
    /// annoncée : « Miles Davis - So What » devenait artist « Miles ». Constat
    /// de la relecture croisée.
    #[tokio::test]
    async fn un_separateur_qui_nest_que_des_espaces_est_refuse() {
        let mut f = fixture();
        for sep in [" ", "  ", "\t"] {
            let op = serde_json::json!({
                "action": "pose",
                "url": "http://exemple/stream.mp3",
                "pattern": { "separe": { "separator": sep, "artist_first": true } }
            });
            let err = f.admin.set_data(op).await.expect_err("un separator clear doit etre refuse");
            assert!(!err.contains("separator_"), "jamais la key brute : {err}");
        }
    }

    #[tokio::test]
    async fn un_separateur_sans_espaces_est_refuse_par_une_phrase_pas_une_cle() {
        // Le contrat du SDK, et la règle réelle : sans espaces autour,
        // `Jean-Michel Jarre` se ferait couper en deux.
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "pose",
            "url": "http://f",
            "pattern": { "separe": { "separator": "-", "artist_first": true } }
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("space"), "doit etre la phrase du sources_catalog : {err}");
        assert!(!err.contains("separator_no_space"), "jamais la key brute : {err}");
        // Rien n'a du etre pose.
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn un_separateur_vide_est_refuse_par_une_phrase_distincte() {
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "pose",
            "url": "http://f",
            "pattern": { "separe": { "separator": "", "artist_first": true } }
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "the separator cannot be empty");
        assert_ne!(err, "separator_empty");
    }

    #[tokio::test]
    async fn supprimer_une_station_inconnue_est_un_refus_et_non_un_succes_muet() {
        let mut f = fixture();
        let op = serde_json::json!({ "action": "remove", "url": "http://inconnue" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "no entry for that stream");
        assert_ne!(err, "unknown_station");
    }

    #[tokio::test]
    async fn supprimer_une_station_connue_reussit_et_persiste() {
        let mut f = fixture();
        f.admin
            .set_data(serde_json::json!({
                "action": "pose", "url": "http://f", "pattern": "ne_pas_decouper"
            }))
            .await
            .unwrap();
        assert!(f.admin.set_data(serde_json::json!({ "action": "remove", "url": "http://f" })).await.is_ok());
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
        assert!(Store::load(&f.state_path).entry("http://f").is_none());
    }

    #[tokio::test]
    async fn vider_efface_toutes_les_stations_et_persiste() {
        let mut f = fixture();
        f.admin
            .set_data(serde_json::json!({ "action": "pose", "url": "http://f", "pattern": "ne_pas_decouper" }))
            .await
            .unwrap();
        assert!(f.admin.set_data(serde_json::json!({ "action": "clear" })).await.is_ok());
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
        assert!(Store::load(&f.state_path).is_empty());
    }

    #[tokio::test]
    async fn vider_un_magasin_deja_vide_necrit_pas_sur_le_disque() {
        // Le raccourci de `Write::Clear` : rien a effacer, donc rien a
        // ecrire. Prouve par l'absence du fichier d'state, que `save`
        // aurait cree.
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "action": "clear" })).await.is_ok());
        assert!(!f.state_path.exists(), "aucune ecriture ne devait avoir lieu sur un store deja clear");
    }

    #[tokio::test]
    async fn get_data_trie_par_dernier_usage_decroissant() {
        // Écrit directement le fichier d'état plutôt que d'espacer des appels
        // à `record_success()` dans le temps réel (qui produiraient des horodatages à
        // la même seconde, donc un order non observable) : c'est exactement
        // la forme que `Store::save` produit elle-même (voir le test
        // d'aller-retour de `patterns.rs`), pas un JSON inventé.
        let f = fixture();
        let raw = serde_json::json!({
            "stations": [
                { "url": "http://b", "pattern": "ne_pas_decouper", "origin": "deviation_apprise",
                  "last_used": "2024-01-01T00:00:00Z", "split_titles": 5 },
                { "url": "http://a", "pattern": { "separe": { "separator": " - ", "artist_first": true } },
                  "origin": "standard_confirme", "last_used": "2026-01-01T00:00:00Z", "split_titles": 10 },
                { "url": "http://c", "pattern": "ne_pas_decouper", "origin": "manuel",
                  "last_used": null, "split_titles": 0 }
            ]
        });
        std::fs::write(&f.state_path, serde_json::to_string(&raw).unwrap()).unwrap();
        let store = Store::load(&f.state_path);
        let admin = MusicBrainzAdmin::new(
            Arc::new(MagasinLock::new(store)),
            f.state_path.clone(),
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
        let err = f.admin.set_data(serde_json::json!({ "action": "pose", "pattern": "ne_pas_decouper" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "message inattendu: {err}");

        let err = f.admin.set_data(serde_json::json!({ "action": "efface_tout" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "message inattendu: {err}");

        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty(), "rien n'a du etre apply");
    }

    #[tokio::test]
    async fn un_echec_decriture_renvoie_une_phrase_de_catalogue_pas_le_detail_io() {
        // Même régression qu'en mpd, generic-input et radio :
        // `enregistrer(...).map_err(|e| e.to_string())` mettrait le détail
        // I/O raw dans le corps de la réponse. `state_path` vise ici un
        // fichier ordinaire comme s'il s'agissait d'un répertoire parent,
        // pour faire échouer l'écriture du temporaire sans toucher au disc
        // réel.
        let mut f = fixture();
        let obstacle = f.state_path.parent().unwrap().join("obstacle");
        std::fs::write(&obstacle, b"pas un repertoire").unwrap();
        f.admin.state_path = obstacle.join("patterns.json");
        let op = serde_json::json!({ "action": "pose", "url": "http://f", "pattern": "ne_pas_decouper" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "could not write the pattern file");
    }
}
