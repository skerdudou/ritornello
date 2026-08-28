//! Les greffons vus de la page de statut : une ligne par (nom, genre), l'ordre du fichier plugins.toml, l'interrupteur actif/inactif, et ce qu'une deconnexion ou une reannonce change.

use super::*;

/// Une ligne de la page de statut : un couple (nom, genre).
///
/// `stalled` distingue **trois** états là où deux ne suffisaient pas :
///
/// - `connected: true` — annoncé et câblé ;
/// - `connected: false` seul — processus mort avant de s'annoncer ;
/// - `connected: false` + `stalled: true` — processus **vivant**, muet à
///   l'échéance du rendez-vous.
///
/// Un greffon figé n'est pas un greffon mort : il tourne, il n'a rien dit, et
/// il peut encore parler — le socket d'enregistrement reste ouvert pour lui et
/// le cœur le câblera à chaud. C'est cette différence que l'opérateur doit
/// voir.
///
/// Le champ est additif, avec l'idiome déjà employé pour `InputMessage.held` :
/// absent du JSON quand il est faux, donc aucune trame existante ne change et
/// une trame ancienne se relit sans erreur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    pub name: String,
    pub kind: String,
    pub connected: bool,
    pub admin: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stalled: bool,
    /// Lancé à l'instant, pas encore annoncé, et **dans le délai normal**.
    ///
    /// Exclusif avec `stalled`, et les deux disent la même chose du greffon —
    /// il n'a pas parlé. Ils ne diffèrent que par le temps écoulé, et cette
    /// différence est tout : « figé » accuse un greffon fautif, alors qu'un
    /// binaire qui met deux secondes à lier ses sockets sur une carte SD est
    /// parfaitement sain. Afficher « figé » pendant un démarrage normal était
    /// donc une accusation à tort, signalée à l'usage.
    ///
    /// La bascule vers `stalled` est faite par la boucle du cœur au bout de
    /// `DELAI_DEMARRAGE` (voir `main.rs`), et seulement si la ligne dit encore
    /// « démarrage » à cet instant.
    ///
    /// Additif comme les deux autres : absent du JSON quand il est faux.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starting: bool,
    /// Greffon éteint depuis l'IHM : aucun processus, aucun câblage, et le
    /// manifeste porte `enabled = false`. La ligne reste affichée — sans elle,
    /// on ne pourrait plus le rallumer.
    ///
    /// Additif comme `stalled` : absent du JSON quand il est faux, donc aucune
    /// trame existante ne change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Greffon joint dont la page d'admin ne répond pas au `Ping` : un
    /// `set_data` long tient son verrou (le plus souvent un partage réseau).
    /// Calculé au moment de `/api/status`, jamais stocké : c'est un état qui
    /// change à la seconde. Additif comme `stalled` et `disabled`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
}

impl PluginStatus {
    /// Ligne d'un genre annoncé, joint (`connected: true`) ou non.
    ///
    /// `stalled` n'a pas de sens ici : le greffon a parlé. Pas de `Default`
    /// dérivé sur cette structure pour la même raison — un statut sans nom ni
    /// genre ne veut rien dire.
    pub fn genre(name: &str, kind: &str, connected: bool, admin: bool) -> Self {
        Self {
            name: name.to_string(),
            kind: kind.to_string(),
            connected,
            admin,
            stalled: false,
            starting: false,
            disabled: false,
            busy: false,
        }
    }

    /// Ligne d'un greffon qui n'a annoncé **aucun** genre : jamais lancé, mort
    /// avant l'annonce, ou vivant et muet (`stalled`).
    ///
    /// Le genre est rapporté « unknown » plutôt qu'inventé : le manifeste ne le
    /// porte plus, c'est le binaire qui l'annonce.
    pub fn genre_inconnu(name: &str, stalled: bool) -> Self {
        Self {
            name: name.to_string(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
            stalled,
            starting: false,
            disabled: false,
            busy: false,
        }
    }

    /// Ligne d'un greffon qu'on vient de lancer : il n'a pas parlé, et c'est
    /// normal.
    ///
    /// Distincte de `genre_inconnu(name, true)`, qui accuse. Voir la doc du
    /// champ `starting`.
    pub fn demarrage(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
            stalled: false,
            starting: true,
            disabled: false,
            busy: false,
        }
    }

    /// Ligne d'un greffon éteint. Ni genre ni page d'admin : il n'a rien
    /// annoncé et n'annoncera rien tant qu'il ne sera pas rallumé.
    pub fn desactive(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
            stalled: false,
            starting: false,
            disabled: true,
            busy: false,
        }
    }
}

/// Ordre d'allumage ou d'extinction, de la couche HTTP vers la boucle du cœur.
///
/// L'accusé est un `oneshot` et non un simple envoi : la page attend une
/// réponse qui décrive un état déjà vrai, sinon elle se rafraîchirait sur un
/// état intermédiaire. `bool` et non `Result` : la seule chose que le cœur
/// puisse rater est le lancement d'un binaire, dont la cause exacte part au
/// journal — que l'IHM montre déjà — pendant que l'écran reçoit un message du
/// catalogue.
pub struct OrdreGreffon {
    pub nom: String,
    pub actif: bool,
    pub ack: tokio::sync::oneshot::Sender<bool>,
}

/// Ce que la couche HTTP doit connaître des greffons pour les basculer.
///
/// Un seul champ d'`AppState` plutôt que trois, pour la raison déjà retenue
/// pour `system` : chaque constructeur de test grossirait sinon de trois
/// lignes.
pub struct GreffonsControle {
    /// Chemin de `plugins.toml` : c'est là qu'est écrit le choix.
    pub manifeste: std::path::PathBuf,
    /// Noms déclarés, dans l'ordre du fichier. Autorité sur ce qui peut être
    /// basculé : un nom absent est refusé **avant** toute écriture.
    pub noms: Vec<String>,
    pub tx: mpsc::Sender<OrdreGreffon>,
}

#[derive(Deserialize)]
pub(super) struct PluginEnabledReq {
    enabled: bool,
}

/// Bascule un greffon, **persistance d'abord**.
///
/// L'ordre des trois étapes est le fond de l'affaire : un nom refusé
/// n'écrit rien, une écriture qui échoue ne tue aucun processus, et le cœur
/// n'est prévenu que d'un choix déjà sur le disque. Un greffon éteint dont
/// l'extinction n'aurait pas été écrite reviendrait au prochain démarrage —
/// un mensonge silencieux, pire qu'un refus franc.
pub(super) async fn plugin_enabled_put(
    State(state): State<AppState>,
    axum::extract::Path(nom): axum::extract::Path<String>,
    Json(req): Json<PluginEnabledReq>,
) -> Response {
    if !state.greffons.noms.iter().any(|n| n == &nom) {
        let msg = state.catalog.read().await.get("plugin_unknown").replace("{name}", &nom);
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    if let Err(e) = crate::plugins::set_enabled(&state.greffons.manifeste, &nom, req.enabled) {
        tracing::warn!("persisting the enabled flag of {nom}: {e:#}");
        let msg = state.catalog.read().await.get("plugin_persist_failed").replace("{name}", &nom);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let ordre = OrdreGreffon { nom: nom.clone(), actif: req.enabled, ack: ack_tx };
    if state.greffons.tx.send(ordre).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match ack_rx.await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        // Le cœur a refusé (binaire introuvable au rallumage) ou n'a pas
        // répondu. La cause exacte est au journal, que l'IHM montre déjà.
        _ => {
            let msg = state.catalog.read().await.get("plugin_action_failed").replace("{name}", &nom);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": msg })))
                .into_response()
        }
    }
}

/// Marque le plugin `name` comme déconnecté dans l'état de statut : un plugin
/// dont le processus s'est terminé n'est plus joignable (supervision, page de
/// statut vivante). No-op si le nom est inconnu.
/// Les drapeaux `stalled` **et** `starting` sont retirés au passage, pour la
/// même raison : tous deux décrivent un processus *vivant* — « figé » veut dire
/// vivant et muet, « démarrage » veut dire vivant et pas encore annoncé. Un
/// processus dont on vient de voir la sortie n'est plus vivant, et laisser l'un
/// ou l'autre raconterait un état qui n'existe pas.
///
/// **`admin` tombe aussi**, et c'est ce qui retire l'entrée du menu du haut.
/// L'IHM construit ce menu par `plugins.filter(p => p.admin)` **sans regarder
/// `connected`** : une ligne restée à `admin: true` continuait donc d'offrir la
/// page d'un greffon mort, et le clic rendait une erreur au lieu de rien. C'est
/// le même symptôme que pour un greffon éteint, réglé de son côté parce que
/// `desactive` pose le drapeau à faux par construction. Corollaire utile : le
/// sondage `Ping` de `/api/status` ne filtre que sur `admin`, donc il cesse du
/// même coup d'interroger un dorsal mort à chaque rafraîchissement.
///
/// `starting` en particulier avait une conséquence visible : `main::a_retrograder`
/// ne consulte que ce drapeau, si bien qu'un greffon mort **pendant** ses dix
/// secondes de grâce gardait sa ligne « démarrage » jusqu'à l'échéance, puis se
/// faisait rétrograder en « figé » — c'est-à-dire annoncer vivant mais muet un
/// processus dont la sortie avait été moissonnée dix secondes plus tôt.
pub fn mark_plugin_disconnected(state: &mut StatusState, name: &str) {
    for p in &mut state.plugins {
        if p.name == name {
            p.connected = false;
            p.stalled = false;
            p.starting = false;
            p.admin = false;
        }
    }
}

/// Remplace **toutes** les lignes du greffon `name` par `lignes`.
///
/// Employé au câblage à chaud. Le remplacement n'est pas un détail : un greffon
/// relancé à la main se réannonce, et une insertion en plus des lignes
/// existantes lui ferait accumuler des doublons dans la page de statut à chaque
/// relance — jusqu'à une page illisible sur un appareil qu'on ne redémarre
/// jamais.
///
/// Les nouvelles lignes sont posées là où étaient les anciennes, pour que
/// l'ordre affiché ne saute pas d'un recâblage à l'autre.
///
/// Une liste **vide** ne fait pas disparaître le greffon : elle le laisse
/// visible en genre inconnu, non joint. Une annonce à `kinds: []` vient d'un
/// binaire mal compilé, et retirer ses lignes sans en insérer aucune le rendrait
/// invisible juste après qu'il a parlé — l'inverse exact de ce que le câblage à
/// chaud existe pour donner à voir. `stalled` reste faux : il vient de parler,
/// il n'est pas muet.
///
/// `admin` ne sert **que** dans ce cas de repli, et il y est indispensable :
/// `PluginStatus::genre_inconnu` met le drapeau à faux par construction, si bien
/// qu'un greffon annonçant `kinds: []` **et** `admin: true` — un binaire mal
/// compilé, mais dont la page d'admin est bel et bien jointe — voyait son dorsal
/// câblé sans rien dans l'IHM pour y mener. Les lignes non vides portent déjà
/// leur propre drapeau, l'appelant l'ayant posé genre par genre.
pub fn replace_plugin_lines(
    state: &mut StatusState,
    name: &str,
    lignes: Vec<PluginStatus>,
    admin: bool,
) {
    let place = state.plugins.iter().position(|p| p.name == name);
    state.plugins.retain(|p| p.name != name);
    let place = place.unwrap_or(state.plugins.len()).min(state.plugins.len());
    let lignes = if lignes.is_empty() {
        vec![PluginStatus { admin, ..PluginStatus::genre_inconnu(name, false) }]
    } else {
        lignes
    };
    for (i, ligne) in lignes.into_iter().enumerate() {
        state.plugins.insert(place + i, ligne);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::reglages_validation::SettingsError;
    use crate::status::tests_support::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    /// Montage avec un vrai `plugins.toml` temporaire et l'oreille du cœur
    /// conservée : les deux choses que la route touche.
    fn app_state_avec_greffons(
    ) -> (AppState, tempfile::TempDir, tokio::sync::mpsc::Receiver<OrdreGreffon>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            "[[plugin]]\nname = \"radio\"\nexec = \"/bin/true\"\n\n\
             [[plugin]]\nname = \"cd\"\nexec = \"/bin/true\"\n",
        )
        .unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let state = AppState {
            greffons: Arc::new(GreffonsControle {
                manifeste: path,
                noms: vec!["radio".into(), "cd".into()],
                tx,
            }),
            ..app_state()
        };
        (state, dir, rx)
    }

    #[tokio::test]
    async fn eteindre_persiste_puis_previent_le_coeur() {
        let (state, dir, mut rx) = app_state_avec_greffons();
        let app = router(state.clone());
        // Le cœur : il accuse réception, comme la boucle principale.
        let coeur = tokio::spawn(async move {
            let ordre = rx.recv().await.unwrap();
            assert_eq!(ordre.nom, "cd");
            assert!(!ordre.actif);
            let _ = ordre.ack.send(true);
        });

        let resp = app
            .oneshot(
                Request::put("/api/plugins/cd/enabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        coeur.await.unwrap();
        let apres = std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap();
        assert!(apres.contains("enabled = false"), "{apres}");
    }

    #[tokio::test]
    async fn un_refus_explicite_du_coeur_renvoie_500_avec_un_message_du_catalogue() {
        // Rallumage sans binaire au chemin `exec` : le cœur répond `false`,
        // pas un canal fermé. Le seul branchement d'`ack_rx` qui restait non
        // couvert.
        let (state, _dir, mut rx) = app_state_avec_greffons();
        let app = router(state);
        let coeur = tokio::spawn(async move {
            let ordre = rx.recv().await.unwrap();
            let _ = ordre.ack.send(false);
        });

        let resp = app
            .oneshot(
                Request::put("/api/plugins/radio/enabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        coeur.await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Un message du catalogue, jamais une clé brute.
        assert!(v["error"].as_str().unwrap().contains("radio"));
    }

    #[tokio::test]
    async fn un_nom_non_declare_est_refuse_sans_rien_ecrire() {
        let (state, dir, _rx) = app_state_avec_greffons();
        let avant = std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap();
        let app = router(state);

        let resp = app
            .oneshot(
                Request::put("/api/plugins/jamais-vu/enabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Un message, jamais une clé de catalogue.
        assert!(v["error"].as_str().unwrap().contains("jamais-vu"));
        assert_eq!(std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap(), avant);
    }

    #[tokio::test]
    async fn une_persistance_impossible_ne_touche_pas_au_runtime() {
        let (mut state, dir, mut rx) = app_state_avec_greffons();
        // Manifeste introuvable : l'écriture échouera.
        state.greffons = Arc::new(GreffonsControle {
            manifeste: dir.path().join("absent.toml"),
            noms: vec!["radio".into()],
            tx: state.greffons.tx.clone(),
        });
        let app = router(state);

        let resp = app
            .oneshot(
                Request::put("/api/plugins/radio/enabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // Rien n'a été demandé au cœur : un greffon tué dont l'extinction n'est
        // pas persistée reviendrait au prochain démarrage.
        assert!(rx.try_recv().is_err());
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Un message du catalogue, jamais une clé brute : sans cette
        // assertion, une faute de frappe dans `plugin_persist_failed`
        // laisserait passer la suite verte avec une clé crue à l'écran.
        assert!(v["error"].as_str().unwrap().contains("radio"));
    }

    /// Aucune clé de refus ne peut atteindre l'écran telle quelle.
    ///
    /// Les tests de `message()` résolvent contre un catalogue **ad hoc**, ce qui
    /// prouve l'interpolation mais pas que la clé écrite dans le code existe
    /// vraiment : `Catalog::get` rend la clé quand il ne la trouve pas, donc une
    /// faute de frappe produirait un toast affichant
    /// « settings_initial_delay_out_of_range » sans qu'aucun test ne s'en
    /// plaigne. Le test de parité entre catalogues ne le voit pas non plus : il
    /// compare les deux fichiers entre eux, pas au code qui les appelle.
    ///
    /// Celui-ci résout donc chaque variante contre le **catalogue anglais
    /// réellement embarqué**, et refuse un message égal à sa propre clé.
    #[test]
    fn chaque_refus_resout_contre_le_catalogue_embarque() {
        let catalog = Catalog::load("core", "en", std::path::Path::new("/inexistant"), crate::i18n::EN);
        // Une clé absente se reconnaît à ce que le message **est** la clé : pas
        // d'espace, et le préfixe qu'on lui a donné.
        let messages = [
            AudioOutputError::EmptyName.message(&catalog),
            SettingsError::InitialDelay { min: 200, max: 5000 }.message(&catalog),
            SettingsError::RepeatInterval { min: 100, max: 2000 }.message(&catalog),
            SettingsError::Overlay { min: 1000, max: 15000 }.message(&catalog),
            SettingsError::TensWindow { min: 1000, max: 15000 }.message(&catalog),
            SettingsError::SeekStep { min: 1, max: 120 }.message(&catalog),
        ];
        for m in &messages {
            assert!(
                m.contains(' '),
                "message réduit à une clé brute, donc absente du catalogue embarqué : {m:?}"
            );
        }
        // Et les bornes arrivent bien interpolées, pas en jetons.
        let borne = SettingsError::InitialDelay { min: 200, max: 5000 }.message(&catalog);
        assert!(borne.contains("200") && borne.contains("5000"), "bornes non interpolées : {borne:?}");
        assert!(!borne.contains("{min}") && !borne.contains("{max}"), "jeton laissé tel quel : {borne:?}");
    }

    #[test]
    fn busy_est_additif_absent_quand_faux() {
        let l = PluginStatus::genre("radio", "source", true, true);
        let json = serde_json::to_string(&l).unwrap();
        assert!(!json.contains("busy"), "{json}");
    }

    #[test]
    fn mark_plugin_disconnected_bascule_connected() {
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::genre("radio", "source", true, true),
                PluginStatus::genre("cd", "source", true, false),
            ],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "cd");
        assert!(!st.plugins.iter().find(|p| p.name == "cd").unwrap().connected);
        assert!(st.plugins.iter().find(|p| p.name == "radio").unwrap().connected);
        // Nom inconnu : no-op, ne panique pas.
        mark_plugin_disconnected(&mut st, "inconnu");
    }

    #[test]
    fn mark_plugin_disconnected_bascule_toutes_les_lignes_dun_greffon_a_plusieurs_genres() {
        // Un greffon peut annoncer plusieurs genres (par exemple input et
        // display) : la page de statut porte alors une ligne par (nom, genre)
        // pour ce même nom. `admin` est un drapeau booléen porté par chaque
        // ligne de genre, jamais un genre en soi. `mark_plugin_disconnected`
        // boucle déjà sur toutes les lignes de même nom, mais rien ne le
        // prouvait jusqu'ici.
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::genre("files", "input", true, true),
                PluginStatus::genre("files", "display", true, true),
                PluginStatus::genre("radio", "source", true, true),
            ],
            active_source: "files".into(),
        };
        mark_plugin_disconnected(&mut st, "files");
        assert!(
            st.plugins.iter().filter(|p| p.name == "files").all(|p| !p.connected),
            "les deux lignes de files doivent basculer"
        );
        assert!(
            st.plugins.iter().find(|p| p.name == "radio").unwrap().connected,
            "les lignes d'un autre greffon ne doivent pas etre touchees"
        );
    }

    #[test]
    fn mark_plugin_disconnected_efface_le_drapeau_fige() {
        // Un greffon figé qui meurt plus tard : `plugin_waits` le voit, et ses
        // lignes doivent cesser d'annoncer « vivant mais muet ». Les deux
        // drapeaux ensemble décriraient un état qui n'existe pas.
        let mut st = StatusState {
            plugins: vec![PluginStatus::genre_inconnu("files", true)],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "files");
        let ligne = &st.plugins[0];
        assert!(!ligne.connected);
        assert!(!ligne.stalled, "un processus dont on a vu la sortie n'est plus fige");
    }

    #[test]
    fn mark_plugin_disconnected_efface_le_drapeau_demarrage() {
        // Un greffon qui meurt **pendant** ses dix secondes de grâce. Sans cet
        // effacement, sa ligne restait « démarrage » jusqu'à l'échéance, et
        // comme `main::a_retrograder` ne consulte que ce drapeau, le balayage
        // la rétrogradait ensuite en « figé » : vivant mais muet, pour un
        // processus dont la sortie avait été moissonnée. Les deux drapeaux
        // décrivent un vivant, et c'est pourquoi les deux tombent ici.
        let mut st = StatusState {
            plugins: vec![PluginStatus::demarrage("cd")],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "cd");
        let ligne = &st.plugins[0];
        assert!(!ligne.connected);
        assert!(!ligne.starting, "un processus dont on a vu la sortie ne demarre plus");
        assert!(!ligne.stalled, "et il n'est pas fige non plus");
    }

    #[test]
    fn mark_plugin_disconnected_retire_la_page_dadmin_du_menu() {
        // Le menu du haut de l'IHM est `plugins.filter(p => p.admin)`, sans
        // regard sur `connected` : sans cet effacement, l'entrée d'un greffon
        // mort restait offerte et le clic rendait une erreur au lieu de rien.
        // Exactement la plainte déjà traitée pour le cas « éteint ».
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::genre("files", "input", true, true),
                PluginStatus::genre("files", "display", true, true),
                PluginStatus::genre("radio", "source", true, true),
            ],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "files");
        assert!(
            st.plugins.iter().filter(|p| p.name == "files").all(|p| !p.admin),
            "toutes les lignes du greffon mort doivent cesser d'annoncer une page"
        );
        assert!(
            st.plugins.iter().find(|p| p.name == "radio").unwrap().admin,
            "la page d'un autre greffon ne doit pas etre touchee"
        );
    }

    #[test]
    fn une_ligne_desactivee_ne_promet_rien() {
        let l = PluginStatus::desactive("cd");
        assert!(l.disabled);
        assert!(!l.connected, "aucun processus : rien n'est joint");
        assert!(!l.stalled, "il ne se tait pas, il n'existe pas");
        assert!(!l.admin, "pas de page d'admin à atteindre");
        assert_eq!(l.kind, "unknown");
    }

    #[test]
    fn disabled_est_omis_quand_il_est_faux() {
        // Idiome de `stalled` : aucune trame existante ne change de forme.
        let json = serde_json::to_string(&PluginStatus::genre("radio", "source", true, false)).unwrap();
        assert!(!json.contains("disabled"), "{json}");
        let json = serde_json::to_string(&PluginStatus::desactive("cd")).unwrap();
        assert!(json.contains("\"disabled\":true"), "{json}");
    }

    #[test]
    fn une_reannonce_remplace_les_lignes_du_greffon_au_lieu_den_ajouter() {
        // Un greffon relancé à la main se réannonce, et le cœur le recâble. S'il
        // accumulait une ligne de plus à chaque fois, la page de statut d'un
        // appareil qu'on ne redémarre jamais finirait illisible.
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::genre_inconnu("files", true),
                PluginStatus::genre("radio", "source", true, true),
            ],
            active_source: "radio".into(),
        };
        // Première annonce : le figé devient deux lignes de genre.
        replace_plugin_lines(
            &mut st,
            "files",
            vec![
                PluginStatus::genre("files", "source", true, true),
                PluginStatus::genre("files", "input", true, true),
            ],
            true,
        );
        // Ré-annonce, cette fois sans le genre `input`.
        replace_plugin_lines(
            &mut st,
            "files",
            vec![PluginStatus::genre("files", "source", true, true)],
            true,
        );

        assert_eq!(
            st.plugins.iter().filter(|p| p.name == "files").count(),
            1,
            "les lignes ne doivent pas s'accumuler d'une reannonce a l'autre"
        );
        assert_eq!(st.plugins.len(), 2, "les autres greffons restent intacts");
        assert!(st.plugins.iter().any(|p| p.name == "radio"));
        // La place du greffon dans la liste ne saute pas d'un recablage à
        // l'autre : `files` était premier, il le reste.
        assert_eq!(st.plugins[0].name, "files");
    }

    #[test]
    fn une_annonce_sans_aucun_genre_laisse_le_greffon_visible() {
        // `kinds: []` : greffon mal compilé, ou binaire qui se trompe. Retirer
        // ses lignes sans en insérer aucune le faisait **disparaître** de la
        // page juste après qu'il a parlé — un greffon fautif devenu invisible,
        // l'inverse de ce que ce câblage existe pour donner à voir.
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::genre("files", "source", true, false),
                PluginStatus::genre("radio", "source", true, false),
            ],
            active_source: "radio".into(),
        };
        replace_plugin_lines(&mut st, "files", vec![], false);

        assert_eq!(st.plugins.len(), 2, "le greffon reste dans la page");
        let ligne = st.plugins.iter().find(|p| p.name == "files").unwrap();
        assert_eq!(ligne.kind, "unknown");
        assert!(!ligne.connected);
        assert!(!ligne.stalled, "il vient de parler : il n'est pas muet");
        assert_eq!(st.plugins[0].name, "files", "et il garde sa place");
    }

    #[test]
    fn une_annonce_sans_genre_garde_son_drapeau_admin() {
        // `kinds: []` **et** `admin: true` : le binaire est mal compilé mais sa
        // page d'admin est jointe, donc le dorsal est câblé. La ligne de repli
        // vient de `genre_inconnu`, dont `admin` est faux par construction : sans
        // le drapeau porté jusqu'ici, l'IHM n'affichait aucun lien vers une page
        // qui existe — l'inverse exact de ce que la règle « le drapeau suit ce
        // qui a été joint » cherchait.
        let mut st = StatusState { plugins: vec![], active_source: String::new() };
        replace_plugin_lines(&mut st, "files", vec![], true);

        let ligne = &st.plugins[0];
        assert_eq!(ligne.kind, "unknown");
        assert!(!ligne.connected, "aucun genre n'a ete joint");
        assert!(ligne.admin, "la page d'admin est jointe : le lien doit apparaitre");
    }

    #[test]
    fn le_drapeau_fige_est_absent_du_json_quand_il_est_faux() {
        // Champ additif : la trame d'un greffon câblé ne change pas d'un octet,
        // et une trame ancienne se relit sans erreur.
        let cable = PluginStatus::genre("radio", "source", true, true);
        assert_eq!(
            serde_json::to_string(&cable).unwrap(),
            r#"{"name":"radio","kind":"source","connected":true,"admin":true}"#
        );
        let fige = PluginStatus::genre_inconnu("files", true);
        assert_eq!(
            serde_json::to_string(&fige).unwrap(),
            r#"{"name":"files","kind":"unknown","connected":false,"admin":false,"stalled":true}"#
        );
        let ancien: PluginStatus = serde_json::from_str(
            r#"{"name":"radio","kind":"source","connected":false,"admin":false}"#,
        )
        .unwrap();
        assert!(!ancien.stalled);
    }
}
