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
use ritornello_plugin_sdk::{Notification, Runtime, SourceOutcome, SourcePlugin};
use ritornello_proto::SourceAction;
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
    /// URL du flux qui joue, quand quelque chose joue.
    ///
    /// La présélection est une **position** : remanier la table depuis la page
    /// fait donc pointer le numéro mémorisé sur une autre station, et l'écran
    /// annonçait le mauvais nom pour le flux qui continuait. L'URL, elle,
    /// identifie durablement ce qui joue, et permet de retrouver le bon numéro
    /// dans la table remaniée.
    url_en_cours: Option<String>,
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
    /// Reçoit le nouveau `Stations::preset_count()` annoncé par la moitié
    /// Admin après un enregistrement réussi (voir `RadioAdmin::set_data`).
    /// `main()` construit toujours ce champ à `Some` : la page d'admin est
    /// enregistrée sans condition auprès de `Runtime`. `None` n'apparaît que
    /// dans les tests, qui construisent `RadioSource` directement sans passer
    /// par `Runtime` et donc sans moitié Admin pour émettre sur ce canal ;
    /// `poll_notification` reste alors en attente pour toujours plutôt que de
    /// rendre `None`, qui est **terminal** pour le SDK.
    preset_count_rx: Option<tokio::sync::watch::Receiver<u8>>,
}

impl RadioSource {
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
            self.url_en_cours = Some(st.url.clone());
            // `update` et non `save` : la moitié Admin écrit le pays choisi dans
            // ce même fichier, et un `save` construit ici l'effacerait.
            // L'échec est journalisé, comme le fait déjà la moitié Admin : un
            // /var/lib en lecture seule perdrait la présélection à chaque
            // redémarrage sans que rien ne le dise.
            if let Err(e) = state::update(&self.state_path, |s| s.preset = n) {
                tracing::warn!("failed to persist preset: {e}");
            }
            SourceOutcome::new(SourceAction::play(st.url.clone()))
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
        // Plus rien ne joue : oublier l'URL, sinon un remaniement de la table
        // corrigerait la présélection d'un flux arrêté.
        self.url_en_cours = None;
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
    /// Elle corrige aussi le **numéro et le nom** de ce qui joue quand le
    /// remaniement les a déplacés : la présélection est une position, donc
    /// réordonner les stations faisait annoncer le nom d'une autre station pour
    /// le flux qui continuait, et un redémarrage reprenait la mauvaise. Le flux
    /// est retrouvé par son URL, seule chose qui l'identifie durablement.
    ///
    /// Ne porte **ni statut, ni identité, et jamais d'action** : la radio joue un
    /// flux unique, il n'y a rien à recharger, seulement à redire juste.
    /// `Core::handle_source_update` fusionne champ par champ, donc une trame
    /// muette sur le reste ne touche à rien d'autre — et le son n'est pas
    /// interrompu.
    async fn poll_notification(&mut self) -> Option<Notification> {
        let Some(rx) = &mut self.preset_count_rx else {
            // N'arrive qu'en test (voir le commentaire sur le champ) : `main()`
            // construit toujours ce récepteur. Jamais `None` ici, qui serait
            // terminal pour le SDK.
            return std::future::pending().await;
        };
        match rx.changed().await {
            Ok(()) => {
                let n = *rx.borrow_and_update();
                let mut avis = Notification::new().preset_count(n);
                // La table vient d'être remaniée : retrouver **où est passé** le
                // flux qui joue, et corriger le numéro et le nom affichés.
                //
                // Sans cela, la présélection étant une position, réordonner les
                // stations faisait annoncer le nom d'une autre station pour le
                // flux qui continuait — et un redémarrage reprenait la mauvaise.
                //
                // Aucune action ici, et c'est bien : la radio joue un flux
                // unique, il n'y a rien à recharger, seulement à redire juste.
                if let Some(url) = self.url_en_cours.clone() {
                    let stations = self.stations.read().await;
                    // Station retirée de la table : son numéro ne désigne plus
                    // rien de sûr, et le protocole n'a pas de « plus aucune
                    // présélection ». On se garde alors de mentir davantage en
                    // ne touchant à rien.
                    if let Some(st) = stations.by_url(&url) {
                        let (p, nom) = (st.preset, st.name.clone());
                        drop(stations);
                        if p != self.preset {
                            self.preset = p;
                            // Persister aussi : sinon un redémarrage reprendrait
                            // le numéro d'avant le remaniement, donc une autre
                            // station.
                            if let Err(e) = state::update(&self.state_path, |s| s.preset = p) {
                                tracing::warn!("failed to persist preset: {e}");
                            }
                        }
                        avis = avis.preset(p).preset_name(nom);
                    }
                }
                Some(avis)
            }
            // L'émetteur (moitié Admin) a disparu — ne devrait pas arriver en
            // pratique tant que les deux moitiés partagent le même processus,
            // mais rien ne justifie de traiter ça comme une fin définitive de
            // notifications : on retombe sur la même attente indéfinie que la
            // branche `None` du champ plutôt que de rendre `None` nous-mêmes.
            Err(_) => std::future::pending().await,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

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
        // Rien ne joue encore : renseigné au premier `Play`.
        url_en_cours: None,
        catalog: catalog.clone(),
        locales_root,
        // Le récepteur n'a de sens que si une moitié Admin existe pour
        // émettre dessus (voir plus bas) : sinon `poll_notification` doit
        // rester en attente pour toujours, pas se rabattre sur un canal mort.
        preset_count_rx: Some(preset_count_rx),
    };
    // Annuaire en ligne : la liste intégrée de serveurs, essayés dans l'ordre
    // jusqu'au premier qui répond, ou l'unique serveur épinglé par
    // `RITORNELLO_RADIO_DIRECTORY`. Journalisé au démarrage : sur un Pi sans
    // écran, savoir quels serveurs seront interrogés évite de deviner.
    let directory = directory::HttpDirectory::from_env();
    tracing::info!("radio directory, candidate servers: {}", directory.bases.join(", "));
    let admin = RadioAdmin {
        stations_path,
        state_path,
        stations: stations_shared,
        catalog,
        directory: Arc::new(directory),
        search: RwLock::new(Vec::new()),
        countries: RwLock::new(Vec::new()),
        preset_count_tx,
    };
    Runtime::from_args()?.source(source)?.admin(admin)?.run().await
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
            url_en_cours: None,
            catalog: catalog.clone(),
            locales_root: dir.path().to_path_buf(),
            preset_count_rx: None,
        };
        source.set_locale("fr".into()).await;
        // aucun preset chargé → branche "empty_preset"
        let outcome = source.select(1).await;
        assert_eq!(outcome.status.as_deref(), Some("PRESET VIDE"));
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
            url_en_cours: None,
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
        // Ne rien dire de l'identité : la station n'a pas changé, et annoncer un
        // changement remettrait à zéro les métadonnées du morceau en cours.
        assert!(outcome.identity.is_none());

        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
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
        // Le mot ephemere est declare via `status` : c'est lui qui alimente
        // l'incrustation cote coeur.
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
    async fn poll_notification_ne_touche_ni_a_l_identite_ni_au_son() {
        // Propriete de surete : l'annonce spontanee (enregistrement depuis la
        // page d'admin) ne doit ni couper le flux ni changer ce que les plugins
        // `metadata` croient entendre. Rien ne joue ici, donc pas de
        // presélection a corriger non plus.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(rx);
        tx.send(5).unwrap();

        let n = source.poll_notification().await.expect("notification attendue");
        assert_eq!(n.preset_count, Some(5));
        assert!(n.identity.is_none(), "le morceau en cours ne doit pas bouger");
        assert!(n.preset.is_none(), "rien ne joue : aucun numero a corriger");
    }

    #[tokio::test]
    async fn un_remaniement_de_la_table_corrige_le_numero_de_ce_qui_joue() {
        // Defaut de conception signale : la presélection est une **position**.
        // Reordonner les stations depuis la page faisait pointer le numero
        // memorisé sur une autre station — l'ecran annoncait le mauvais nom pour
        // le flux qui continuait, et un redemarrage reprenait la mauvaise. Le
        // flux est retrouve par son URL, seule chose qui l'identifie durablement.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(rx);
        // La station 1 joue.
        let url = match source.activate().await.action {
            SourceAction::Play { uri, .. } => uri,
            autre => panic!("attendu un Play, recu {autre:?}"),
        };

        // La page remanie la table : ce meme flux passe en presélection 2.
        {
            let mut st = source.stations.write().await;
            for s in st.stations.iter_mut() {
                s.preset = if s.url == url { 2 } else { 1 };
            }
        }
        tx.send(2).unwrap();

        let n = source.poll_notification().await.expect("notification attendue");
        assert_eq!(n.preset, Some(2), "le numero doit suivre la station");
        assert!(n.preset_name.is_some(), "et le nom avec");
        assert_eq!(source.preset, 2, "memorise, pour que suivant/precedent partent de la");
        // Et surtout : aucune action, donc le flux n'est pas coupe pour autant.
        assert!(n.identity.is_none(), "l'identite du flux n'a pas change");
    }

    #[tokio::test]
    async fn une_station_retiree_ne_fait_pas_inventer_de_numero() {
        // Son numero ne designe plus rien de sur, et le protocole n'a pas de
        // « plus aucune presélection » : mieux vaut ne rien dire que designer
        // une station au hasard.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut source = make_source(two_stations(), 1);
        source.preset_count_rx = Some(rx);
        source.activate().await;
        source.stations.write().await.stations.clear();
        tx.send(0).unwrap();

        let n = source.poll_notification().await.expect("notification attendue");
        assert_eq!(n.preset_count, Some(0));
        assert!(n.preset.is_none(), "aucun numero invente");
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
        // Source construite directement, sans passer par `Runtime` (comme le
        // fait ce test), donc sans canal d'annonce de `preset_count` : aucun
        // émetteur n'existe, donc rien ne doit jamais en sortir — surtout pas
        // un `None`, terminal pour le SDK (voir le commentaire sur le champ
        // `preset_count_rx`). `main()`, lui, enregistre toujours la page
        // d'admin et fournit donc toujours ce canal.
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
