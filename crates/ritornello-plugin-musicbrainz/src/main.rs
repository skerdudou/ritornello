//! Plugin `metadata` : reconnaît un disque auprès de MusicBrainz, et sert
//! aussi de relai générique de pochette pour tout le reste.
//!
//! Deux intentions cohabitent dans ce seul binaire :
//! - le **chemin disque** ne réagit qu'aux identités de disque
//!   (`kind: "disc"`), interroge MusicBrainz **une fois par disque**, et émet
//!   ensuite un enrichissement par piste depuis ce qu'il a appris. Il connaît
//!   la TOC, donc il sait ce qui joue : il écrase (`fill_only: false`).
//! - le **chemin générique** cherche une pochette dès que le cœur annonce un
//!   artiste et un album connus, quelle que soit la Source. Il ne sait rien
//!   de plus que ce qu'on lui a donné, donc il ne fait que **compléter**
//!   (`fill_only: true`) : le cœur ne perd rien à ignorer sa réponse si un
//!   autre contributeur tient déjà une pochette.
//!
//! Ce code vivait dans le plugin cd, où un appel réseau de plusieurs secondes
//! partageait le processus qui doit répondre aux commandes de piste. Ici, son
//! échec ou sa lenteur ne concernent que les métadonnées.

mod icy;
mod motifs;
mod musicbrainz;

use anyhow::Result;
use musicbrainz::DiscInfo;
use ritornello_plugin_sdk::{MetadataPlugin, Runtime};
use ritornello_proto::{CoverRef, Enrichment, NowPlaying};
use serde_json::Value;
use tokio::sync::mpsc;

/// Résultat d'une interrogation : la TOC concernée, et ce qu'on a trouvé.
type Trouve = (String, Option<DiscInfo>);

/// Couple qui identifie une recherche du relai générique : artiste, puis
/// album. C'est aussi la clé de mémorisation (voir `MusicBrainzPlugin`).
type CleGenerique = (String, String);

/// Résultat d'une recherche générique : le couple concerné, et le MBID trouvé.
type TrouvePochette = (CleGenerique, Option<String>);

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

/// Faut-il chercher une pochette pour cet état partiel ?
///
/// Un artiste **et** un album, jamais un titre ICY seul : ce dernier est un
/// texte brut, non découpé exprès dans ce projet, et OUI FM émet
/// `Titre - ARTISTE` dans l'ordre inverse de l'usage — le donner à MusicBrainz
/// rendrait n'importe quoi avec assurance.
///
/// Et rien à faire si une pochette est déjà tenue : ce greffon **complète**,
/// donc l'appel serait jeté par l'arbitrage du cœur — une requête dont
/// l'inutilité est connue d'avance.
fn doit_chercher(known: &ritornello_proto::Known) -> bool {
    !known.cover && known.artist.is_some() && known.album.is_some()
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
    /// Enrichissement prêt à partir. Un seul suffit : les deux chemins sont
    /// mutuellement exclusifs (une identité est un disque, ou ne l'est pas).
    pret: Option<Enrichment>,
    trouve_tx: mpsc::Sender<Trouve>,
    trouve_rx: mpsc::Receiver<Trouve>,

    // --- Relai générique (fichier sans pochette, flux dont les métadonnées
    // textuelles suffisent...) ---
    /// Identité courante pour ce chemin, réémise en écho. `None` = rien à
    /// compléter maintenant (chemin disque actif, artiste/album pas encore
    /// connus tous les deux, ou pochette déjà tenue).
    identite_generique: Option<Value>,
    /// Couple (artiste, album) actuellement visé. C'est la clé de
    /// mémorisation choisie : c'est exactement ce que porte la requête
    /// MusicBrainz, elle change dès qu'on change d'album (donc jamais de
    /// pochette d'un autre album qui survit au changement de piste), et elle
    /// reste stable tant que l'album ne change pas (donc pas une requête par
    /// trame reçue). Une identité de Source ne convenait pas : elle peut
    /// rester fixe pendant que artiste/album arrivent en plusieurs trames
    /// (ICY), ou changer sans que l'album change (piste suivante du même
    /// disque de fichiers).
    cle_generique: Option<CleGenerique>,
    /// Dernier couple recherché, et le release_id trouvé (`None` = recherche
    /// faite, rien trouvé). Mémoriser aussi les échecs évite de réinterroger
    /// MusicBrainz à chaque trame tant que l'album ne change pas.
    pochette_connue: Option<TrouvePochette>,
    /// Couple dont la recherche est en vol, pour ne pas la lancer deux fois.
    pochette_en_vol: Option<CleGenerique>,
    pochette_tx: mpsc::Sender<TrouvePochette>,
    pochette_rx: mpsc::Receiver<TrouvePochette>,
}

impl MusicBrainzPlugin {
    fn new() -> Self {
        let (trouve_tx, trouve_rx) = mpsc::channel(4);
        let (pochette_tx, pochette_rx) = mpsc::channel(4);
        Self {
            identite: None,
            disque: None,
            connu: None,
            en_vol: None,
            pret: None,
            trouve_tx,
            trouve_rx,
            identite_generique: None,
            cle_generique: None,
            pochette_connue: None,
            pochette_en_vol: None,
            pochette_tx,
            pochette_rx,
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
            // Le lookup par TOC portait déjà le MBID de la release : aucune
            // requête de plus pour la pochette, juste construire l'URL fixe.
            cover: info.release_id.as_deref().map(|id| CoverRef::Url { url: musicbrainz::url_caa(id) }),
            // Chemin disque : la TOC dit ce qui joue, donc il écrase (défaut).
            ..Default::default()
        });
    }

    /// Prépare l'enrichissement générique (pochette seule) si la recherche a
    /// abouti pour le couple (artiste, album) actuellement visé.
    fn prepare_generique(&mut self) {
        let (Some(identite), Some(cle)) = (&self.identite_generique, &self.cle_generique) else {
            return;
        };
        let Some((connu, Some(release_id))) = &self.pochette_connue else { return };
        if connu != cle {
            return;
        }
        self.pret = Some(Enrichment {
            identity: identite.clone(),
            cover: Some(CoverRef::Url { url: musicbrainz::url_caa(release_id) }),
            // Ce chemin ne sait rien de plus que ce qu'on lui a donné : il ne
            // fait que compléter, jamais écraser un champ déjà renseigné.
            fill_only: true,
            ..Default::default()
        });
    }

    /// Lance la recherche d'une pochette pour ce couple (artiste, album), une
    /// seule fois — même motif que [`Self::cherche`] pour le disque.
    fn cherche_pochette(&mut self, cle: CleGenerique) {
        if self.pochette_en_vol.as_ref() == Some(&cle) {
            return;
        }
        if self.pochette_connue.as_ref().is_some_and(|(connue, _)| connue == &cle) {
            return; // déjà recherché, résultat mémorisé (trouvé ou non)
        }
        self.pochette_en_vol = Some(cle.clone());
        let (artist, album) = cle.clone();
        let tx = self.pochette_tx.clone();
        tokio::spawn(async move {
            let release_id = match musicbrainz::cherche_release(&artist, &album).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::info!("MusicBrainz release search: {e}");
                    None
                }
            };
            let _ = tx.send((cle, release_id)).await;
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
                // Le chemin disque est exclusif : sur un disque, rien à
                // compléter par le relai générique.
                self.identite_generique = None;
                self.cle_generique = None;
                let toc = disque.toc.clone();
                self.disque = Some(disque);
                self.cherche(toc);
                self.prepare();
            }
            None => {
                // Ni disque, ni arrêt : une identité de fichier ou de flux
                // radio, par exemple. Le chemin disque se tait — c'est
                // l'affaire d'un autre plugin — mais le relai générique peut
                // avoir de quoi chercher une pochette.
                self.identite = None;
                self.disque = None;
                match np.identity {
                    Some(identite) if doit_chercher(&np.known) => {
                        let cle = (
                            np.known.artist.expect("verifie par doit_chercher"),
                            np.known.album.expect("verifie par doit_chercher"),
                        );
                        self.identite_generique = Some(identite);
                        self.cle_generique = Some(cle.clone());
                        self.cherche_pochette(cle);
                        self.prepare_generique();
                    }
                    _ => {
                        self.identite_generique = None;
                        self.cle_generique = None;
                    }
                }
            }
        }
    }

    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            if let Some(e) = self.pret.take() {
                return e;
            }
            // `select!` sur deux `recv` reste annulable sans perte : si un
            // `NowPlaying` arrive d'abord, le runner abandonne ce futur et
            // aucun résultat n'est perdu — chaque branche ne mute `self`
            // qu'une fois son message reçu, jamais avant (l'état durable vit
            // dans `self`, pas dans les variables locales de ce futur).
            tokio::select! {
                r = self.trouve_rx.recv() => match r {
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
                },
                r = self.pochette_rx.recv() => match r {
                    Some((cle, release_id)) => {
                        if self.pochette_en_vol.as_ref() == Some(&cle) {
                            self.pochette_en_vol = None;
                        }
                        // Même garde que côté disque : ne retenir le résultat
                        // que s'il décrit le couple (artiste, album) toujours
                        // visé — un changement de piste peut avoir rendu la
                        // recherche en vol obsolète pendant qu'elle volait.
                        if self.cle_generique.as_ref() == Some(&cle) {
                            self.pochette_connue = Some((cle, release_id));
                            self.prepare_generique();
                        }
                    }
                    None => std::future::pending().await,
                },
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

    fn identite_fichier(chemin: &str) -> Value {
        json!({ "kind": "file", "path": chemin })
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
        // Le MBID etait deja porte par le lookup TOC : la pochette part sans
        // requete de plus, et ce chemin ecrase (il sait ce qui joue).
        assert_eq!(
            e.cover,
            Some(CoverRef::Url { url: musicbrainz::url_caa("e32a3f0b-1c19-3170-bb1c-650893774744") })
        );
        assert!(!e.fill_only, "le chemin disque connait la TOC, il ecrase");
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

    #[test]
    fn le_relai_generique_exige_un_artiste_et_un_album_et_se_tait_si_la_pochette_est_tenue() {
        use ritornello_proto::Known;
        // Jamais sur un titre ICY seul : c'est un texte brut, non decoupe, et
        // OUI FM emet « Titre - ARTISTE » dans l'ordre inverse de l'usage.
        assert!(!doit_chercher(&Known { title: Some("X - Y".into()), ..Default::default() }));
        assert!(!doit_chercher(&Known { artist: Some("A".into()), ..Default::default() }));
        assert!(!doit_chercher(&Known { album: Some("B".into()), ..Default::default() }));
        assert!(doit_chercher(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            ..Default::default()
        }));
        // Une pochette deja tenue : l'appel serait jete.
        assert!(!doit_chercher(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            cover: true,
            ..Default::default()
        }));
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

    // `..Default::default()` derrière un littéral pourtant complet : clippy le
    // dit sans effet (`needless_update`), et il a raison **aujourd'hui**. Ce
    // n'est pas de la redondance mais de la compatibilité ascendante — un
    // littéral qui se termine ainsi survit à l'ajout d'un champ dans la
    // structure, celui qui les énumère tous casse. Le dépôt a payé cette
    // leçon : un champ ajouté à une structure publique a cassé 44 littéraux
    // ailleurs, qu'un `cargo test -p` ne compile jamais. Quand clippy et la
    // compatibilité ascendante se contredisent ici, c'est la seconde qui
    // gagne, et la règle qui reçoit un `allow`.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn le_relai_generique_emet_une_pochette_seule_en_completion() {
        // La recherche est pré-mémorisée pour n'exercer aucun appel réseau :
        // c'est `cherche_pochette` qui décide de ne pas relancer, exactement
        // comme `plugin_avec_disque_connu` le fait côté disque.
        let mut p = MusicBrainzPlugin::new();
        let cle = ("Miles Davis".to_string(), "Kind of Blue".to_string());
        p.pochette_connue = Some((cle, Some("e32a3f0b-1c19-3170-bb1c-650893774744".to_string())));
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(identite_fichier("/musique/a.flac")),
            known: ritornello_proto::Known {
                artist: Some("Miles Davis".into()),
                album: Some("Kind of Blue".into()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, identite_fichier("/musique/a.flac"), "l'identite doit etre reemise en echo");
        assert_eq!(
            e.cover,
            Some(CoverRef::Url { url: musicbrainz::url_caa("e32a3f0b-1c19-3170-bb1c-650893774744") })
        );
        assert!(e.fill_only, "ce chemin ne sait rien de plus que ce qu'on lui a donne, il complete");
        assert!(
            e.artist.is_none() && e.title.is_none() && e.album.is_none(),
            "aucun champ de texte : il ne connait rien de plus que ce qu'on lui a donne"
        );
    }

    // Voir `le_relai_generique_emet_une_pochette_seule_en_completion` : le
    // `..Default::default()` est de la compatibilité ascendante, pas de la
    // redondance.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn un_couple_artiste_album_deja_recherche_nest_pas_interroge_a_nouveau() {
        // Mémorisé comme « recherché, rien trouvé » : ne doit pas relancer de
        // requête pour la même trame ni pour une trame suivante du même album.
        let mut p = MusicBrainzPlugin::new();
        let cle = ("A".to_string(), "B".to_string());
        p.pochette_connue = Some((cle, None));
        let known = ritornello_proto::Known { artist: Some("A".into()), album: Some("B".into()), ..Default::default() };
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(identite_fichier("/x")),
            known: known.clone(),
            ..Default::default()
        })
        .await;
        assert!(p.pret.is_none());
        assert!(p.pochette_en_vol.is_none());
        p.now_playing(NowPlaying { source: "files".into(), identity: Some(identite_fichier("/x")), known, ..Default::default() })
            .await;
        assert!(p.pochette_en_vol.is_none(), "un couple deja recherche ne doit pas l'etre a nouveau");
    }

    // Voir `le_relai_generique_emet_une_pochette_seule_en_completion` : le
    // `..Default::default()` est de la compatibilité ascendante, pas de la
    // redondance.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn un_changement_dalbum_ne_reutilise_pas_lancienne_pochette() {
        // La mémorisation est clée par (artiste, album) : un nouvel album doit
        // changer la clé et ne jamais réafficher la pochette de l'ancien.
        let mut p = MusicBrainzPlugin::new();
        p.pochette_connue =
            Some((("A".to_string(), "Vieux".to_string()), Some("11111111-1111-1111-1111-111111111111".into())));
        // Recherche du nouvel album déclarée « en vol » : évite tout appel
        // réseau dans ce test, sans changer ce qui est observé (`en_vol`
        // arrête `cherche_pochette` avant le `tokio::spawn`).
        p.pochette_en_vol = Some(("A".to_string(), "Nouveau".to_string()));
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(identite_fichier("/x")),
            known: ritornello_proto::Known { artist: Some("A".into()), album: Some("Nouveau".into()), ..Default::default() },
            ..Default::default()
        })
        .await;
        assert!(p.pret.is_none(), "la pochette de l'ancien album ne doit pas s'appliquer au nouveau");
        assert_eq!(p.cle_generique, Some(("A".to_string(), "Nouveau".to_string())), "la cle suit le nouvel album");
    }

    #[tokio::test]
    async fn une_identite_de_disque_efface_letat_generique() {
        // Les deux chemins sont exclusifs : un disque inséré ne doit rien
        // laisser du relai générique en place.
        let mut p = MusicBrainzPlugin::new();
        p.en_vol = Some(TOC.to_string()); // évite tout appel réseau dans ce test
        p.identite_generique = Some(identite_fichier("/x"));
        p.cle_generique = Some(("A".to_string(), "B".to_string()));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        assert!(p.identite_generique.is_none());
        assert!(p.cle_generique.is_none());
    }
}
