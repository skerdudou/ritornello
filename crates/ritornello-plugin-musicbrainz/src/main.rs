//! Plugin `metadata` : reconnaît un disque auprès de MusicBrainz.
//!
//! Il reçoit du cœur l'identité de ce qui joue, ne réagit qu'aux identités de
//! disque (`kind: "disc"`), interroge MusicBrainz **une fois par disque**, et
//! émet ensuite un enrichissement par piste depuis ce qu'il a appris.
//!
//! Ce code vivait dans le plugin cd, où un appel réseau de plusieurs secondes
//! partageait le processus qui doit répondre aux commandes de piste. Ici, son
//! échec ou sa lenteur ne concernent que les métadonnées.

mod musicbrainz;

use anyhow::Result;
use musicbrainz::DiscInfo;
use ritornello_plugin_sdk::{MetadataPlugin, Runtime};
use ritornello_proto::{Enrichment, NowPlaying};
use serde_json::Value;
use tokio::sync::mpsc;

/// Résultat d'une interrogation : la TOC concernée, et ce qu'on a trouvé.
type Trouve = (String, Option<DiscInfo>);

/// Ce qu'une identité de disque apprend à ce plugin.
#[derive(Debug, Clone, PartialEq)]
struct Disque {
    toc: String,
    piste: usize,
}

/// Lit une identité opaque et n'en retient un disque que si elle en décrit un.
///
/// Fonction pure : c'est le point d'entrée de données venues d'un autre
/// processus, donc l'endroit où une forme inattendue doit être écartée sans
/// bruit plutôt que de faire paniquer le plugin.
fn disque_de(identity: &Value) -> Option<Disque> {
    if identity.get("kind").and_then(Value::as_str)? != "disc" {
        return None;
    }
    let toc = identity.get("toc").and_then(Value::as_str)?.trim();
    if toc.is_empty() {
        return None;
    }
    // Une identité de disque sans index de piste n'est pas exploitable : on ne
    // saurait pas quel titre annoncer.
    let piste = identity.get("track").and_then(Value::as_u64)? as usize;
    Some(Disque { toc: toc.to_string(), piste })
}

struct MusicBrainzPlugin {
    /// Identité courante, réémise en écho dans chaque enrichissement — c'est le
    /// garde-fou de péremption côté cœur.
    identite: Option<Value>,
    disque: Option<Disque>,
    /// Dernier disque interrogé : TOC brute → résultat (`None` = interrogé,
    /// rien trouvé). Un seul disque suffit : il n'y a qu'un tiroir. Mémoriser
    /// aussi les échecs évite de réinterroger MusicBrainz à chaque changement
    /// de piste d'un disque inconnu — douze pistes, douze requêtes inutiles.
    connu: Option<Trouve>,
    /// TOC dont l'interrogation est en vol, pour ne pas la lancer deux fois.
    en_vol: Option<String>,
    /// Enrichissement prêt à partir.
    pret: Option<Enrichment>,
    trouve_tx: mpsc::Sender<Trouve>,
    trouve_rx: mpsc::Receiver<Trouve>,
}

impl MusicBrainzPlugin {
    fn new() -> Self {
        let (trouve_tx, trouve_rx) = mpsc::channel(4);
        Self {
            identite: None,
            disque: None,
            connu: None,
            en_vol: None,
            pret: None,
            trouve_tx,
            trouve_rx,
        }
    }

    /// Prépare l'enrichissement de la piste courante si le disque est connu.
    fn prepare(&mut self) {
        let (Some(identite), Some(disque)) = (&self.identite, &self.disque) else { return };
        let Some((toc, Some(info))) = &self.connu else { return };
        if toc != &disque.toc {
            return;
        }
        let Some(titre) = info.tracks.get(disque.piste) else {
            // Index hors bornes : le disque reconnu n'a pas ce nombre de pistes.
            // Mieux vaut se taire que d'annoncer le titre d'une autre piste.
            tracing::info!("track {} beyond the {} known titles", disque.piste, info.tracks.len());
            return;
        };
        self.pret = Some(Enrichment {
            identity: identite.clone(),
            artist: Some(info.artist.clone()),
            title: Some(titre.clone()),
            album: Some(info.album.clone()),
            // MusicBrainz donnerait les durées avec `inc=recordings`, mais la
            // durée n'est pas affichée : rien ne justifie d'alourdir la requête.
            duration_s: None,
            // Ce plugin ne sait pas où en est la lecture : il répond
            // sur l'identité d'un morceau, pas sur son déroulement.
            position_s: None,
            ..Default::default()
        });
    }

    /// Lance l'interrogation d'un disque inconnu, une seule fois.
    fn cherche(&mut self, toc: String) {
        if self.en_vol.as_deref() == Some(toc.as_str()) {
            return;
        }
        if let Some((connue, _)) = &self.connu {
            if connue == &toc {
                return; // déjà interrogé, résultat mémorisé (trouvé ou non)
            }
        }
        let param = match musicbrainz::mb_toc_param(&toc) {
            Ok(p) => p,
            Err(e) => {
                // TOC douteuse : on n'appelle pas un service tiers pour rien.
                tracing::info!("unusable TOC, no call made: {e}");
                return;
            }
        };
        // Le premier champ de la TOC **est** le nombre de pistes, et
        // `mb_toc_param` vient de vérifier qu'il concorde avec les offsets.
        let ntracks = toc.split_whitespace().next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        self.en_vol = Some(toc.clone());
        let tx = self.trouve_tx.clone();
        tokio::spawn(async move {
            let info = match musicbrainz::lookup(&param, ntracks).await {
                Ok(info) => info,
                Err(e) => {
                    tracing::info!("MusicBrainz lookup: {e}");
                    None
                }
            };
            let _ = tx.send((toc, info)).await;
        });
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for MusicBrainzPlugin {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Toute annonce périme l'enrichissement préparé : il portait l'identité
        // précédente, et le cœur le jetterait de toute façon.
        self.pret = None;
        let disque = np.identity.as_ref().and_then(disque_de);
        match disque {
            Some(disque) => {
                self.identite = np.identity;
                let toc = disque.toc.clone();
                self.disque = Some(disque);
                self.cherche(toc);
                self.prepare();
            }
            None => {
                // Ni disque, ni arrêt : une identité de flux radio, par exemple.
                // On se tait — c'est l'affaire d'un autre plugin.
                self.identite = None;
                self.disque = None;
            }
        }
    }

    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            if let Some(e) = self.pret.take() {
                return e;
            }
            // `recv` est annulable sans perte : si un `NowPlaying` arrive
            // d'abord, le runner abandonne ce futur et aucun résultat n'est
            // perdu (l'état durable vit dans `self`, pas dans ce futur).
            match self.trouve_rx.recv().await {
                Some((toc, info)) => {
                    if self.en_vol.as_deref() == Some(toc.as_str()) {
                        self.en_vol = None;
                    }
                    // Un résultat n'est retenu que s'il décrit le disque
                    // suivi : deux lookups peuvent se croiser lors d'un
                    // échange rapide de disques (A en vol, B inséré, réponse
                    // de B puis celle de A), et retenir le retardataire
                    // écrasait le cache du disque courant — `prepare()`
                    // protégeait l'affichage, mais le prochain changement de
                    // piste relançait une requête MusicBrainz pour rien.
                    if self.disque.as_ref().is_some_and(|d| d.toc == toc) {
                        self.connu = Some((toc, info));
                        self.prepare();
                    }
                }
                // Impossible en pratique (le plugin garde un Sender) : ne pas
                // rendre la main plutôt que de boucler à vide.
                None => std::future::pending().await,
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    Runtime::from_args()?.metadata(MusicBrainzPlugin::new())?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../tests/fixtures/mb_discid.json");
    const TOC: &str = "3 150 22767 41887 63000";

    fn identite_disque(piste: u64) -> Value {
        json!({ "kind": "disc", "toc": TOC, "tracks": 3, "track": piste })
    }

    /// Plugin dont le disque est déjà connu : évite tout appel réseau dans les
    /// tests, **aucun d'entre eux ne touche le réseau**.
    fn plugin_avec_disque_connu() -> MusicBrainzPlugin {
        let mut p = MusicBrainzPlugin::new();
        p.connu = Some((TOC.to_string(), musicbrainz::parse_lookup(FIXTURE, 3)));
        p
    }

    #[test]
    fn une_identite_de_disque_est_reconnue() {
        let d = disque_de(&identite_disque(2)).unwrap();
        assert_eq!(d.toc, TOC);
        assert_eq!(d.piste, 2);
    }

    #[test]
    fn une_identite_qui_nest_pas_un_disque_est_ignoree() {
        // Le plugin doit se taire sur un flux radio, sans rien inspecter de plus.
        assert!(disque_de(&json!({"kind": "stream", "url": "http://fip"})).is_none());
        assert!(disque_de(&json!({"kind": "disc"})).is_none(), "sans TOC");
        assert!(disque_de(&json!({"kind": "disc", "toc": "  "})).is_none(), "TOC vide");
        assert!(disque_de(&json!({"kind": "disc", "toc": TOC})).is_none(), "sans index de piste");
        assert!(disque_de(&json!("pas un objet")).is_none());
        assert!(disque_de(&Value::Null).is_none());
    }

    #[tokio::test]
    async fn emet_le_titre_de_la_piste_annoncee_avec_echo_de_lidentite() {
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(1)), ..Default::default() }).await;
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, identite_disque(1), "l'identite doit etre reemise en echo");
        assert_eq!(e.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(e.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(e.title.as_deref(), Some("Freddie Freeloader"));
    }

    #[tokio::test]
    async fn un_changement_de_piste_reemet_depuis_le_cache() {
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        assert_eq!(p.next_enrichment().await.title.as_deref(), Some("So What"));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(2)), ..Default::default() }).await;
        let e = p.next_enrichment().await;
        assert_eq!(e.title.as_deref(), Some("Blue in Green"));
        assert_eq!(e.identity, identite_disque(2));
        assert!(p.en_vol.is_none(), "aucune nouvelle interrogation pour le meme disque");
    }

    #[tokio::test]
    async fn larret_efface_lenrichissement_prepare() {
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        p.now_playing(NowPlaying { source: "cd".into(), identity: None, ..Default::default() }).await;
        assert!(p.pret.is_none(), "un enrichissement perime ne doit pas partir apres l'arret");
        assert!(p.identite.is_none());
    }

    #[tokio::test]
    async fn un_flux_radio_ne_declenche_rien() {
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(json!({"kind": "stream", "url": "http://fip"})),
            ..Default::default()
        })
        .await;
        assert!(p.pret.is_none());
        assert!(p.en_vol.is_none(), "aucun appel reseau pour une identite de flux");
    }

    #[tokio::test]
    async fn une_piste_hors_bornes_ne_produit_rien() {
        // Disque reconnu à 3 pistes, mais l'identité annonce la piste 7 : se
        // taire vaut mieux qu'annoncer le titre d'une autre piste.
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(7)), ..Default::default() }).await;
        assert!(p.pret.is_none());
    }

    #[tokio::test]
    async fn un_disque_inconnu_ne_produit_rien_et_nest_interroge_quune_fois() {
        // Résultat mémorisé comme « interrogé, rien trouvé » : les changements
        // de piste suivants ne doivent pas relancer de requête.
        let mut p = MusicBrainzPlugin::new();
        p.connu = Some((TOC.to_string(), None));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        assert!(p.pret.is_none());
        assert!(p.en_vol.is_none());
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(1)), ..Default::default() }).await;
        assert!(p.en_vol.is_none(), "un disque deja interroge ne doit pas l'etre a nouveau");
    }

    #[tokio::test]
    async fn une_toc_inexploitable_ne_declenche_aucun_appel() {
        let mut p = MusicBrainzPlugin::new();
        p.now_playing(NowPlaying {
            source: "cd".into(),
            identity: Some(json!({"kind": "disc", "toc": "n'importe quoi", "track": 0})),
            ..Default::default()
        })
        .await;
        assert!(p.en_vol.is_none());
        assert!(p.pret.is_none());
    }

    #[tokio::test]
    async fn un_resultat_pour_un_autre_disque_ne_produit_rien() {
        // Le disque a été changé pendant que la requête volait : le résultat
        // arrive pour une TOC qui n'est plus celle du tiroir.
        let mut p = MusicBrainzPlugin::new();
        // Interrogation déclarée « en vol » : `cherche` ne lancera donc aucune
        // requête réseau, et le résultat est injecté à la main ci-dessous.
        p.en_vol = Some(TOC.to_string());
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        p.trouve_tx
            .send(("42 1 2 3".to_string(), musicbrainz::parse_lookup(FIXTURE, 3)))
            .await
            .unwrap();
        // `next_enrichment` consomme le résultat périmé puis se remet en attente :
        // on vérifie qu'il ne rend rien dans un délai borné.
        let r = tokio::time::timeout(std::time::Duration::from_millis(200), p.next_enrichment()).await;
        assert!(r.is_err(), "aucun enrichissement ne doit sortir d'un resultat hors sujet");
    }
}
