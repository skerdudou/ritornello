//! Plugin `metadata` : titres des webradios OUI FM, depuis leur propre flux.
//!
//! Pourquoi ce plugin existe : sur cinq flux mesurés, un seul livre un en-tête
//! ICY exploitable, et c'est une webradio étrangère. Les stations françaises
//! courantes n'annoncent rien, ou un texte de remplissage — OUI FM émet
//! littéralement « Now Playing info goes here ». Elle expose en revanche un
//! `text/event-stream` de première main, sans authentification, avec artiste et
//! titre **déjà séparés**.
//!
//! Ce point d'entrée est **privé et non documenté** : il peut changer, exiger
//! une authentification ou disparaître sans préavis. D'où trois règles tenues
//! ici : la récupération vit dans son propre processus et ne retarde jamais la
//! lecture, son échec est silencieux à l'écran, et la reconnexion se fait avec
//! un recul progressif — un appareil sans surveillance ne doit pas marteler le
//! serveur d'un tiers. Rien n'est mis en cache sur disque.

mod flux;
mod table;

use anyhow::Result;
use flux::Meta;
use ritornello_plugin_sdk::{run_metadata_plugin, MetadataPlugin};
use ritornello_proto::{Enrichment, NowPlaying};
use serde_json::Value;
use std::path::PathBuf;
use table::Table;
use tokio::sync::mpsc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// URL d'une identité de flux, si c'en est une.
///
/// Fonction pure : point d'entrée de données venues d'un autre processus, donc
/// l'endroit où une forme inattendue doit être écartée sans bruit.
fn url_du_flux(identity: &Value) -> Option<&str> {
    if identity.get("kind").and_then(Value::as_str)? != "stream" {
        return None;
    }
    let url = identity.get("url").and_then(Value::as_str)?;
    (!url.trim().is_empty()).then_some(url)
}

struct OuiFmMetas {
    table: Table,
    /// Identité courante, réémise en écho dans chaque enrichissement.
    identite: Option<Value>,
    /// Webradio suivie : son identifiant, et la tâche qui tient la connexion.
    ///
    /// La connexion vit dans une tâche et non dans le futur de
    /// `next_enrichment` : ce futur est abandonné dès qu'un `NowPlaying` arrive,
    /// ce qui couperait le flux HTTP à chaque changement d'état du cœur.
    suivi: Option<(String, tokio::task::JoinHandle<()>)>,
    metas_tx: mpsc::Sender<(String, Meta)>,
    metas_rx: mpsc::Receiver<(String, Meta)>,
}

impl OuiFmMetas {
    fn new(table: Table) -> Self {
        let (metas_tx, metas_rx) = mpsc::channel(8);
        Self { table, identite: None, suivi: None, metas_tx, metas_rx }
    }

    /// Arrête le suivi en cours, s'il y en a un.
    fn arrete(&mut self) {
        if let Some((id, tache)) = self.suivi.take() {
            tracing::debug!("stopping tracking of webradio {id}");
            tache.abort();
        }
    }

    /// Suit cette webradio, sauf si c'est déjà celle qu'on suit — auquel cas la
    /// connexion ouverte est conservée. C'est le cas de tous les changements de
    /// morceau sur une même station : les rouvrir ferait perdre la trame que le
    /// serveur pousse dès la connexion, et solliciterait un tiers pour rien.
    fn suit(&mut self, id: &str) {
        if self.suivi.as_ref().is_some_and(|(en_cours, _)| en_cours == id) {
            return;
        }
        self.arrete();
        let tx = self.metas_tx.clone();
        let id_tache = id.to_string();
        let tache = tokio::spawn(flux::suit(id_tache, tx));
        self.suivi = Some((id.to_string(), tache));
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for OuiFmMetas {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Reconnaissance puis mutation : les identifiants sont copiés avant de
        // toucher à `self`, la table étant empruntée à `self`.
        let reconnue = np
            .identity
            .as_ref()
            .and_then(url_du_flux)
            .and_then(|url| self.table.metas_pour(url))
            .map(|w| (w.metas.clone(), w.label.clone()));
        match reconnue {
            Some((metas, label)) => {
                tracing::debug!("webradio recognized: {label} (metas {metas})");
                self.identite = np.identity;
                self.suit(&metas);
            }
            None => {
                // Arrêt, disque, ou station inconnue de la table : on se tait, et
                // surtout on referme la connexion — un flux laissé ouvert
                // continuerait de solliciter un tiers pour un morceau qui ne joue
                // plus.
                self.identite = None;
                self.arrete();
            }
        }
    }

    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            // `recv` est annulable sans perte : si un `NowPlaying` arrive
            // d'abord, le runner abandonne ce futur sans qu'aucune trame reçue
            // ne soit perdue. Tout ce qui suit sa résolution est synchrone,
            // donc hors d'atteinte d'une annulation — c'est ce qui permet de
            // renvoyer directement, sans champ d'attente intermédiaire.
            let Some((id, meta)) = self.metas_rx.recv().await else {
                // Impossible en pratique (le plugin garde un Sender).
                std::future::pending().await
            };
            // Trame d'une station qu'on ne suit plus : elle attendait en file au
            // moment du changement. Même principe que la péremption côté cœur.
            let suit_toujours = self.suivi.as_ref().is_some_and(|(en_cours, _)| en_cours == &id);
            if !suit_toujours {
                continue;
            }
            if let Some(identite) = &self.identite {
                return Enrichment {
                    identity: identite.clone(),
                    artist: meta.artist,
                    title: meta.title,
                    // Le flux ne donne pas d'album (ce sont des webradios).
                    album: None,
                    duration_s: meta.duration_s,
                    // Ce plugin ne sait pas où en est la lecture : il répond
                    // sur l'identité d'un morceau, pas sur son déroulement.
                    position_s: None,
                };
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let socket_path = ritornello_plugin_sdk::socket_path();
    let table_path =
        PathBuf::from(env_or("RITORNELLO_OUIFM_METAS", "/etc/ritornello/ouifm-metas.toml"));
    let table = Table::load(&table_path);
    tracing::info!(
        "{} known webradio(s) (embedded table + {})",
        table.webradios.len(),
        table_path.display()
    );
    run_metadata_plugin(OuiFmMetas::new(table), &socket_path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// URL de flux réelle d'OUI FM Classic Rock, jeton signé compris.
    const URL: &str = "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/11d46a.NND%2BFTMcarOrumMD%2FJU7lENzKQUNWno%2FSz7wPrtsPIw%3D?format=hd";
    /// Identifiant de métadonnées correspondant, relevé de la même source.
    const METAS: &str = "3134161803443976427";

    fn identite_flux(url: &str) -> Value {
        json!({ "kind": "stream", "url": url })
    }

    /// Plugin dont le suivi est déjà déclaré : aucune tâche réseau n'est lancée
    /// dans les tests. **Aucun test ne touche le réseau.**
    fn plugin_suivant(id: &str) -> OuiFmMetas {
        let mut p = OuiFmMetas::new(Table::embarquee());
        // Une tâche inerte tient la place de la connexion HTTP.
        let tache = tokio::spawn(std::future::pending::<()>());
        p.suivi = Some((id.to_string(), tache));
        p
    }

    #[test]
    fn reconnait_une_identite_de_flux() {
        assert_eq!(url_du_flux(&identite_flux(URL)), Some(URL));
    }

    #[test]
    fn ignore_ce_qui_nest_pas_un_flux() {
        assert!(url_du_flux(&json!({"kind": "disc", "toc": "3 1 2 3"})).is_none());
        assert!(url_du_flux(&json!({"kind": "stream"})).is_none());
        assert!(url_du_flux(&json!({"kind": "stream", "url": "  "})).is_none());
        assert!(url_du_flux(&Value::Null).is_none());
    }

    #[tokio::test]
    async fn une_trame_devient_un_enrichissement_avec_echo_de_lidentite() {
        let mut p = plugin_suivant(METAS);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)) }).await;
        p.metas_tx
            .send((
                METAS.into(),
                Meta {
                    artist: Some("Shaka Ponk".into()),
                    title: Some("Wanna Get Free".into()),
                    duration_s: Some(214),
                },
            ))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, identite_flux(URL), "l'identite doit etre reemise en echo");
        assert_eq!(e.artist.as_deref(), Some("Shaka Ponk"));
        assert_eq!(e.title.as_deref(), Some("Wanna Get Free"));
        assert_eq!(e.duration_s, Some(214));
        assert_eq!(e.album, None, "une webradio n'a pas d'album");
    }

    #[tokio::test]
    async fn une_station_inconnue_de_la_table_ferme_le_suivi() {
        let mut p = plugin_suivant(METAS);
        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(identite_flux("http://icecast.radiofrance.fr/fip-midfi.mp3")),
        })
        .await;
        assert!(p.suivi.is_none(), "un flux laisse ouvert solliciterait un tiers pour rien");
        assert!(p.identite.is_none());
    }

    #[tokio::test]
    async fn larret_de_la_lecture_ferme_le_suivi() {
        let mut p = plugin_suivant(METAS);
        p.now_playing(NowPlaying { source: "radio".into(), identity: None }).await;
        assert!(p.suivi.is_none());
    }

    #[tokio::test]
    async fn une_identite_de_disque_ferme_le_suivi() {
        let mut p = plugin_suivant(METAS);
        p.now_playing(NowPlaying {
            source: "cd".into(),
            identity: Some(json!({"kind": "disc", "toc": "3 150 22767 41887 63000", "track": 0})),
        })
        .await;
        assert!(p.suivi.is_none(), "ce plugin ne traite pas les disques");
    }

    #[tokio::test]
    async fn rester_sur_la_meme_station_conserve_la_connexion() {
        // Un changement de morceau donne une nouvelle identité mais la même
        // station : rouvrir le flux ferait perdre la trame que le serveur pousse
        // dès la connexion.
        //
        // Ce test prouve **aussi** la correspondance de bout en bout, depuis la
        // table embarquée : le suivi en place porte l'identifiant de métadonnées
        // de Classic Rock, et si l'URL réelle ne se résolvait pas exactement sur
        // lui, `suit` abandonnerait cette tâche pour en lancer une autre — ce que
        // l'assertion ci-dessous refuse. Aucune connexion réseau n'est ouverte,
        // pour cette raison même.
        let mut p = plugin_suivant(METAS);
        let avant = p.suivi.as_ref().map(|(id, t)| (id.clone(), t.id()));
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)) }).await;
        let apres = p.suivi.as_ref().map(|(id, t)| (id.clone(), t.id()));
        assert_eq!(avant, apres, "la meme tache doit continuer");
    }

    #[tokio::test]
    async fn une_trame_dune_station_quon_ne_suit_plus_est_ecartee() {
        let mut p = plugin_suivant(METAS);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)) }).await;
        // Trame en file au moment du changement de station.
        p.metas_tx
            .send(("99".into(), Meta { title: Some("ancien".into()), ..Default::default() }))
            .await
            .unwrap();
        let r = tokio::time::timeout(std::time::Duration::from_millis(200), p.next_enrichment()).await;
        assert!(r.is_err(), "une trame hors sujet ne doit produire aucun enrichissement");
    }

    #[tokio::test]
    async fn une_table_vide_ne_suit_jamais_rien() {
        // Cas dégénéré, atteignable si la table embarquée devenait vide : le
        // plugin doit rester muet, jamais deviner un identifiant.
        let mut p = OuiFmMetas::new(Table::default());
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)) }).await;
        assert!(p.suivi.is_none());
    }

}
