use crate::config::{Station, Stations};
use crate::directory::{Directory, DirectoryCountry, DirectoryStation};
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

/// Opérations portées par `SetData`, discriminées par le champ `op` (modèle du
/// plugin generic-input) : le protocol d'admin n'est **pas** étendu, tout
/// passe par `GetAsset` / `GetCatalog` / `GetData` / `SetData`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// Enregistre la table complète. Seule opération qui écrit sur disque.
    /// Les présélections sont attribuées par position côté navigateur, mais
    /// `Stations::validate` reste l'autorité.
    Save {
        #[serde(default)]
        stations: Vec<Station>,
    },
    /// Interroge l'annuaire en line et mémorise les résultats. Aucune station
    /// n'est persistée : l'utilisateur add ensuite celles qui l'intéressent
    /// puis clique « Enregistrer ». Le **pays**, lui, est retenu (voir
    /// `PluginState::country`) : c'est une préférence de l'appareil, et la
    /// retrouver au rechargement évite de la resaisir à chaque fois.
    Search {
        query: String,
        /// Code pays ISO ; chaîne clear = « tous pays ».
        #[serde(default)]
        country: String,
    },
    /// Récupère la liste des pays de l'annuaire et la mémorise.
    ///
    /// Opération distincte, et **à la demande** : elle coûte un appel réseau que
    /// rien ne justifie tant que l'utilisateur n'ouvre pas le sélecteur de pays.
    /// La mémoriser évite de la redemander à chaque ouverture.
    Countries,
}

pub struct RadioAdmin {
    pub stations_path: PathBuf,
    /// État persisté du plugin, partagé avec la moitié Source : c'est là que le
    /// pays choisi est retenu, à côté de la présélection.
    pub state_path: PathBuf,
    pub stations: Arc<AsyncRwLock<Stations>>,
    pub catalog: Arc<RwLock<Catalog>>,
    /// Accès à l'annuaire derrière un trait : les tests injectent des
    /// résultats sans jamais toucher au réseau.
    pub directory: Arc<dyn Directory>,
    /// Derniers résultats de recherche, exposés par `get_data` (champ
    /// `search`) ; liste clear tant qu'aucune recherche n'a été faite. Une
    /// recherche en échec les laisse intacts.
    pub search: RwLock<Vec<DirectoryStation>>,
    /// Liste des pays, une fois récupérée. Vide tant que l'utilisateur n'a pas
    /// ouvert le sélecteur : aucun appel réseau n'est fait sans cela.
    pub countries: RwLock<Vec<DirectoryCountry>>,
    /// Annonce le nouveau `Stations::preset_count()` à la moitié Source après
    /// un enregistrement réussi : c'est ce qui permet à la grille de la
    /// télécommande web de se mettre à jour sans attendre qu'une présélection
    /// soit jouée. Voir `RadioSource::poll_notification` côté Source.
    pub preset_count_tx: tokio::sync::watch::Sender<u8>,
}

#[async_trait::async_trait]
impl AdminPlugin for RadioAdmin {
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

    fn catalog(&self) -> serde_json::Value {
        let cat = self.catalog.read().unwrap();
        serde_json::json!(cat.entries())
    }

    async fn get_data(&self) -> serde_json::Value {
        let stations = self.stations.read().await.stations.clone();
        // Gardes `std::sync` prises après le seul `.await` de la fonction :
        // aucune garde ne traverse un point d'attente.
        let search = self.search.read().unwrap().clone();
        let countries = self.countries.read().unwrap().clone();
        // Le pays est relu du disque à chaque appel plutôt que gardé en mémoire :
        // la moitié Source écrit dans le même fichier, et une copie en mémoire
        // divergerait sans qu'on s'en aperçoive.
        let country = crate::state::load(&self.state_path).country;
        serde_json::json!({
            "stations": stations,
            "search": search,
            "countries": countries,
            "country": country,
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
            Op::Save { stations } => {
                let stations = Stations { stations };
                stations
                    .validate()
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                stations.save(&self.stations_path).map_err(|e| {
                    // Le détail technique (path, cause I/O) reste dans le
                    // journal : un `/var/lib` en playback seule doit rester
                    // diagnosticable, mais pas au prix de serve ce diagnostic
                    // comme texte d'interface.
                    tracing::warn!("failed to save stations: {e}");
                    self.catalog.read().unwrap().get("save_failed").to_string()
                })?;
                let compte = stations.preset_count();
                *self.stations.write().await = stations;
                // Annonce spontanée à la moitié Source, sur **chaque**
                // enregistrement réussi — même si le compte ne change pas :
                // comparer avant d'send_frame coûterait une garde pour un gain
                // nul, la fusion côté cœur (`Core::handle_source_update`) et
                // sa diffusion (`publish_state`) dédupliquant déjà toute trame
                // identique à la précédente. Aucun récepteur en mode dégradé
                // (pas d'admin) : `send` renvoie alors une erreur sans
                // conséquence, ignorée.
                let _ = self.preset_count_tx.send(compte);
                Ok(())
            }
            Op::Search { query, country } => {
                let pays = country.trim().to_string();
                let pays = if pays.is_empty() { None } else { Some(pays) };
                // L'appel réseau est attendu ici (pas de sondage, contrairement
                // à l'apprentissage du plugin input) ; il ne concerne que la
                // moitié Admin, la playback audio n'est jamais bloquée. C'est
                // aussi le point qui doit rendre la main avant les 5 s
                // qu'`AdminClient::request` accorde au cœur : le budget de
                // `search_with_fallback` (4 s) est là pour ça.
                let resultats = self
                    .directory
                    .search(query.trim(), pays.as_deref())
                    .await
                    .map_err(|detail| {
                        self.catalog
                            .read()
                            .unwrap()
                            .get("search_error")
                            .replace("{detail}", &detail)
                    })?;
                *self.search.write().unwrap() = resultats;
                // Le pays n'est retenu qu'après une recherche **réussie** : une
                // recherche en échec ne dit rien de l'intention de
                // l'utilisateur, et mémoriser un pays qui vient d'échouer le
                // ferait réessayer au rechargement.
                let choisi = pays.unwrap_or_default();
                if let Err(e) = crate::state::update(&self.state_path, |s| s.country = choisi) {
                    // Sans conséquence sur la recherche qui vient d'aboutir :
                    // seule la mémoire du choix est perdue.
                    tracing::warn!("country not saved: {e}");
                }
                Ok(())
            }
            Op::Countries => {
                let pays = self.directory.countries().await.map_err(|detail| {
                    self.catalog
                        .read()
                        .unwrap()
                        .get("search_error")
                        .replace("{detail}", &detail)
                })?;
                *self.countries.write().unwrap() = pays;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Station;
    use crate::directory::parse_search_results;

    const FIXTURE: &str = include_str!("../tests/fixtures/radio-browser-search.json");

    /// Annuaire de test : renvoie un résultat figé (ou une erreur) et
    /// enregistre les arguments reçus. Aucune socket, aucun réseau.
    struct StubDirectory {
        resultat: Result<Vec<DirectoryStation>, String>,
        pays: Result<Vec<DirectoryCountry>, String>,
        vus: std::sync::Mutex<Vec<(String, Option<String>)>>,
        appels_pays: std::sync::atomic::AtomicUsize,
    }

    impl StubDirectory {
        fn ok(stations: Vec<DirectoryStation>) -> Arc<Self> {
            Arc::new(StubDirectory {
                resultat: Ok(stations),
                pays: Ok(vec![
                    DirectoryCountry { code: "FR".into(), stations: 2746 },
                    DirectoryCountry { code: "BE".into(), stations: 300 },
                ]),
                vus: std::sync::Mutex::new(Vec::new()),
                appels_pays: std::sync::atomic::AtomicUsize::new(0),
            })
        }
        fn err(msg: &str) -> Arc<Self> {
            Arc::new(StubDirectory {
                resultat: Err(msg.to_string()),
                pays: Err(msg.to_string()),
                vus: std::sync::Mutex::new(Vec::new()),
                appels_pays: std::sync::atomic::AtomicUsize::new(0),
            })
        }
    }

    #[async_trait::async_trait]
    impl Directory for StubDirectory {
        async fn search(
            &self,
            query: &str,
            country: Option<&str>,
        ) -> Result<Vec<DirectoryStation>, String> {
            self.vus
                .lock()
                .unwrap()
                .push((query.to_string(), country.map(str::to_string)));
            self.resultat.clone()
        }

        async fn countries(&self) -> Result<Vec<DirectoryCountry>, String> {
            self.appels_pays.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.pays.clone()
        }
    }

    fn admin_avec(dir: &std::path::Path, directory: Arc<dyn Directory>) -> RadioAdmin {
        let path = dir.join("stations.toml");
        let stations = Stations {
            stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }],
        };
        stations.save(&path).unwrap();
        RadioAdmin {
            stations_path: path,
            state_path: dir.join("plugin-radio.json"),
            stations: Arc::new(AsyncRwLock::new(stations)),
            catalog: Arc::new(RwLock::new(Catalog::load(
                "radio",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::RADIO_EN,
            ))),
            directory,
            search: RwLock::new(Vec::new()),
            countries: RwLock::new(Vec::new()),
            // Sans observateur : les tests qui ne s'intéressent pas au compte
            // de présélections n'ont rien à câbler. Voir `admin_avec_canal`
            // pour ceux qui l'observent.
            preset_count_tx: tokio::sync::watch::channel(0).0,
        }
    }

    fn admin(dir: &std::path::Path) -> RadioAdmin {
        admin_avec(dir, StubDirectory::ok(Vec::new()))
    }

    /// Comme `admin_avec`, mais expose aussi le récepteur du canal de compte
    /// de présélections, pour les tests qui vérifient ce qui y est publié.
    fn admin_avec_canal(
        dir: &std::path::Path,
        directory: Arc<dyn Directory>,
    ) -> (RadioAdmin, tokio::sync::watch::Receiver<u8>) {
        let mut a = admin_avec(dir, directory);
        let (tx, rx) = tokio::sync::watch::channel(0);
        a.preset_count_tx = tx;
        (a, rx)
    }

    #[test]
    fn asset_expose_ui_js_et_ui_css_et_rien_dautre() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let (mime, corps) = a.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!corps.is_empty());
        assert_eq!(a.asset("ui.css").unwrap().0, "text/css");
        // Un path inconnu n'est pas une erreur : c'est un 404 cote coeur.
        assert!(a.asset("../../../etc/passwd").is_none());
        assert!(a.asset("index.html").is_none());
    }

    #[test]
    fn catalog_expose_les_cles_du_composant() {
        let dir = tempfile::tempdir().unwrap();
        let v = admin(dir.path()).catalog();
        assert!(v["btn_save"].is_string(), "le sources_catalog doit porter les cles du plugin");
    }

    #[tokio::test]
    async fn get_data_renvoie_les_stations_et_une_recherche_vide() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let v = a.get_data().await;
        assert_eq!(v["stations"][0]["name"], "FIP");
        assert_eq!(v["search"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn save_valide_persiste_et_met_a_jour() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let nouveau = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "Inter", "url": "http://inter", "preset": 1 }]
        });
        assert!(a.set_data(nouveau).await.is_ok());
        assert_eq!(a.stations.read().await.stations[0].name, "Inter");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "Inter");
    }

    #[tokio::test]
    async fn un_echec_decriture_renvoie_une_phrase_de_catalogue_pas_le_detail_io() {
        // `stations_path` vise un fichier ordinaire comme s'il s'agissait
        // d'un répertoire parent : `create_dir_all` échoue avec une erreur
        // d'E/S, sans jamais toucher au disque des stations réelles.
        // Régression visée : `Stations::save(...).map_err(|e| e.to_string())`
        // mettait cette erreur brute (chemins compris) dans le corps de la
        // réponse — le texte destiné au player doit rester une phrase de
        // sources_catalog, le détail technique allant au journal.
        let dir = tempfile::tempdir().unwrap();
        let obstacle = dir.path().join("obstacle");
        std::fs::write(&obstacle, b"pas un repertoire").unwrap();
        let mut a = admin(dir.path());
        a.stations_path = obstacle.join("stations.toml");
        let nouveau = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "Inter", "url": "http://inter", "preset": 1 }]
        });
        let err = a.set_data(nouveau).await.unwrap_err();
        assert_eq!(err, "the save failed");
    }

    #[tokio::test]
    async fn un_enregistrement_reussi_publie_le_nouveau_compte() {
        // C'est ce qui permet a la grille de la telecommande web de se mettre
        // a jour des l'enregistrement, sans attendre qu'une preselection soit
        // jouee — voir le defaut constate a l'usage.
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut rx) = admin_avec_canal(dir.path(), StubDirectory::ok(Vec::new()));
        let nouveau = serde_json::json!({
            "op": "save",
            "stations": [
                { "name": "A", "url": "http://a", "preset": 1 },
                { "name": "B", "url": "http://b", "preset": 2 }
            ]
        });
        assert!(a.set_data(nouveau).await.is_ok());
        assert!(rx.has_changed().unwrap(), "le nouveau compte doit etre publie");
        assert_eq!(*rx.borrow_and_update(), 2);
    }

    #[tokio::test]
    async fn un_enregistrement_refuse_ne_publie_rien() {
        // Un payload qui ne passe pas `Stations::validate` ne doit rien
        // annoncer : rien n'a changé cote table.
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut rx) = admin_avec_canal(dir.path(), StubDirectory::ok(Vec::new()));
        rx.borrow_and_update();
        let mauvais = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "X", "url": "http://x", "preset": 200 }]
        });
        assert!(a.set_data(mauvais).await.is_err());
        assert!(!rx.has_changed().unwrap(), "un enregistrement refuse ne doit rien publier");
    }

    #[tokio::test]
    async fn save_numerote_de_1_a_n_par_position() {
        // Charge utile telle que la produit l'IHM : `preset` = position.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let nouveau = serde_json::json!({
            "op": "save",
            "stations": [
                { "name": "A", "url": "http://a", "preset": 1 },
                { "name": "B", "url": "http://b", "preset": 2 },
                { "name": "C", "url": "http://c", "preset": 3 }
            ]
        });
        assert!(a.set_data(nouveau).await.is_ok());
        let s = Stations::load(&a.stations_path).unwrap();
        assert_eq!(s.by_preset(2).unwrap().name, "B");
        assert_eq!(s.by_preset(3).unwrap().name, "C");
    }

    #[tokio::test]
    async fn save_invalide_renvoie_erreur_et_ne_persiste_pas() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let mauvais = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "X", "url": "http://x", "preset": 200 }]
        });
        assert!(a.set_data(mauvais).await.is_err());
        // l'état partagé et le disque restent inchangés
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn save_dune_preselection_hors_bornes_est_refuse_cote_serveur() {
        // Filet serveur : `Stations::validate` reste l'autorité même pour un
        // payload qui ne passe pas par la page d'admin — la bounded est
        // désormais 1..=99 (avant : 1..=9, d'où l'ancien name de ce test).
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let stations = vec![serde_json::json!({ "name": "S100", "url": "http://x", "preset": 100 })];
        let err = a
            .set_data(serde_json::json!({ "op": "save", "stations": stations }))
            .await
            .unwrap_err();
        assert!(err.contains("100"), "message inattendu: {err}");
        assert!(!Stations::load(&a.stations_path).unwrap().stations.is_empty());
    }

    #[tokio::test]
    async fn search_memorise_les_resultats_et_get_data_les_expose() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(parse_search_results(FIXTURE).unwrap());
        let mut a = admin_avec(dir.path(), stub.clone());
        let op = serde_json::json!({ "op": "search", "query": "france", "country": "FR" });
        assert!(a.set_data(op).await.is_ok());

        let v = a.get_data().await;
        assert_eq!(v["search"].as_array().unwrap().len(), 4);
        assert_eq!(v["search"][0]["name"], "France Info");
        assert_eq!(v["search"][0]["url"], "http://direct.franceinfo.fr/live/franceinfo-midfi.mp3");
        assert_eq!(v["search"][0]["codec"], "MP3");
        assert_eq!(v["search"][0]["bitrate"], 128);
        assert_eq!(v["search"][0]["country"], "FR");
        // les stations configurées ne bougent pas
        assert_eq!(v["stations"][0]["name"], "FIP");
        // rien n'est persisté par une recherche
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
        assert_eq!(stub.vus.lock().unwrap()[0], ("france".to_string(), Some("FR".to_string())));
    }

    #[tokio::test]
    async fn search_sans_pays_ne_transmet_aucun_countrycode() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(Vec::new());
        let mut a = admin_avec(dir.path(), stub.clone());
        let op = serde_json::json!({ "op": "search", "query": "  jazz  ", "country": "" });
        assert!(a.set_data(op).await.is_ok());
        assert_eq!(stub.vus.lock().unwrap()[0], ("jazz".to_string(), None));
        assert_eq!(a.get_data().await["search"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn search_en_erreur_renvoie_un_message_traduit_et_laisse_letat_intact() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(parse_search_results(FIXTURE).unwrap());
        let mut a = admin_avec(dir.path(), stub);
        assert!(a
            .set_data(serde_json::json!({ "op": "search", "query": "france", "country": "FR" }))
            .await
            .is_ok());

        // l'annuaire tombe : les résultats précédents restent affichables
        a.directory = StubDirectory::err("timeout");
        let err = a
            .set_data(serde_json::json!({ "op": "search", "query": "france", "country": "FR" }))
            .await
            .unwrap_err();
        assert_eq!(err, "Directory search failed: timeout");
        assert_eq!(a.get_data().await["search"].as_array().unwrap().len(), 4);
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn les_pays_ne_sont_recuperes_qua_la_demande_et_memorises() {
        // L'appel reseau ne doit pas partir au chargement de la page : il ne se
        // justifie que quand l'utilisateur ouvre le selecteur de pays.
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(Vec::new());
        let mut a = admin_avec(dir.path(), stub.clone());
        assert_eq!(a.get_data().await["countries"], serde_json::json!([]));
        assert_eq!(stub.appels_pays.load(std::sync::atomic::Ordering::SeqCst), 0);

        assert!(a.set_data(serde_json::json!({ "op": "countries" })).await.is_ok());
        let v = a.get_data().await;
        assert_eq!(v["countries"][0]["code"], "FR");
        assert_eq!(v["countries"][0]["stations"], 2746);
        assert_eq!(stub.appels_pays.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn les_pays_en_erreur_renvoient_un_message_traduit() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_avec(dir.path(), StubDirectory::err("timeout"));
        let err = a.set_data(serde_json::json!({ "op": "countries" })).await.unwrap_err();
        assert_eq!(err, "Directory search failed: timeout");
        assert_eq!(a.get_data().await["countries"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn une_recherche_reussie_memorise_le_pays_et_get_data_le_rend() {
        // C'est ce qui evite de resaisir le pays a chaque ouverture de la page.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_avec(dir.path(), StubDirectory::ok(Vec::new()));
        assert_eq!(a.get_data().await["country"], "");
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "BE" });
        assert!(a.set_data(op).await.is_ok());
        assert_eq!(a.get_data().await["country"], "BE");
        // « tous pays » est un choix comme un autre, et doit se retenir aussi.
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "" });
        assert!(a.set_data(op).await.is_ok());
        assert_eq!(a.get_data().await["country"], "");
    }

    #[tokio::test]
    async fn memoriser_le_pays_ne_perd_pas_la_preselection() {
        // Les deux halves du plugin ecrivent dans le meme fichier d'state.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_avec(dir.path(), StubDirectory::ok(Vec::new()));
        crate::state::update(&a.state_path, |s| s.preset = 6).unwrap();
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "DE" });
        assert!(a.set_data(op).await.is_ok());
        let state = crate::state::load(&a.state_path);
        assert_eq!(state.country, "DE");
        assert_eq!(state.preset, 6, "la preselection ne doit pas etre ecrasee");
    }

    #[tokio::test]
    async fn une_recherche_en_echec_ne_memorise_pas_le_pays() {
        // Retenir un pays qui vient d'echouer ferait reessayer au rechargement
        // ce dont on sait deja qu'il ne marche pas.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_avec(dir.path(), StubDirectory::err("timeout"));
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "IT" });
        assert!(a.set_data(op).await.is_err());
        assert_eq!(crate::state::load(&a.state_path).country, "");
    }

    #[tokio::test]
    async fn op_inconnue_ou_absente_renvoie_une_erreur() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let err = a.set_data(serde_json::json!({ "op": "detruire" })).await.unwrap_err();
        assert!(err.starts_with("invalid request:"), "message inattendu: {err}");
        let err2 = a
            .set_data(serde_json::json!({ "stations": [] }))
            .await
            .unwrap_err();
        assert!(err2.starts_with("invalid request:"), "message inattendu: {err2}");
    }

    /// Pack français livré dans le dépôt.
    fn fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/radio/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::RADIO_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }
}
