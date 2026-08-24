mod config;
mod etat;

use anyhow::Result;
use config::Config;
use etat::EtatPartage;
use ritornello_plugin_sdk::{DisplayPlugin, InputPlugin, Runtime};
use ritornello_proto::{InputMessage, PlayerState};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

fn env_ou(cle: &str, defaut: &str) -> String {
    std::env::var(cle).unwrap_or_else(|_| defaut.to_string())
}

/// Moitié `display` : reçoit chaque trame du cœur et la dépose dans l'état
/// partagé, lu par les sessions clientes (Task 8).
struct AfficheurMpd {
    etat: Arc<EtatPartage>,
}

#[async_trait::async_trait]
impl DisplayPlugin for AfficheurMpd {
    async fn show(&mut self, state: PlayerState) -> Result<()> {
        self.etat.appliquer_etat(state).await;
        Ok(())
    }
}

/// Moitié `input` : dépile le canal alimenté par les sessions clientes.
/// Pour cette tâche personne n'y écrit encore — `accepter` ferme aussitôt
/// chaque connexion — donc `rx` ne reçoit rien tant que la Task 8 n'a pas
/// câblé de vraie session. Il ne se termine pas pour autant : voir le
/// commentaire sur `next_command`.
struct EntreeMpd {
    rx: mpsc::Receiver<InputMessage>,
}

#[async_trait::async_trait]
impl InputPlugin for EntreeMpd {
    async fn next_command(&mut self) -> Result<InputMessage> {
        // Tant qu'`accepter` tourne (boucle infinie, voir plus bas), il
        // détient un clone de l'émetteur, donc ce `recv()` reste en attente
        // indéfiniment plutôt que de rendre `None` — même contrat que
        // `EvdevInput::next_command` côté `generic-input`, où l'oubli de
        // cette propriété avait fait sortir le greffon en `exit 0` dès le
        // démarrage (régression du 2026-07-27).
        self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("no mpd session sends commands yet"))
    }
}

/// Accepte les connexions TCP et les ferme aussitôt.
///
/// **Stub de la Task 3.** La Task 8 remplace ce corps par la vraie session
/// MPD (lecture de lignes, réponses, écriture sur `cmd_tx`), déplacée dans
/// `session.rs`. Ici, personne ne répond encore à rien : accepter puis
/// fermer évite qu'un client MPD reste bloqué en écriture sur un port ouvert
/// qui ne le lirait jamais.
async fn accepter(ecoute: TcpListener, _etat: Arc<EtatPartage>, _cmd_tx: mpsc::Sender<InputMessage>) {
    loop {
        match ecoute.accept().await {
            Ok((flux, adresse)) => {
                tracing::info!("mpd client connected from {adresse}");
                drop(flux);
            }
            Err(e) => {
                tracing::warn!("mpd accept failed: {e}");
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let chemin = PathBuf::from(env_ou("RITORNELLO_MPD_CONFIG", "/etc/ritornello/mpd.toml"));
    let config = Config::charger(&chemin);

    // **Lié avant l'annonce.** C'est la même doctrine que le SDK tient pour ses
    // sockets Unix — lier d'abord, annoncer ensuite — et elle donne ici un
    // comportement utile : un port 6600 déjà pris fait échouer le greffon (le
    // `?` sort de `main` avant même de construire un `Runtime`) sans qu'il
    // s'annonce, donc le cœur le rapporte mort avant annonce et la page de
    // statut le montre. Sinon un port occupé se devinerait dans les journaux.
    let ecoute = TcpListener::bind((config.listen.as_str(), config.port)).await?;
    tracing::info!("mpd server listening on {}:{}", config.listen, config.port);

    let etat = Arc::new(EtatPartage::default());
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    tokio::spawn(accepter(ecoute, etat.clone(), cmd_tx));

    Runtime::from_args()?
        .input(EntreeMpd { rx: cmd_rx })?
        .display(AfficheurMpd { etat })?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn afficheur_mpd_depose_letat_recu_dans_letat_partage() {
        let etat = Arc::new(EtatPartage::default());
        let mut afficheur = AfficheurMpd { etat: etat.clone() };
        let envoye = PlayerState { volume: 17, ..Default::default() };
        afficheur.show(envoye.clone()).await.unwrap();
        assert_eq!(etat.lire().await, envoye);
    }

    #[tokio::test(start_paused = true)]
    async fn la_moitie_input_attend_tant_quun_emetteur_du_canal_vit() {
        // Régression visée : celle documentée sur `EvdevInput` côté
        // `generic-input` (2026-07-27), où l'oubli de cette propriété faisait
        // sortir le greffon en exit 0 dès le démarrage faute de périphérique
        // ouvert. Ici l'émetteur vivant tant qu'`accepter` tourne, ce
        // `next_command` ne doit jamais se terminer tout seul.
        //
        // Horloge simulée (`start_paused`) : sans émetteur qui envoie ni
        // aucun autre minuteur en jeu, tokio avance le temps virtuel jusqu'à
        // l'échéance du `sleep` dès que tout le reste est en attente — la
        // propriété testée ne dépend donc pas de deviner combien de temps
        // "assez longtemps" représente sur la machine qui exécute le test.
        let (tx, rx) = mpsc::channel::<InputMessage>(4);
        let mut entree = EntreeMpd { rx };
        tokio::select! {
            _ = entree.next_command() => panic!("next_command ne doit pas se terminer tant qu'un emetteur vit"),
            _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {}
        }
        // Émetteur lâché : fin propre, l'erreur nomme la cause.
        drop(tx);
        let e = entree.next_command().await.unwrap_err();
        assert!(e.to_string().contains("mpd session"), "erreur inattendue: {e}");
    }

    #[tokio::test]
    async fn accepter_ferme_aussitot_chaque_connexion_et_continue_a_ecouter() {
        // Propriété observable de bout en bout : un client qui se connecte
        // ne reçoit rien et voit son flux fermé (Task 8 y répondra), et la
        // boucle continue d'accepter — un second client n'attend pas
        // indéfiniment derrière le premier. `for _ in 0..2` est ce qui
        // distingue ce test d'un simple test à connexion unique : une
        // implémentation qui accepterait une seule fois (sans boucle) le
        // ferait échouer par un blocage sur le second `connect`/`read`.
        let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adresse = ecoute.local_addr().unwrap();
        let etat = Arc::new(EtatPartage::default());
        let (tx, _rx) = mpsc::channel::<InputMessage>(1);
        tokio::spawn(accepter(ecoute, etat, tx));

        for _ in 0..2 {
            let mut flux = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tokio::net::TcpStream::connect(adresse),
            )
            .await
            .expect("connect n'a pas du bloquer")
            .unwrap();
            let mut octet = [0u8; 1];
            let n = tokio::time::timeout(std::time::Duration::from_secs(5), flux.read(&mut octet))
                .await
                .expect("le flux doit se fermer, pas rester ouvert")
                .unwrap();
            assert_eq!(n, 0, "rien a lire : la session stub ferme aussitot");
        }
    }
}
