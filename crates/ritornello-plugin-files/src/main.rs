//! Source `files` : lit des fichiers audio depuis une racine locale ou un
//! partage réseau monté.
//!
//! mpv tient la liste de lecture : le plugin lui donne un m3u généré et pilote
//! l'index. L'avance automatique passe donc par `playlist-pos`, exactement
//! comme pour un disque, et le plugin n'a rien à cadencer lui-même.

mod state;

use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_files::m3u::Entry;
use ritornello_plugin_files::playlist::Playlist;
use ritornello_plugin_files::FILES_EN;
use ritornello_plugin_sdk::{run_source_plugin, SourceOutcome, SourcePlugin};
use ritornello_proto::SourceAction;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct FilesSource {
    playlist: Playlist,
    state_path: PathBuf,
    /// Le m3u **généré** que mpv reçoit. Découplé de toute liste utilisateur.
    mpv_playlist_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
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

    fn persiste(&self) {
        // L'échec est journalisé et non propagé : un /var/lib en lecture seule
        // doit coûter la reprise après redémarrage, pas la lecture en cours.
        if let Err(e) = state::update(&self.state_path, |s| s.index = self.playlist.index) {
            tracing::warn!("persisting the current track: {e}");
        }
    }

    /// Lance la liste à l'index courant, après avoir réécrit le m3u de mpv.
    fn jouer(&mut self) -> SourceOutcome {
        let count = self.playlist.preset_count();
        let Some(entry) = self.playlist.current().cloned() else {
            return SourceOutcome::new(SourceAction::Noop)
                .status(self.mot("no_playlist"))
                .preset_count(0)
                .plays_nothing();
        };
        if let Err(e) = self.playlist.write_for_mpv(&self.mpv_playlist_path) {
            tracing::warn!("writing the mpv playlist: {e}");
        }
        let action = SourceAction::play(self.mpv_playlist_path.to_string_lossy().to_string())
            .starting_at(self.playlist.index as i64)
            // Une liste de fichiers a une fin normale : sans cette
            // déclaration, l'inactivité de mpv en fin de liste passerait pour
            // une coupure de flux et la relance rejouerait la liste en boucle.
            .finite();
        let mut issue = SourceOutcome::new(action)
            .plays(Self::identite(&entry.path))
            .preset_name(entry.display_name())
            .preset_count(count)
            .status(self.statut());
        if let Some(n) = self.playlist.preset() {
            issue = issue.preset(n);
        }
        issue
    }

    /// Trame qui ne fait que redire où on en est, sans rien relancer.
    fn recale(&self) -> SourceOutcome {
        let mut issue = SourceOutcome::new(SourceAction::Noop)
            .preset_count(self.playlist.preset_count())
            .status(self.statut());
        if let Some(entry) = self.playlist.current() {
            issue = issue.plays(Self::identite(&entry.path)).preset_name(entry.display_name());
        }
        if let Some(n) = self.playlist.preset() {
            issue = issue.preset(n);
        }
        issue
    }
}

#[async_trait::async_trait]
impl SourcePlugin for FilesSource {
    async fn activate(&mut self) -> SourceOutcome {
        self.jouer()
    }

    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Stop).plays_nothing().status(self.statut())
    }

    async fn select(&mut self, n: u8) -> SourceOutcome {
        if self.playlist.select(n) {
            self.persiste();
            return self.jouer();
        }
        // Rien n'a été lancé : la piste précédente joue toujours. Message
        // éphémère, et surtout **aucune déclaration d'identité** — un
        // `plays_nothing()` ici ferait cesser les plugins `metadata` et
        // viderait le titre affiché alors que le son continue.
        SourceOutcome::new(SourceAction::Noop)
            .status(self.mot("empty_track"))
            .transient()
            .preset_count(self.playlist.preset_count())
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
        if !self.playlist.set_index(n) {
            // mpv dit `-1` en fin de liste : ne rien déclarer, l'arrêt sera
            // annoncé par `stop()` que le cœur envoie juste après.
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.persiste();
        self.recale()
    }

    async fn stop(&mut self) -> SourceOutcome {
        // Le cœur a arrêté de sa propre initiative, ou la liste s'est terminée.
        // Le dire, sinon la dernière piste et ses métadonnées resteraient
        // affichées indéfiniment.
        SourceOutcome::new(SourceAction::Noop).plays_nothing().status(self.statut())
    }

    async fn set_locale(&mut self, locale: String) {
        *self.catalog.write().unwrap() =
            Catalog::load("files", &locale, &self.locales_root, FILES_EN);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = ritornello_plugin_sdk::socket_path();
    let state_path =
        PathBuf::from(env_or("RITORNELLO_FILES_STATE", "/var/lib/ritornello/plugin-files.json"));
    let mpv_playlist_path =
        PathBuf::from(env_or("RITORNELLO_FILES_MPV_PLAYLIST", "/var/lib/ritornello/plugin-files.m3u"));
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

    let catalog = Arc::new(RwLock::new(Catalog::load("files", "en", &locales_root, FILES_EN)));
    let source = FilesSource {
        playlist: Playlist { entries, index },
        state_path,
        mpv_playlist_path,
        catalog,
        locales_root,
    };

    run_source_plugin(source, &socket_path).await
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
            playlist,
            state_path: racine.join("plugin-files.json"),
            mpv_playlist_path: racine.join("plugin-files.m3u"),
            catalog: Arc::new(RwLock::new(Catalog::load("files", "en", &racine, FILES_EN))),
            locales_root: racine,
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
        assert_eq!(s.playlist.index, 0, "l'index ne doit pas avoir bouge de lui-meme");
    }

    #[tokio::test]
    async fn selectionner_persiste_la_piste() {
        let mut s = source_de_test(liste_de(4));
        s.select(3).await;
        assert_eq!(state::load(&s.state_path).index, 2);
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

    #[test]
    fn en_embarque_files_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(FILES_EN).unwrap().is_empty());
    }
}
