//! Source `files` : lit des fichiers audio depuis une racine locale ou un
//! partage réseau monté.
//!
//! mpv tient la liste de lecture : le plugin lui donne un m3u généré et pilote
//! l'index. L'avance automatique passe donc par `playlist-pos`, exactement
//! comme pour un disque, et le plugin n'a rien à cadencer lui-même.
//!
//! Deux moitiés indépendantes, sur le plan du plugin radio : la Source et la
//! page d'admin, chacune dans sa tâche, partageant la table des racines et la
//! liste en cours. Une panne de la page ne doit jamais couper l'audio.

mod admin;
mod state;

use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_files::m3u::Entry;
use ritornello_plugin_files::playlist::Playlist;
use ritornello_plugin_files::roots::Roots;
use ritornello_plugin_files::FILES_EN;
use ritornello_plugin_sdk::{
    run_admin_plugin, run_source_plugin, Notification, SourceOutcome, SourcePlugin,
};
use ritornello_proto::SourceAction;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct FilesSource {
    /// Partagée avec la moitié Admin, qui la modifie depuis la page.
    playlist: Arc<AsyncRwLock<Playlist>>,
    state_path: PathBuf,
    /// Le m3u **généré** que mpv reçoit. Découplé de toute liste utilisateur.
    mpv_playlist_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
    /// Compte de présélections annoncé par la moitié Admin après chaque
    /// modification de la liste.
    ///
    /// `None` en mode dégradé (pas de moitié Admin, faute d'`--admin-socket`) :
    /// `poll_notification` reste alors en attente pour toujours plutôt que de
    /// rendre `None`, qui est **terminal** pour le SDK et journaliserait un
    /// avertissement trompeur pour un déploiement pourtant légitime.
    preset_count_rx: Option<tokio::sync::watch::Receiver<u8>>,
}

impl FilesSource {
    /// Identité de ce qui joue : le fichier, désigné par son chemin absolu.
    ///
    /// Opaque pour le cœur, qui ne fait que la comparer et la relayer. C'est
    /// aussi ce qu'un plugin `metadata` lirait pour reconnaître un morceau.
    fn identite(path: &Path) -> serde_json::Value {
        serde_json::json!({ "kind": "file", "path": path.to_string_lossy() })
    }

    fn mot(&self, cle: &str) -> String {
        self.catalog.read().unwrap().get(cle).to_string()
    }

    /// Statut permanent de la source.
    ///
    /// **Redéclaré à chaque trame utile** : `status` a la convention inverse de
    /// `preset`, l'absence voulant dire « pas de statut » et non « garde le
    /// précédent ». Une Source qui l'omettrait verrait son affichage s'effacer
    /// tout seul à la trame suivante.
    fn statut(&self) -> String {
        self.mot("status_files")
    }

    async fn persiste(&self) {
        let index = self.playlist.read().await.index;
        // `update` et non `save` : la moitié Admin écrit la liste dans ce même
        // fichier, et un `save` reconstruit ici l'effacerait. L'échec est
        // journalisé et non propagé — un `/var/lib` en lecture seule doit
        // coûter la reprise après redémarrage, pas la lecture en cours.
        if let Err(e) = state::update(&self.state_path, |s| s.index = index) {
            tracing::warn!("persisting the current track: {e}");
        }
    }

    /// Lance la liste à l'index courant, après avoir réécrit le m3u de mpv.
    async fn jouer(&mut self) -> SourceOutcome {
        let liste = self.playlist.read().await;
        let count = liste.preset_count();
        let Some(entry) = liste.current().cloned() else {
            return SourceOutcome::new(SourceAction::Noop)
                .status(self.mot("no_playlist"))
                .preset_count(0)
                .plays_nothing();
        };
        if let Err(e) = liste.write_for_mpv(&self.mpv_playlist_path) {
            tracing::warn!("writing the mpv playlist: {e}");
        }
        let index = liste.index;
        let preset = liste.preset();
        drop(liste);

        let action = SourceAction::play(self.mpv_playlist_path.to_string_lossy().to_string())
            // Sans cette déclaration, le cœur chargerait le m3u comme un média
            // unique : mpv ne le déplierait qu'après coup, l'index de départ
            // arriverait hors bornes, et toute sélection de piste rejouerait la
            // première en perdant l'affichage. Mesuré, et corrigé ici.
            .playlist()
            .starting_at(index as i64)
            // Une liste de fichiers a une fin normale : sans cette
            // déclaration, l'inactivité de mpv en fin de liste passerait pour
            // une coupure de flux et la relance rejouerait la liste en boucle.
            .finite();
        let mut issue = SourceOutcome::new(action)
            .plays(Self::identite(&entry.path))
            .preset_name(entry.display_name())
            .preset_count(count)
            .status(self.statut());
        if let Some(n) = preset {
            issue = issue.preset(n);
        }
        issue
    }

    /// Trame qui ne fait que redire où on en est, sans rien relancer.
    async fn recale(&self) -> SourceOutcome {
        let liste = self.playlist.read().await;
        let mut issue = SourceOutcome::new(SourceAction::Noop)
            .preset_count(liste.preset_count())
            .status(self.statut());
        if let Some(entry) = liste.current() {
            issue = issue.plays(Self::identite(&entry.path)).preset_name(entry.display_name());
        }
        if let Some(n) = liste.preset() {
            issue = issue.preset(n);
        }
        issue
    }
}

#[async_trait::async_trait]
impl SourcePlugin for FilesSource {
    async fn activate(&mut self) -> SourceOutcome {
        // L'index est conservé : reprendre après un arrêt rend la piste qu'on
        // écoutait, et non la première.
        //
        // Une version antérieure repartait du début quand la liste s'était
        // terminée, en se fiant au `playlist-pos = -1` de mpv. Mesuré : ce -1
        // arrive **aussi de façon transitoire à chaque rechargement de liste**,
        // donc à chaque changement de piste. La reprise retombait alors sur la
        // piste 1. Le signal n'étant pas fiable, la distinction est abandonnée
        // plutôt que devinée — au prix d'un détail : après une liste allée à son
        // terme, la touche Lecture rejoue la dernière piste.
        self.jouer().await
    }

    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Stop).plays_nothing().status(self.statut())
    }

    async fn select(&mut self, n: u8) -> SourceOutcome {
        if self.playlist.write().await.select(n) {
            self.persiste().await;
            return self.jouer().await;
        }
        // Rien n'a été lancé : la piste précédente joue toujours. Message
        // éphémère, et surtout **aucune déclaration d'identité** — un
        // `plays_nothing()` ici ferait cesser les plugins `metadata` et
        // viderait le titre affiché alors que le son continue.
        let compte = self.playlist.read().await.preset_count();
        SourceOutcome::new(SourceAction::Noop)
            .status(self.mot("empty_track"))
            .transient()
            .preset_count(compte)
    }

    async fn next(&mut self) -> SourceOutcome {
        // mpv marche dans sa propre liste ; c'est lui qui nous dira où il est
        // arrivé, par `player_track`. Rien à recaler ici, sous peine de le
        // faire deux fois et de se contredire.
        SourceOutcome::new(SourceAction::PlayerNext).status(self.statut())
    }

    async fn prev(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::PlayerPrev).status(self.statut())
    }

    async fn eject(&mut self) -> SourceOutcome {
        // Rien à éjecter : pas de support amovible ici.
        SourceOutcome::new(SourceAction::Noop).status(self.statut())
    }

    async fn player_track(&mut self, n: i64) -> SourceOutcome {
        if !self.playlist.write().await.set_index(n) {
            // mpv dit `-1` en fin de liste — **et aussi transitoirement à chaque
            // rechargement de liste**, donc à chaque changement de piste : c'est
            // mesuré, et c'est pourquoi on n'en tire aucune conclusion. Ne rien
            // déclarer ; l'arrêt éventuel sera annoncé par `stop()`.
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.persiste().await;
        self.recale().await
    }

    async fn stop(&mut self) -> SourceOutcome {
        // Le cœur a arrêté de sa propre initiative, ou la liste s'est terminée.
        // Le dire, sinon la dernière piste et ses métadonnées resteraient
        // affichées indéfiniment.
        //
        // Et **dire lequel des trois**, ce qui n'était pas le cas : cette trame
        // écrasait le « AUCUNE LISTE » que `jouer()` venait d'afficher. Sans
        // piste, mpv reste inactif, le cœur envoie donc `stop()` aussitôt, et
        // l'utilisateur ne voyait qu'un statut générique sans jamais apprendre
        // que sa liste était vide.
        let vide = self.playlist.read().await.entries.is_empty();
        let mot = if vide { self.mot("no_playlist") } else { self.statut() };
        SourceOutcome::new(SourceAction::Noop).plays_nothing().status(mot)
    }

    async fn set_locale(&mut self, locale: String) {
        *self.catalog.write().unwrap() =
            Catalog::load("files", &locale, &self.locales_root, FILES_EN);
    }

    async fn poll_notification(&mut self) -> Option<Notification> {
        let Some(rx) = &mut self.preset_count_rx else {
            // Mode dégradé (pas de moitié Admin) : voir le commentaire sur le
            // champ. Jamais `None` ici, qui serait terminal pour le SDK.
            return std::future::pending().await;
        };
        match rx.changed().await {
            Ok(()) => {
                let n = *rx.borrow_and_update();
                // Uniquement le compte : ne rien dire de l'identité, de la
                // présélection ni du statut, pour ne déranger ni l'affichage ni
                // ce qui joue. Modifier la liste depuis la page ne doit pas
                // interrompre la piste en cours.
                Some(Notification::new().preset_count(n))
            }
            // L'émetteur a disparu (moitié Admin terminée) : plus rien à
            // annoncer, mais la Source continue de jouer.
            Err(_) => std::future::pending().await,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = ritornello_plugin_sdk::socket_path();
    // `--admin-socket` n'est fourni par le cœur que si `admin = true` dans
    // plugins.toml. Absent, on continue en mode dégradé : la moitié Source
    // tourne seule, sans page de gestion.
    let admin_socket = ritornello_plugin_sdk::admin_socket_path();
    if admin_socket.is_none() {
        tracing::warn!(
            "no --admin-socket: the management page will not be served, only the Source half runs (add 'admin = true' to plugins.toml)"
        );
    }

    let state_path =
        PathBuf::from(env_or("RITORNELLO_FILES_STATE", "/var/lib/ritornello/plugin-files.json"));
    let mpv_playlist_path = PathBuf::from(env_or(
        "RITORNELLO_FILES_MPV_PLAYLIST",
        "/var/lib/ritornello/plugin-files.m3u",
    ));
    let roots_path =
        PathBuf::from(env_or("RITORNELLO_FILES_ROOTS", "/etc/ritornello/media-roots.toml"));
    let creds_dir = PathBuf::from(env_or(
        "RITORNELLO_FILES_CREDENTIALS",
        "/etc/ritornello/media-credentials",
    ));
    let playlists_dir =
        PathBuf::from(env_or("RITORNELLO_FILES_PLAYLISTS", "/var/lib/ritornello/playlists"));
    // Répertoire de travail transitoire, où l'assistant réseau pose son fichier
    // d'authentification le temps d'un appel à `smbclient`.
    //
    // Le **répertoire d'exécution**, et surtout pas celui des identifiants
    // persistés : celui-là vit sous `/etc` et n'est inscriptible qu'en
    // production. Le confondre faisait échouer l'assistant en développement
    // avec un « Permission denied » qui semblait accuser SMB.
    //
    // Même défaut et même variable que le cœur (`RITORNELLO_RUNTIME_DIR`), pour
    // que `docs/development.md` reste vrai d'un binaire à l'autre.
    let runtime_dir = PathBuf::from(env_or("RITORNELLO_RUNTIME_DIR", "/run/ritornello"));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));

    let etat = state::load(&state_path);
    let entries: Vec<Entry> = etat.playlist.iter().map(Entry::from).collect();
    // Les pistes absentes sont journalisées mais **conservées** : un partage
    // momentanément injoignable (NAS endormi, montage pas encore fait au boot)
    // effacerait sinon la liste de l'utilisateur.
    let manquantes = entries.iter().filter(|e| !e.path.is_file()).count();
    if manquantes > 0 {
        tracing::warn!(
            "{manquantes} of {} tracks are missing at startup: the share may not be mounted yet",
            entries.len()
        );
    }
    let index = if etat.index < entries.len() { etat.index } else { 0 };

    let roots = Roots::load(&roots_path).unwrap_or_else(|e| {
        tracing::warn!("no usable media-roots.toml ({e}): starting with no root");
        Roots::default()
    });
    let catalog = Arc::new(RwLock::new(Catalog::load("files", "en", &locales_root, FILES_EN)));
    let playlist = Arc::new(AsyncRwLock::new(Playlist { entries, index }));
    let roots = Arc::new(AsyncRwLock::new(roots));
    let (preset_count_tx, preset_count_rx) =
        tokio::sync::watch::channel(playlist.read().await.preset_count());

    let source = FilesSource {
        playlist: playlist.clone(),
        state_path: state_path.clone(),
        mpv_playlist_path,
        catalog: catalog.clone(),
        locales_root,
        preset_count_rx: admin_socket.as_ref().map(|_| preset_count_rx),
    };

    // Sonde au démarrage plutôt qu'à l'usage : la page doit pouvoir griser
    // l'assistant réseau dès son ouverture, comme l'onglet Système grise le
    // redémarrage sur `can_reboot`. La sonde est refaite à chaque tentative de
    // connexion, pour qu'installer le paquet sans redémarrer donne un résultat
    // juste.
    let smb_ok = Arc::new(std::sync::atomic::AtomicBool::new(
        ritornello_plugin_files::smb::available().await,
    ));
    if !smb_ok.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("smbclient is not available: the network wizard will be offered read-only");
    }

    let admin = admin_socket.map(|socket| {
        (
            admin::FilesAdmin {
                explore: ritornello_plugin_files::explore::Explorateur::new(
                    runtime_dir.clone(),
                    catalog.clone(),
                    smb_ok.clone(),
                ),
                mount_error: Arc::new(Mutex::new(None)),
                smb_ok,
                roots_path,
                creds_dir,
                internal_playlists: playlists_dir,
                state_path,
                roots,
                playlist,
                catalog,
                scan: Arc::new(Mutex::new(admin::ScanProgress::default())),
                scan_task: None,
                unresolved: Arc::new(Mutex::new(Vec::new())),
                browse: Arc::new(Mutex::new(serde_json::json!({}))),
                preset_count_tx,
            },
            socket,
        )
    });

    // Les deux moitiés sont indépendantes : une panne sur la socket admin ne
    // doit pas tuer la lecture audio, et réciproquement. Chacune dans sa propre
    // tâche, pour qu'une panique y soit capturée dans le JoinHandle au lieu de
    // dérouler la pile de l'autre.
    let source_handle = tokio::spawn(async move { run_source_plugin(source, &socket_path).await });
    match admin {
        Some((admin, socket)) => {
            let admin_handle = tokio::spawn(async move { run_admin_plugin(admin, &socket).await });
            let (source_res, admin_res) = tokio::join!(source_handle, admin_handle);
            log_half("source half", source_res);
            log_half("admin half", admin_res);
        }
        None => log_half("source half", source_handle.await),
    }
    Ok(())
}

/// Journalise le résultat d'une des deux moitiés (succès, erreur applicative,
/// panique) sans jamais faire remonter l'échec de l'une sur l'autre.
fn log_half(label: &str, res: std::result::Result<Result<()>, tokio::task::JoinError>) {
    match res {
        Ok(Ok(())) => tracing::warn!("files plugin ({label}) ended normally"),
        Ok(Err(e)) => tracing::warn!("files plugin ({label}) error: {e}"),
        Err(join_err) => tracing::error!("files plugin ({label}) panicked: {join_err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::IdentityUpdate;

    fn source_de_test(playlist: Playlist) -> FilesSource {
        let dir = tempfile::tempdir().unwrap();
        let racine = dir.path().to_path_buf();
        // Le tempdir est volontairement fuité : la Source vit le temps du test,
        // et le laisser tomber effacerait les chemins qu'elle écrit.
        std::mem::forget(dir);
        FilesSource {
            playlist: Arc::new(AsyncRwLock::new(playlist)),
            state_path: racine.join("plugin-files.json"),
            mpv_playlist_path: racine.join("plugin-files.m3u"),
            catalog: Arc::new(RwLock::new(Catalog::load("files", "en", &racine, FILES_EN))),
            locales_root: racine,
            preset_count_rx: None,
        }
    }

    fn liste_de(n: usize) -> Playlist {
        Playlist {
            entries: (1..=n)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/musique/{i:02}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect(),
            index: 0,
        }
    }

    #[tokio::test]
    async fn activer_une_liste_vide_ne_lance_rien_et_le_dit() {
        let mut s = source_de_test(Playlist::default());
        let out = s.activate().await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert_eq!(out.preset_count, Some(0));
        assert!(out.status.is_some(), "le statut doit dire pourquoi rien ne joue");
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn activer_reprend_a_la_piste_memorisee() {
        // La reprise après redémarrage : sans `start`, la lecture repartirait à
        // la première piste à chaque démarrage de l'appareil.
        let mut p = liste_de(5);
        p.index = 3;
        let mut s = source_de_test(p);
        let out = s.activate().await;
        match out.action {
            SourceAction::Play { start, finite, .. } => {
                assert_eq!(start, Some(3));
                assert!(finite, "une liste de fichiers a une fin normale");
            }
            autre => panic!("attendu un Play, obtenu {autre:?}"),
        }
        assert_eq!(out.preset, Some(4));
        assert_eq!(out.preset_count, Some(5));
        assert!(out.preset_name.is_some(), "l'ecran ne doit jamais etre muet");
    }

    #[tokio::test]
    async fn une_piste_inexistante_donne_un_message_ephemere_sans_couper_la_lecture() {
        // Même règle que la présélection vide de la radio : rien n'a été lancé,
        // donc la piste précédente joue toujours et doit reparaître à l'écran.
        // Surtout : aucune déclaration d'identité, sans quoi les métadonnées du
        // morceau en cours seraient effacées.
        let mut s = source_de_test(liste_de(3));
        let out = s.select(9).await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert!(out.transient, "le message doit s'effacer de lui-meme");
        assert!(out.identity.is_none(), "declarer un arret serait faux");
        assert_eq!(out.preset_count, Some(3));
    }

    #[tokio::test]
    async fn le_statut_est_redeclare_a_chaque_trame() {
        // PIÈGE : `status` a la convention INVERSE de `preset`. Absent veut dire
        // « pas de statut », et non « garde le précédent » : une Source qui
        // l'omettrait verrait son affichage s'effacer tout seul.
        let mut s = source_de_test(liste_de(3));
        for (nom, out) in [
            ("activate", s.activate().await),
            ("select", s.select(2).await),
            ("next", s.next().await),
            ("prev", s.prev().await),
            ("stop", s.stop().await),
        ] {
            assert!(out.status.is_some(), "statut omis sur {nom} : l'ecran s'effacerait");
        }
    }

    #[tokio::test]
    async fn l_avance_automatique_recale_index_identite_et_nom() {
        // Chemin réel : mpv passe à la piste suivante seul, le cœur relaie
        // `PlayerTrack(n)`, et seule la Source sait ce que « piste n » désigne.
        let mut s = source_de_test(liste_de(5));
        let out = s.player_track(2).await;
        assert_eq!(out.preset, Some(3));
        assert!(out.preset_name.is_some());
        assert_eq!(
            out.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({
                "kind": "file", "path": "/musique/03.mp3"
            })))
        );
    }

    #[tokio::test]
    async fn un_index_negatif_est_ecarte_sans_rien_declarer() {
        // mpv dit -1 en fin de liste. Le cœur le transmet tel quel ; la Source
        // l'écarte, et surtout ne déclare rien — l'arrêt viendra de `stop()`.
        let mut s = source_de_test(liste_de(3));
        let out = s.player_track(-1).await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert!(out.identity.is_none());
    }

    #[tokio::test]
    async fn la_fin_de_liste_declare_que_plus_rien_ne_joue() {
        let mut s = source_de_test(liste_de(3));
        let out = s.stop().await;
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn next_et_prev_delegent_a_mpv_sans_recaler_deux_fois() {
        // Recaler ici en plus de `player_track` ferait deux corrections pour un
        // seul changement, et la seconde pourrait contredire la première.
        let mut s = source_de_test(liste_de(3));
        assert_eq!(s.next().await.action, SourceAction::PlayerNext);
        assert_eq!(s.prev().await.action, SourceAction::PlayerPrev);
        assert_eq!(s.playlist.read().await.index, 0, "l'index ne doit pas avoir bouge de lui-meme");
    }

    #[tokio::test]
    async fn selectionner_persiste_la_piste() {
        let mut s = source_de_test(liste_de(4));
        s.select(3).await;
        assert_eq!(state::load(&s.state_path).index, 2);
    }

    #[tokio::test]
    async fn la_moitie_admin_annonce_le_compte_sans_deranger_la_lecture() {
        // Modifier la liste depuis la page doit mettre à jour la grille de la
        // télécommande web tout de suite, sans attendre qu'une piste soit
        // jouée — et sans rien dire de l'identité ni du statut, sous peine
        // d'interrompre ce qui joue.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut s = source_de_test(liste_de(3));
        s.preset_count_rx = Some(rx);
        tx.send(7).unwrap();
        let n = s.poll_notification().await.expect("une notification attendue");
        assert_eq!(n.preset_count, Some(7));
        assert!(n.identity.is_none());
        assert!(n.status.is_none());
        assert!(n.preset.is_none());
    }

    #[tokio::test]
    async fn le_statut_suit_le_catalogue_apres_set_locale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        std::fs::write(dir.path().join("files/fr.toml"), "status_files = \"FICHIERS\"\n").unwrap();
        let mut s = source_de_test(liste_de(2));
        s.locales_root = dir.path().to_path_buf();
        s.set_locale("fr".into()).await;
        assert_eq!(s.activate().await.status.as_deref(), Some("FICHIERS"));
    }

    #[tokio::test]
    async fn un_arret_sur_liste_vide_dit_que_la_liste_est_vide() {
        // Défaut signalé, et il était mesquin : `jouer()` affichait bien
        // « AUCUNE LISTE », mais sans piste mpv reste inactif, le cœur envoyait
        // donc `stop()` aussitôt — et cette trame écrasait le message par un
        // statut générique. L'utilisateur ne pouvait pas apprendre que sa liste
        // était vide.
        let mut s = source_de_test(Playlist::default());
        assert_eq!(s.activate().await.status.as_deref(), Some("NO PLAYLIST"));
        assert_eq!(s.stop().await.status.as_deref(), Some("NO PLAYLIST"));
    }

    #[tokio::test]
    async fn un_arret_sur_une_liste_pleine_reste_un_arret_ordinaire() {
        // Garde-fou du test précédent : « aucune liste » doit rester réservé au
        // cas où il n'y a vraiment rien à jouer.
        let mut s = source_de_test(liste_de(3));
        s.activate().await;
        assert_eq!(s.stop().await.status.as_deref(), Some("FILES"));
    }

    #[tokio::test]
    async fn un_playlist_pos_negatif_ne_deplace_pas_l_index() {
        // mpv annonce `-1` en fin de liste **et transitoirement à chaque
        // rechargement**, donc à chaque changement de piste : c'est mesuré. En
        // tirer une conclusion — « la liste est terminée, repartons du début » —
        // faisait retomber toute reprise sur la piste 1.
        let mut s = source_de_test(liste_de(4));
        s.select(3).await;
        s.player_track(-1).await;
        assert_eq!(s.activate().await.preset, Some(3), "le -1 ne doit rien conclure");
    }

    #[tokio::test]
    async fn une_liste_est_declaree_comme_telle_au_coeur() {
        // Le défaut central : sans `playlist`, le cœur chargeait le m3u par
        // `loadfile`, que mpv ne déplie qu'après coup — l'index de départ
        // arrivait hors bornes et toute sélection rejouait la première piste.
        let mut s = source_de_test(liste_de(3));
        match s.select(2).await.action {
            SourceAction::Play { playlist, start, finite, .. } => {
                assert!(playlist, "un m3u doit etre charge comme une liste");
                assert_eq!(start, Some(1), "piste 2 = index 1");
                assert!(finite, "une liste de fichiers a une fin normale");
            }
            autre => panic!("attendu un Play, recu {autre:?}"),
        }
    }

    #[tokio::test]
    async fn reprendre_apres_un_arret_rend_la_piste_ecoutee() {
        // La touche Lecture après un Stop redemande `activate()`. Elle doit
        // rendre la piste qu'on écoutait — l'index vit dans le plugin et aucun
        // arrêt ne le déplace — et non repartir de la première.
        let mut s = source_de_test(liste_de(4));
        s.select(3).await;
        s.stop().await;
        assert_eq!(s.activate().await.preset, Some(3), "la piste ecoutee, pas la premiere");
    }

    #[test]
    fn en_embarque_files_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(FILES_EN).unwrap().is_empty());
    }
}
