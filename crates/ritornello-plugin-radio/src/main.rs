mod admin;
mod config;
mod directory;
// Uniquement compile sous `cargo test` : `ui_placeholder_js` ne sert au run-
// time nulle part dans ce crate (contrairement a `placeholder_html` du coeur,
// utilise en repli par `web.rs`), seulement a `build.rs` (compilation
// separee, via `include!`) et a ses propres tests. Le compiler en continu
// dans le binaire declencherait un `dead_code` que `-D warnings` refuserait.
#[cfg(test)]
mod placeholder;
mod state;

use crate::admin::RadioAdmin;
use anyhow::Result;
use config::Stations;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{
    run_admin_plugin, run_source_plugin, Notification, SourceOutcome, SourcePlugin,
};
use ritornello_proto::{SourceAction, View};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

pub(crate) const RADIO_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct RadioSource {
    state_path: PathBuf,
    stations: Arc<AsyncRwLock<Stations>>,
    preset: u8,
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
    /// Reçoit le nouveau `Stations::preset_count()` annoncé par la moitié
    /// Admin après un enregistrement réussi (voir `RadioAdmin::set_data`).
    /// `None` en mode dégradé (pas de `--admin-socket`, donc pas de moitié
    /// Admin pour émettre) : `poll_notification` reste alors en attente pour
    /// toujours plutôt que de rendre `None`, qui est **terminal** pour le
    /// SDK et journaliserait un avertissement trompeur pour un déploiement
    /// pourtant légitime.
    preset_count_rx: Option<tokio::sync::watch::Receiver<u8>>,
}

impl RadioSource {
    fn view_for(&self, preset: u8, status: &str) -> View {
        View { line1: format!("RADIO  P{preset}"), line2: status.to_string(), line3: String::new() }
    }

    /// Identité de ce que joue la radio : le flux, désigné par son URL.
    ///
    /// Opaque pour le cœur, qui ne fait que la comparer et la relayer. C'est en
    /// revanche ce qu'un plugin `metadata` lit pour reconnaître une station :
    /// l'URL est la seule chose qui distingue durablement un flux (le nom de
    /// présélection, lui, dépend de la configuration de l'appareil).
    fn identite_du_flux(url: &str) -> serde_json::Value {
        serde_json::json!({ "kind": "stream", "url": url })
    }

    async fn play_preset(&mut self, n: u8) -> SourceOutcome {
        let stations = self.stations.read().await;
        // How many numbered presets exist right now, for the web grid — see
        // `Stations::preset_count`. Declared on both branches below: a miss
        // (empty preset) still tells the truth about the table.
        let count = stations.preset_count();
        if let Some(st) = stations.by_preset(n) {
            self.preset = n;
            // `update` et non `save` : la moitié Admin écrit le pays choisi dans
            // ce même fichier, et un `save` construit ici l'effacerait.
            // L'échec est journalisé, comme le fait déjà la moitié Admin : un
            // /var/lib en lecture seule perdrait la présélection à chaque
            // redémarrage sans que rien ne le dise.
            if let Err(e) = state::update(&self.state_path, |s| s.preset = n) {
                tracing::warn!("failed to persist preset: {e}");
            }
            SourceOutcome::new(SourceAction::Play { uri: st.url.clone() })
                .with_view(View {
                    line1: format!("RADIO  P{n}"),
                    line2: st.name.clone(),
                    line3: String::new(),
                })
                .plays(Self::identite_du_flux(&st.url))
                // La touche que l'IHM doit mettre en évidence : seule la
                // Source sait à quelle présélection correspond ce qui joue.
                .preset(n)
                // Le nom configuré de la station : c'est ce que la carte
                // Lecteur affiche à côté du numéro de présélection.
                .preset_name(st.name.clone())
                .preset_count(count)
        } else {
            let empty = self.catalog.read().unwrap().get("empty_preset").to_string();
            // Message **éphémère** : rien n'a été lancé, donc la station
            // précédente joue toujours et doit reparaître à l'écran. Le laisser
            // permanent faisait décrire durablement un état qui n'existait pas.
            //
            // Et surtout, aucune déclaration d'identité : `plays_nothing()`
            // serait faux ici, puisque le flux précédent continue — cela aurait
            // fait cesser les plugins `metadata` et vidé le titre affiché.
            SourceOutcome::new(SourceAction::Noop)
                .with_view(self.view_for(self.preset, &empty))
                .transient()
                .status(empty)
                .preset_count(count)
        }
    }
}

#[async_trait::async_trait]
impl SourcePlugin for RadioSource {
    async fn activate(&mut self) -> SourceOutcome {
        let preset = self.preset;
        self.play_preset(preset).await
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Stop).plays_nothing()
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        self.play_preset(n).await
    }
    async fn next(&mut self) -> SourceOutcome {
        let next = self.stations.read().await.next_preset(self.preset);
        match next {
            // Une seule station configurée : next_preset reboucle sur le
            // preset courant. Rejouer provoquerait une reconnexion du flux
            // live (loadfile mpv), audible comme un changement de station
            // alors que l'affichage ne bouge pas. Rien à faire dans ce cas —
            // et surtout ne rien dire de l'identité, qui n'a pas changé.
            Some(n) if n == self.preset => SourceOutcome::new(SourceAction::Noop),
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome::new(SourceAction::Noop),
        }
    }
    async fn prev(&mut self) -> SourceOutcome {
        let prev = self.stations.read().await.prev_preset(self.preset);
        match prev {
            // Voir commentaire dans next() : même garde contre la
            // reconnexion audible quand une seule station est configurée.
            Some(n) if n == self.preset => SourceOutcome::new(SourceAction::Noop),
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome::new(SourceAction::Noop),
        }
    }
    async fn eject(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop)
    }
    async fn set_locale(&mut self, locale: String) {
        *self.catalog.write().unwrap() = Catalog::load("radio", &locale, &self.locales_root, RADIO_EN);
    }

    /// Annonce spontanément le nouveau `preset_count` quand la moitié Admin
    /// vient d'enregistrer une table de stations — c'est ce qui met à jour la
    /// grille de la télécommande web sans attendre qu'une présélection soit
    /// jouée (défaut constaté à l'usage : la grille restait sur l'ancien jeu
    /// de numéros jusque-là).
    ///
    /// Ne porte **que** `preset_count` : ni vue, ni identité, ni présélection,
    /// jamais éphémère. C'est ce qui garantit que cette notification ne
    /// perturbe ni l'affichage courant ni le morceau en cours de lecture —
    /// `Core::handle_source_update` fusionne champ par champ, donc une trame
    /// muette sur tout le reste ne touche à rien d'autre.
    async fn poll_notification(&mut self) -> Option<Notification> {
        let Some(rx) = &mut self.preset_count_rx else {
            // Mode dégradé (pas de moitié Admin) : voir le commentaire sur le
            // champ. Jamais `None` ici, qui serait terminal pour le SDK.
            return std::future::pending().await;
        };
        match rx.changed().await {
            Ok(()) => {
                let n = *rx.borrow_and_update();
                Some(Notification::new().preset_count(n))
            }
            // L'émetteur (moitié Admin) a disparu — ne devrait pas arriver en
            // pratique tant que les deux moitiés partagent le même processus,
            // mais rien ne justifie de traiter ça comme une fin définitive de
            // notifications : on retombe sur la même attente indéfinie que le
            // mode dégradé plutôt que de rendre `None`.
            Err(_) => std::future::pending().await,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = ritornello_plugin_sdk::socket_path();
    // `--admin-socket` n'est fourni par le cœur que si `admin = true` dans
    // plugins.toml. Absent (oubli lors d'une mise à jour de plugins.toml, ou
    // usage volontaire sans page d'admin), on continue en mode dégradé :
    // la moitié Source tourne seule, sans page de gestion des stations.
    let admin_socket = ritornello_plugin_sdk::admin_socket_path();
    if admin_socket.is_none() {
        tracing::warn!(
            "--admin-socket absent: the station management page will not be served, only the Source half runs (missing 'admin = true' in plugins.toml)"
        );
    }
    let stations_path = PathBuf::from(env_or("RITORNELLO_RADIO_STATIONS", "/etc/ritornello/stations.toml"));
    let state_path = PathBuf::from(env_or("RITORNELLO_RADIO_STATE", "/var/lib/ritornello/plugin-radio.json"));

    let stations = Stations::load(&stations_path).unwrap_or_else(|e| {
        tracing::warn!("stations.toml invalid or missing ({e}): starting without stations");
        Stations::default()
    });
    let preset = state::load(&state_path).preset;
    let stations_shared = Arc::new(AsyncRwLock::new(stations));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let catalog = Arc::new(RwLock::new(Catalog::load("radio", "en", &locales_root, RADIO_EN)));

    // Canal Admin -> Source pour l'annonce spontanée de `preset_count` (voir
    // `RadioAdmin::set_data` et `RadioSource::poll_notification`). La valeur
    // initiale ne sert jamais : seuls les changements ultérieurs comptent, le
    // compte de démarrage est déjà porté par `activate`/`select`.
    let (preset_count_tx, preset_count_rx) = tokio::sync::watch::channel(0u8);

    let source = RadioSource {
        state_path: state_path.clone(),
        stations: stations_shared.clone(),
        preset,
        catalog: catalog.clone(),
        locales_root,
        // Le récepteur n'a de sens que si une moitié Admin existe pour
        // émettre dessus (voir plus bas) : sinon `poll_notification` doit
        // rester en attente pour toujours, pas se rabattre sur un canal mort.
        preset_count_rx: admin_socket.is_some().then_some(preset_count_rx),
    };
    // Annuaire en ligne : la liste intégrée de serveurs, essayés dans l'ordre
    // jusqu'au premier qui répond, ou l'unique serveur épinglé par
    // `RITORNELLO_RADIO_DIRECTORY`. Journalisé au démarrage : sur un Pi sans
    // écran, savoir quels serveurs seront interrogés évite de deviner.
    let directory = directory::HttpDirectory::from_env();
    tracing::info!("radio directory, candidate servers: {}", directory.bases.join(", "));
    // La moitié admin n'est construite que si `--admin-socket` a été fourni
    // (mode dégradé sinon, voir plus haut).
    let admin = admin_socket.map(|admin_socket| {
        (
            RadioAdmin {
                stations_path,
                state_path,
                stations: stations_shared,
                catalog,
                directory: Arc::new(directory),
                search: RwLock::new(Vec::new()),
                countries: RwLock::new(Vec::new()),
                preset_count_tx,
            },
            admin_socket,
        )
    });

    // Les deux moitiés sont indépendantes : une panne (déconnexion, erreur
    // d'écriture, voire panique sur un lock empoisonné) sur la socket admin ne
    // doit pas tuer la lecture audio, et réciproquement. Chaque moitié tourne
    // dans sa propre tâche tokio::spawn : une panique y est capturée dans le
    // JoinHandle (JoinError) au lieu de dérouler la pile de l'autre moitié,
    // ce qu'un simple tokio::join! sur des blocs async inline ne garantirait
    // pas (les deux futures seraient pollées dans la même tâche). Quand
    // `admin` est `None`, seule la moitié Source est lancée : jamais de
    // `try_join!` ici, les deux tâches (quand les deux existent) restent
    // suivies indépendamment.
    let source_handle = tokio::spawn(async move { run_source_plugin(source, &socket_path).await });

    match admin {
        Some((admin, admin_socket)) => {
            let admin_handle = tokio::spawn(async move { run_admin_plugin(admin, &admin_socket).await });
            let (source_res, admin_res) = tokio::join!(source_handle, admin_handle);
            log_half("source half", source_res);
            log_half("admin half", admin_res);
        }
        None => {
            log_half("source half", source_handle.await);
        }
    }

    Ok(())
}

/// Logue le résultat d'une des deux moitiés (succès / erreur applicative /
/// panique) sans jamais faire remonter l'échec d'une moitié sur l'autre.
fn log_half(label: &str, res: std::result::Result<Result<()>, tokio::task::JoinError>) {
    match res {
        Ok(Ok(())) => tracing::warn!("radio plugin ({label}) exited normally"),
        Ok(Err(e)) => tracing::warn!("radio plugin ({label}) error: {e}"),
        Err(join_err) => tracing::error!("radio plugin ({label}) panicked: {join_err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_preset_utilise_le_catalogue_apres_set_locale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "empty_preset = \"PRESET VIDE\"\n").unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        let catalog = Arc::new(RwLock::new(Catalog::load("radio", "en", dir.path(), RADIO_EN)));
        let mut source = RadioSource {
            state_path: state_dir.path().join("plugin-radio.json"),
            stations: Arc::new(AsyncRwLock::new(Stations::default())),
            preset: 1,
            catalog: catalog.clone(),
            locales_root: dir.path().to_path_buf(),
            preset_count_rx: None,
        };
        source.set_locale("fr".into()).await;
        // aucun preset chargé → branche "empty_preset"
        let outcome = source.select(1).await;
        assert_eq!(outcome.view.unwrap().line2, "PRESET VIDE");
    }

    #[test]
    fn en_embarque_radio_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(RADIO_EN).unwrap().is_empty());
    }

    fn make_source(stations: Stations, preset: u8) -> RadioSource {
        let dir = tempfile::tempdir().unwrap();
        RadioSource {
            state_path: dir.path().join("plugin-radio.json"),
            stations: Arc::new(AsyncRwLock::new(stations)),
            preset,
            catalog: Arc::new(RwLock::new(Catalog::load("radio", "en", dir.path(), RADIO_EN))),
            locales_root: dir.path().to_path_buf(),
            preset_count_rx: None,
        }
    }

    fn one_station() -> Stations {
        config::Stations {
            stations: vec![config::Station {
                name: "FIP".into(),
                url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(),
                preset: 1,
            }],
        }
    }

    fn two_stations() -> Stations {
        config::Stations {
            stations: vec![
                config::Station {
                    name: "FIP".into(),
                    url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(),
                    preset: 1,
                },
                config::Station {
                    name: "France Inter".into(),
                    url: "http://icecast.radiofrance.fr/franceinter-midfi.mp3".into(),
                    preset: 2,
                },
            ],
        }
    }

    #[tokio::test]
    async fn une_seule_station_next_et_prev_sont_sans_effet() {
        let mut source = make_source(one_station(), 1);
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.view.is_none());
        // Ne rien dire de l'identité : la station n'a pas changé, et annoncer un
        // changement remettrait à zéro les métadonnées du morceau en cours.
        assert!(outcome.identity.is_none());

        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.view.is_none());
        assert!(outcome.identity.is_none());
    }

    #[tokio::test]
    async fn jouer_une_station_declare_son_flux_comme_identite() {
        let mut source = make_source(two_stations(), 1);
        let outcome = source.select(2).await;
        assert_eq!(
            outcome.identity,
            Some(ritornello_proto::IdentityUpdate::Playing(serde_json::json!({
                "kind": "stream",
                "url": "http://icecast.radiofrance.fr/franceinter-midfi.mp3"
            })))
        );
    }

    #[tokio::test]
    async fn jouer_une_preselection_la_declare_pour_lihm() {
        // C'est ce qui permet a la telecommande web de mettre la touche active
        // en evidence : seule la Source sait a quelle preselection correspond
        // ce qui joue.
        let mut source = make_source(two_stations(), 1);
        let outcome = source.select(2).await;
        assert_eq!(outcome.preset, Some(2));
        // Le nom configure de la station accompagne toujours le numero.
        assert_eq!(outcome.preset_name.as_deref(), Some("France Inter"));
        // Le compte de preselections (ici 2, la plus haute de two_stations)
        // est declare sur la branche "trouvee".
        assert_eq!(outcome.preset_count, Some(2));
        // Et une preselection vide ne declare ni preset ni nom : ce qui joue
        // n'a pas change, la station precedente continue.
        let outcome = source.select(7).await;
        assert_eq!(outcome.preset, None);
        assert_eq!(outcome.preset_name, None, "aucun nom sur la branche vide : rien n'a change");
        // ... mais le compte reste declare sur la branche "vide" aussi : la
        // table n'a pas change, seule la selection a echoue.
        assert_eq!(outcome.preset_count, Some(2));
    }

    #[tokio::test]
    async fn une_preselection_vide_affiche_un_message_ephemere_sans_couper_la_lecture() {
        // Defaut constate a l'usage : le message restait a l'ecran
        // indefiniment. Or rien n'a ete lance — la station precedente joue
        // toujours — donc l'affichage doit revenir a elle, et surtout les
        // metadonnees ne doivent pas etre effacees.
        let mut source = make_source(Stations::default(), 1);
        let outcome = source.select(4).await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.transient, "le message doit s'effacer de lui-meme");
        assert!(
            outcome.identity.is_none(),
            "declarer un arret serait faux : le flux precedent continue"
        );
        assert!(outcome.view.is_some());
        // Le mot ephemere est declare via `status` : c'est lui qui alimente
        // l'incrustation cote coeur, pas la vue qui l'accompagne encore.
        assert_eq!(outcome.status.as_deref(), Some("empty preset"));
        // Table vide : le compte declare est 0, pas absent.
        assert_eq!(outcome.preset_count, Some(0));
    }

    #[tokio::test]
    async fn se_desactiver_declare_que_plus_rien_ne_joue() {
        let mut source = make_source(two_stations(), 1);
        let outcome = source.deactivate().await;
        assert!(matches!(outcome.action, SourceAction::Stop));
        assert_eq!(outcome.identity, Some(ritornello_proto::IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn deux_stations_next_et_prev_rebouclent_toujours_vers_l_autre() {
        let mut source = make_source(two_stations(), 1);
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));

        let mut source = make_source(two_stations(), 1);
        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));
    }

    #[tokio::test]
    async fn activate_sur_le_preset_courant_rejoue_quand_meme_le_flux() {
        // Chemin de récupération après coupure (retry_stream côté cœur) :
        // activate() doit continuer à rejouer le même preset, sans la garde
        // ajoutée à next()/prev().
        let mut source = make_source(one_station(), 1);
        let outcome = source.activate().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));
    }

    #[tokio::test]
    async fn poll_notification_ne_declare_que_le_compte() {
        // Propriete de surete : c'est ce qui garantit que l'annonce
        // spontanee du nouveau compte (enregistrement depuis la page
        // d'admin) ne perturbe ni l'affichage courant ni le morceau en
        // cours de lecture. Voir le commentaire sur `poll_notification`.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(rx);
        tx.send(5).unwrap();

        let n = source.poll_notification().await.expect("notification attendue");
        assert_eq!(n.preset_count, Some(5));
        assert!(n.view.is_none(), "l'affichage ne doit pas bouger");
        assert!(n.identity.is_none(), "le morceau en cours ne doit pas bouger");
        assert!(n.preset.is_none());
    }

    #[tokio::test]
    async fn la_valeur_initiale_dun_watch_frais_nest_pas_vue_comme_un_changement() {
        // Pilier dont dépend `poll_notification` : un `watch::channel(v).1`
        // fraîchement créé ne signale jamais sa valeur de départ comme un
        // changement pour `changed()`. Si cette propriété cessait d'être
        // vraie — ou si le câblage passait par `subscribe()` puis
        // `mark_changed()`, ou déplaçait la création du canal ailleurs —
        // chaque démarrage radio annoncerait `preset_count(0)` avant même
        // la première lecture : grille vide et « Présélections : 0 » jusqu'à
        // ce que quelque chose joue.
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(tokio::sync::watch::channel(0u8).1);
        let resultat = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            source.poll_notification(),
        )
        .await;
        assert!(
            resultat.is_err(),
            "la valeur initiale du watch ne doit produire aucune notification"
        );
    }

    #[tokio::test]
    async fn sans_moitie_admin_poll_notification_reste_en_attente() {
        // Mode dégradé (`--admin-socket` absent) : aucun émetteur n'existe
        // pour ce plugin, donc rien ne doit jamais en sortir — surtout pas un
        // `None`, terminal pour le SDK et source d'un avertissement trompeur
        // pour un déploiement pourtant légitime (voir le commentaire sur le
        // champ `preset_count_rx`).
        let mut source = make_source(two_stations(), 1);
        assert!(source.preset_count_rx.is_none());
        let resultat = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            source.poll_notification(),
        )
        .await;
        assert!(resultat.is_err(), "poll_notification n'aurait jamais du se terminer");
    }
}
