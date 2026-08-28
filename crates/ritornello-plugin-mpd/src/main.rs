mod admin;
mod commands;
mod config;
mod state;
// Uniquement compilé sous `cargo test` : `ui_placeholder_js` ne sert au
// run-time nulle part dans ce crate, seulement à `build.rs` (compilation
// séparée, via `include!`) et à ses propres tests. Le compiler en continu
// dans le binaire déclencherait un `dead_code` que `-D warnings` refuserait
// (voir `generic-input/src/main.rs`, même piège).
#[cfg(test)]
mod placeholder;
mod protocol;
mod session;

use admin::MpdAdmin;
use anyhow::Result;
use config::Config;
use state::SharedState;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{DisplayPlugin, InputPlugin, Runtime};
use ritornello_proto::{SourcesCatalog, Cover, InputMessage, PlayerState};
use session::listen;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

pub(crate) const MPD_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, defaut: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| defaut.to_string())
}

/// Moitié `display` : reçoit chaque trame du cœur et la dépose dans l'état
/// partagé, lu par les sessions clientes.
struct MpdDisplay {
    state: Arc<SharedState>,
}

#[async_trait::async_trait]
impl DisplayPlugin for MpdDisplay {
    async fn show(&mut self, state: PlayerState) -> Result<()> {
        self.state.apply_state(state).await;
        Ok(())
    }

    /// Le sources_catalog, sur son propre canal : c'est de lui que viennent les
    /// listes enregistrées (`listplaylists`) et les vrais names de la file
    /// d'attente.
    ///
    /// Le corps par défaut du trait ignore cette trame ; ce greffon est le
    /// premier afficheur à s'y intéresser. Un seul appel au démarrage, puis un
    /// par changement réel — pas un par trame d'état, et c'est tout le sens des
    /// deux canaux (voir `SharedState::apply_catalog`).
    async fn sources_catalog(&mut self, c: SourcesCatalog) -> Result<()> {
        self.state.apply_catalog(c).await;
        Ok(())
    }

    /// **C'est cette line qui allume la fonction pochettes de tout
    /// l'appareil.** Le cœur ne push_cover les bytes qu'aux afficheurs qui les
    /// demandent, l'announcement est dérivée de cette méthode (voir
    /// `Runtime::display`), et ce greffon est le seul du dépôt à la redéfinir :
    /// la console garde le corps par défaut et ne reçoit donc jamais de
    /// mégaoctet qu'elle jetterait.
    ///
    /// Redéfinie parce qu'il en a un usage réel et non parce qu'il *peut* :
    /// `albumart` et `readpicture` doivent rendre des bytes, et aucune autre
    /// voie ne les lui donne — le `cover_href` de la trame d'état est une URL
    /// du serveur HTTP du cœur, que le greffon n'a ni le droit ni le moyen
    /// d'aller read.
    fn wants_covers(&self) -> bool {
        true
    }

    /// La cover de ce qui plays. Déposée dans l'état partagé, d'où les
    /// sessions la servent par tranches.
    async fn cover(&mut self, c: Cover) -> Result<()> {
        self.state.apply_cover(c).await;
        Ok(())
    }
}

/// Moitié `input` : dépile le canal alimenté par les sessions clientes.
struct MpdInput {
    rx: mpsc::Receiver<InputMessage>,
}

#[async_trait::async_trait]
impl InputPlugin for MpdInput {
    async fn next_command(&mut self) -> Result<InputMessage> {
        // Tant qu'`accepter` tourne (sa boucle est infinie, voir
        // `session.rs`), il détient un clone de l'émetteur, donc ce `recv()`
        // remainder en attente indéfiniment plutôt que de rendre `None` — même
        // contrat que `EvdevInput::next_command` côté `generic-input`, où
        // l'oubli de cette propriété avait fait sortir le greffon en `exit 0`
        // dès le démarrage (régression du 2026-07-27).
        self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("no mpd session sends commands yet"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let path = PathBuf::from(env_or("RITORNELLO_MPD_CONFIG", "/etc/ritornello/mpd.toml"));
    let config = Config::load(&path);

    // **Lié avant l'announcement.** C'est la même doctrine que le SDK tient pour ses
    // sockets Unix — lier d'abord, annoncer ensuite — et elle donne ici un
    // comportement utile : un port 6600 déjà pris fait échouer le greffon (le
    // `?` sort de `main` avant même de construire un `Runtime`) sans qu'il
    // s'announcement, donc le cœur le rapporte mort avant announcement et la page de
    // statut le montre. Sinon un port occupé se devinerait dans les logs.
    let listener = TcpListener::bind((config.listen.as_str(), config.port)).await?;
    tracing::info!("mpd server listening on {}:{}", config.listen, config.port);

    let state = Arc::new(SharedState::default());
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    // Le canal par lequel la page d'admin fait se relier la moitié réseau, sans
    // redémarrage du greffon (voir `session::listen`). Un `watch` et non un
    // `mpsc` : seule la **dernière** configuration compte, et deux
    // enregistrements coup sur coup ne doivent pas provoquer deux reliaisons
    // dont la première serait déjà périmée.
    let (rebind_tx, rebind_rx) = tokio::sync::watch::channel(config.clone());
    tokio::spawn(listen(listener, rebind_rx, state.clone(), cmd_tx));

    // Un greffon Display/Input ne reçoit pas de `SetLocale` (le protocol ne
    // le prévoit que pour les sources) : la langue de la page vient de
    // l'environnement, comme en generic-input.
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let locale = env_or("RITORNELLO_LOCALE", "en");
    let catalog = Arc::new(RwLock::new(Catalog::load("mpd", &locale, &locales_root, MPD_EN)));
    let admin = MpdAdmin {
        config_path: path,
        config: RwLock::new(config),
        catalog,
        rebind_tx: Some(rebind_tx),
    };

    Runtime::from_args()?
        .input(MpdInput { rx: cmd_rx })?
        .display(MpdDisplay { state })?
        .admin(admin)?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_embarque_mpd_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(MPD_EN).unwrap().is_empty());
    }

    #[tokio::test]
    async fn afficheur_mpd_depose_letat_recu_dans_letat_partage() {
        let state = Arc::new(SharedState::default());
        let mut afficheur = MpdDisplay { state: state.clone() };
        let envoye = PlayerState { volume: 17, ..Default::default() };
        afficheur.show(envoye.clone()).await.unwrap();
        assert_eq!(state.read().await.state, envoye);
    }

    #[tokio::test]
    async fn afficheur_mpd_depose_le_catalogue_recu_dans_letat_partage() {
        // Le seul path par lequel le sources_catalog entre dans le greffon : sans
        // cette surcharge, le corps par défaut du trait l'ignorerait en
        // silence et `listplaylists` resterait clear pour toujours.
        let state = Arc::new(SharedState::default());
        let mut afficheur = MpdDisplay { state: state.clone() };
        let envoye = SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog {
                name: "radio".into(),
                presets: vec![ritornello_proto::Preset { index: 5, name: "Nova".into() }],
            }],
        };

        afficheur.sources_catalog(envoye.clone()).await.unwrap();

        assert_eq!(state.read().await.sources_catalog, envoye);
    }

    #[tokio::test]
    async fn une_trame_detat_ne_touche_pas_au_catalogue_deja_recu() {
        // Les deux moitiés d'un même afficheur écrivent dans le même état
        // partagé, et chacune ne doit toucher que le sien : un `show` qui
        // remettrait l'instantané à neuf effacerait le sources_catalog reçu au
        // démarrage, et plus rien ne le renverrait.
        let state = Arc::new(SharedState::default());
        let mut afficheur = MpdDisplay { state: state.clone() };
        let sources_catalog = SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog { name: "radio".into(), presets: vec![] }],
        };
        afficheur.sources_catalog(sources_catalog.clone()).await.unwrap();

        afficheur.show(PlayerState { source: "radio".into(), volume: 17, ..Default::default() }).await.unwrap();

        let inst = state.read().await;
        assert_eq!(inst.sources_catalog, sources_catalog);
        assert_eq!(inst.state.volume, 17);
    }

    #[test]
    fn lafficheur_mpd_demande_les_pochettes() {
        // **L'opt-in, épinglé.** C'est cette valeur qui allume la fonction pour
        // tout l'appareil : le cœur dérive l'announcement de `wants_covers` (voir
        // `Runtime::display`), et personne d'autre ne la redéfinit. Sans ce
        // test, la put_back au corps par défaut ne casserait *aucun* autre
        // test du greffon — les tests de session poussent la cover dans
        // l'état partagé directement — et `albumart` répondrait `ACK 50` sur
        // l'appareil réel sans que rien ne le signale.
        let afficheur = MpdDisplay { state: Arc::new(SharedState::default()) };
        assert!(afficheur.wants_covers(), "le serveur MPD doit recevoir les bytes");
    }

    #[tokio::test]
    async fn afficheur_mpd_depose_la_pochette_recue_dans_letat_partage() {
        // Le seul path par lequel une image entre dans le greffon. Sans cette
        // surcharge, le corps par défaut du trait l'avalerait en silence et
        // `albumart` ne répondrait jamais rien — un greffon qui *demande* les
        // pochettes et les jette est exactement ce que le cœur ne peut pas
        // distinguer tout seul.
        let state = Arc::new(SharedState::default());
        let mut afficheur = MpdDisplay { state: state.clone() };
        let envoyee = Cover {
            href: "/api/cover/1a2b3c".into(),
            mime: "image/jpeg".into(),
            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
        };

        afficheur.cover(envoyee.clone()).await.unwrap();

        let tenue = state.read().await.cover.expect("la cover doit etre tenue");
        assert_eq!(tenue.href, envoyee.href);
        assert_eq!(tenue.mime, envoyee.mime);
        assert_eq!(*tenue.bytes, envoyee.bytes);
    }

    #[tokio::test(start_paused = true)]
    async fn la_moitie_input_attend_tant_quun_emetteur_du_canal_vit() {
        // Régression visée : celle documentée sur `EvdevInput` côté
        // `generic-input` (2026-07-27), où l'oubli de cette propriété faisait
        // sortir le greffon en exit 0 dès le démarrage faute de périphérique
        // ouvert. Ici l'émetteur vivant tant qu'`accepter` tourne, ce
        // `next_command` ne doit jamais se terminer tout seul.
        //
        // Clock simulée (`start_paused`) : sans émetteur qui envoie ni
        // aucun autre minuteur en jeu, tokio avance le temps virtuel jusqu'à
        // l'échéance du `sleep` dès que tout le remainder est en attente — la
        // propriété testée ne dépend donc pas de deviner combien de temps
        // "assez longtemps" représente sur la machine qui exécute le test.
        let (tx, rx) = mpsc::channel::<InputMessage>(4);
        let mut entree = MpdInput { rx };
        tokio::select! {
            _ = entree.next_command() => panic!("next_command ne doit pas se terminer tant qu'un emetteur vit"),
            _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {}
        }
        // Émetteur lâché : fin propre, l'erreur nomme la cause.
        drop(tx);
        let e = entree.next_command().await.unwrap_err();
        assert!(e.to_string().contains("mpd session"), "erreur inattendue: {e}");
    }
}
