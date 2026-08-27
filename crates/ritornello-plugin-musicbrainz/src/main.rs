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

mod admin;
mod icy;
mod motifs;
mod musicbrainz;
// Uniquement compilé sous `cargo test` : `ui_placeholder_js` ne sert au
// run-time nulle part dans ce crate, seulement à `build.rs` (compilation
// séparée, via `include!`) et à ses propres tests. Le compiler en continu
// dans le binaire déclencherait un `dead_code` que `-D warnings` refuserait
// (voir `ritornello-plugin-mpd/src/main.rs`, même piège).
#[cfg(test)]
mod placeholder;

use anyhow::Result;
use musicbrainz::DiscInfo;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{MetadataPlugin, Runtime};
use ritornello_proto::{CoverRef, Enrichment, NowPlaying};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Catalogue i18n embarqué de la page d'admin (`admin.rs`). Nommé comme
/// `MPD_EN` côté greffon mpd : c'est ce nom que `Catalog::load` embarque en
/// dernier recours si aucun pack externe n'est présent.
pub(crate) const MUSICBRAINZ_EN: &str = include_str!("locales/en.toml");

/// Échecs de validation **consécutifs** avant de resonder une station déjà
/// connue.
///
/// Un morceau que MusicBrainz ne connaît pas est un échec parfaitement
/// légitime sur un motif juste : resonder au premier échec ferait partir un
/// sondage sur chaque titre obscur, et — puisque l'ordre inverse rend parfois
/// lui aussi un résultat acceptable — pourrait remplacer un bon motif par un
/// mauvais sur un seul coup de chance. Trois échecs d'affilée décrivent une
/// station qui a changé de forme, pas un titre que le catalogue ignore.
const ECHECS_AVANT_RESONDAGE: u32 = 3;

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

/// Lit une identité opaque et n'en retient l'URL que si elle décrit un flux.
///
/// Fonction pure, même contrat que [`disque_de`] : une forme inattendue est
/// écartée sans bruit plutôt que de faire paniquer le plugin.
fn url_de_flux(identity: &Value) -> Option<String> {
    if identity.get("kind").and_then(Value::as_str)? != "stream" {
        return None;
    }
    let url = identity.get("url").and_then(Value::as_str)?.trim();
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
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
    /// Dernier couple recherché, et l'URL de pochette trouvée (`None` =
    /// recherche faite, rien trouvé). Mémoriser aussi les échecs évite de
    /// réinterroger MusicBrainz à chaque trame tant que l'album ne change pas.
    pochette_connue: Option<TrouvePochette>,
    /// Couple dont la recherche est en vol, pour ne pas la lancer deux fois.
    pochette_en_vol: Option<CleGenerique>,
    pochette_tx: mpsc::Sender<TrouvePochette>,
    pochette_rx: mpsc::Receiver<TrouvePochette>,

    // --- Chemin ICY (radio) ---
    /// Le magasin, **partagé avec la page d'admin** : les deux moitiés du
    /// processus le lisent et l'écrivent, comme les deux moitiés du greffon
    /// radio partagent son fichier d'état.
    magasin: Arc<RwLock<motifs::Magasin>>,
    chemin_etat: PathBuf,
    /// Dernière chaîne brute traitée. Icecast répète le même en-tête tout au
    /// long d'un morceau : sans cette garde, chaque répétition relancerait une
    /// requête.
    icy_vu: Option<String>,
    /// Échecs de validation **consécutifs**, par URL de flux. En mémoire et
    /// non persisté : c'est une suite d'événements en cours, pas un fait acquis
    /// sur la station, et un redémarrage est une remise à zéro légitime.
    echecs: HashMap<String, u32>,
    /// URL dont un traitement est en vol, pour ne pas le lancer deux fois.
    icy_en_vol: Option<String>,
    icy_tx: mpsc::Sender<IssueIcy>,
    icy_rx: mpsc::Receiver<IssueIcy>,
}

/// Ce qu'une tâche de traitement ICY rapporte, en **un seul** message.
///
/// Un message et non deux (« voici le motif », « voici le couple ») : la
/// boucle doit pouvoir mettre à jour le magasin, le compteur d'échecs et
/// l'enrichissement dans le même tour, sans état intermédiaire où le motif
/// serait retenu mais le compteur pas encore remis à zéro.
#[derive(Debug)]
struct IssueIcy {
    url: String,
    /// L'identité **reçue** du cœur, transportée avec le travail pour être
    /// renvoyée en écho.
    ///
    /// Dans le message et non dans un champ du greffon, et les deux raisons
    /// comptent :
    ///
    /// * **Reçue, pas reconstruite.** Cet écho est le garde-fou de péremption
    ///   du cœur, qui compare la valeur *entière*. La rebâtir depuis l'URL est
    ///   juste aujourd'hui et faux dès qu'une source enrichit son identité — un
    ///   numéro de présélection, ce serait naturel — et le mode de panne serait
    ///   un rejet **silencieux** de chaque enrichissement.
    /// * **Attachée au travail, pas au greffon.** Un champ de `self` serait
    ///   écrasé par une trame plus récente pendant qu'un traitement vole
    ///   encore, et l'issue d'un ancien morceau repartirait avec l'identité du
    ///   nouveau. La faire voyager lie l'écho à ce qu'il décrit.
    identite: Value,
    /// La chaîne traitée. Sert de garde de péremption : une issue qui ne
    /// décrit pas la chaîne courante est jetée, comme les deux autres chemins
    /// jettent une réponse qui ne décrit plus ce qui joue.
    brut: String,
    /// Le motif à retenir quand un sondage a eu lieu. `None` = pas de
    /// sondage (régime établi), donc rien à apprendre.
    motif: Option<motifs::Motif>,
    /// Le couple validé et sa pochette. `None` = validation échouée.
    valide: Option<(String, String, Option<String>)>,
    /// Le couple issu du découpage **local**, que la validation ait abouti ou
    /// non.
    ///
    /// Distinct de `valide`, et la distinction porte une correction de
    /// relecture : un morceau que MusicBrainz ne connaît pas est un échec de
    /// **validation**, pas une raison de jeter un découpage dont le motif a
    /// déjà fait ses preuves sur cette station. Sans ce champ, le greffon
    /// n'émettait rien dans ce cas — et comme l'identité d'une radio ne change
    /// pas d'un morceau à l'autre, l'enrichissement du morceau **précédent**
    /// restait gagnant : l'écran annonçait l'artiste, le titre et la pochette
    /// d'avant pendant toute la durée du suivant.
    couple: Option<(String, String)>,
}

impl MusicBrainzPlugin {
    fn new(magasin: Arc<RwLock<motifs::Magasin>>, chemin_etat: PathBuf) -> Self {
        let (trouve_tx, trouve_rx) = mpsc::channel(4);
        let (pochette_tx, pochette_rx) = mpsc::channel(4);
        let (icy_tx, icy_rx) = mpsc::channel(4);
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
            magasin,
            chemin_etat,
            icy_vu: None,
            echecs: HashMap::new(),
            icy_en_vol: None,
            icy_tx,
            icy_rx,
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
            // Le lookup par TOC portait déjà de quoi construire l'URL, et le
            // choix du niveau (ce pressage, ou l'album à défaut de face avant)
            // a été fait à l'analyse. Aucune requête de plus ici.
            cover: info.cover_url.clone().map(|url| CoverRef::Url { url }),
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
        let Some((connu, Some(cover_url))) = &self.pochette_connue else { return };
        if connu != cle {
            return;
        }
        self.pret = Some(Enrichment {
            identity: identite.clone(),
            // URL déjà résolue par `cherche_release` : ce chemin ne rebâtit
            // rien. Une recherche ne porte pas de bloc `cover-art-archive`,
            // donc c'est la pochette de l'album qui en sort.
            cover: Some(CoverRef::Url { url: cover_url.clone() }),
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
            let cover_url = match musicbrainz::cherche_release(&artist, &album).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::info!("MusicBrainz release search: {e}");
                    None
                }
            };
            let _ = tx.send((cle, cover_url)).await;
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
                // Capturés avant que le traitement générique ci-dessous ne
                // déplace `np.identity` : le chemin ICY en a besoin après.
                let url_flux = np.identity.as_ref().and_then(url_de_flux);
                let stream_title = np.known.stream_title.clone();
                // Clonée ici, avec ses voisines, parce que le `match` ci-dessous
                // déplace `np.identity` : c'est cette valeur-là qui repartira en
                // écho, jamais une reconstruction. Voir `IssueIcy::identite`.
                let identite_flux = np.identity.clone();
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

                // --- Chemin ICY : après le traitement générique qui précède,
                // sans y toucher --------------------------------------------
                //
                // Déclenché sur un changement de `stream_title`, pas sur
                // chaque trame : Icecast répète le même en-tête tout au long
                // d'un morceau, et le retraiter à chaque fois serait une
                // requête pour rien.
                if let Some(url) = url_flux {
                    if stream_title != self.icy_vu {
                        self.icy_vu = stream_title.clone();
                        if let Some(brut) = stream_title {
                            // `icy_en_vol` empêche de lancer un second
                            // traitement pour la même URL pendant qu'un
                            // premier vole encore ; la garde de péremption
                            // dans `next_enrichment` filtre une réponse
                            // devenue hors sujet le temps du vol.
                            if self.icy_en_vol.as_deref() != Some(url.as_str()) {
                                self.icy_en_vol = Some(url.clone());
                                // **Une station au motif manuel n'est jamais
                                // resondee.** Le magasin refusait bien de
                                // reecrire l'entree (`Magasin::apprend`), mais
                                // rien n'empechait le sondage de partir — et
                                // alors c'etait *son* decoupage qui s'affichait,
                                // pas celui de l'operateur. La documentation
                                // etait donc vraie du fichier et fausse de
                                // l'ecran. Consulter l'origine ici ferme l'ecart
                                // a la source : si l'operateur a tranche, on
                                // applique ce qu'il a pose, meme quand
                                // MusicBrainz n'en veut pas.
                                let manuel = self
                                    .magasin
                                    .read()
                                    .await
                                    .entree(&url)
                                    .map(|e| e.origine == motifs::Origine::Manuel)
                                    .unwrap_or(false);
                                let resonde = !manuel && doit_resonder(&self.echecs, &url);
                                if resonde {
                                    // **Le resondage consomme le compteur.**
                                    // Sans ça il restait au-dessus du seuil
                                    // pour la vie du processus : une station
                                    // qui ne valide jamais — un flux en
                                    // mojibake, par exemple — repartait en
                                    // sondage complet à *chaque* titre, ce qui
                                    // démentait la documentation et faisait de
                                    // cette limite une tempête de requêtes
                                    // garantie. Un resondage rachète trois
                                    // titres, il ne s'arme pas en permanence.
                                    self.echecs.remove(&url);
                                }
                                let magasin = self.magasin.clone();
                                let tx = self.icy_tx.clone();
                                let url_tache = url.clone();
                                // `url_de_flux` a déjà reconnu l'identité, donc
                                // elle est là : l'`unwrap_or` n'est qu'une
                                // totalité de type, pas un cas d'usage.
                                let identite = identite_flux.clone().unwrap_or(Value::Null);
                                tokio::spawn(async move {
                                    let connu = magasin.read().await.entree(&url_tache).map(|e| e.motif.clone());
                                    let issue = traite_icy(url_tache, brut, identite, connu, resonde).await;
                                    let _ = tx.send(issue).await;
                                });
                            }
                        }
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
                    Some((cle, cover_url)) => {
                        if self.pochette_en_vol.as_ref() == Some(&cle) {
                            self.pochette_en_vol = None;
                        }
                        // Même garde que côté disque : ne retenir le résultat
                        // que s'il décrit le couple (artiste, album) toujours
                        // visé — un changement de piste peut avoir rendu la
                        // recherche en vol obsolète pendant qu'elle volait.
                        if self.cle_generique.as_ref() == Some(&cle) {
                            self.pochette_connue = Some((cle, cover_url));
                            self.prepare_generique();
                        }
                    }
                    None => std::future::pending().await,
                },
                r = self.icy_rx.recv() => match r {
                    Some(issue) => {
                        if self.icy_en_vol.as_deref() == Some(issue.url.as_str()) {
                            self.icy_en_vol = None;
                        }
                        // **Le motif est retenu avant la garde de
                        // péremption**, et l'ordre est le correctif : un motif
                        // décrit la **station**, pas le morceau. Une issue de
                        // sondage devenue périmée pendant son vol — la station a
                        // changé de titre, ce qui prend quelques secondes et le
                        // sondage en prend quatre — porte quand même un
                        // apprentissage valable, vérifié contre MusicBrainz.
                        //
                        // Jeter l'issue entière avant cette ligne, comme le
                        // faisait la version d'avant, pouvait faire qu'une
                        // station n'apprenne **jamais rien** : chaque sondage
                        // était invalidé par le changement de titre qui l'avait
                        // en partie provoqué.
                        if let Some(m) = issue.motif {
                            let mut magasin = self.magasin.write().await;
                            magasin.apprend(&issue.url, m);
                            if let Err(e) = magasin.enregistre(&self.chemin_etat) {
                                tracing::warn!("could not save ICY patterns: {e}");
                            }
                        }
                        // Garde de péremption, comme les deux autres chemins,
                        // mais elle ne protège plus que ce qui décrit **le
                        // morceau** : le couple et la pochette.
                        if self.icy_vu.as_deref() != Some(issue.brut.as_str()) {
                            continue;
                        }
                        match issue.valide {
                            Some((artist, title, cover_url)) => {
                                {
                                    let mut magasin = self.magasin.write().await;
                                    magasin.succes(&issue.url);
                                    if let Err(e) = magasin.enregistre(&self.chemin_etat) {
                                        tracing::warn!("could not save ICY patterns: {e}");
                                    }
                                }
                                self.echecs.remove(&issue.url);
                                self.pret = Some(Enrichment {
                                    // L'identité **reçue**, reportée telle
                                    // quelle. Voir `IssueIcy::identite`, qui dit
                                    // pourquoi elle voyage avec le travail.
                                    identity: issue.identite,
                                    artist: Some(artist),
                                    title: Some(title),
                                    // URL déjà résolue par `premier_enregistrement`.
                                    cover: cover_url.map(|url| CoverRef::Url { url }),
                                    // Ce chemin **remplace** la chaîne ICY
                                    // brute, qui est précisément ce qu'on
                                    // corrige — à la différence du relai
                                    // générique voisin (`fill_only: true`),
                                    // qui ne fait que compléter parce qu'il ne
                                    // sait rien de plus que ce qu'on lui a
                                    // donné. Ici on écrase, et seulement ce
                                    // que MusicBrainz vient de confirmer.
                                    fill_only: false,
                                    ..Default::default()
                                });
                            }
                            None => {
                                *self.echecs.entry(issue.url.clone()).or_default() += 1;
                                // **Émettre quand même.** Ne rien envoyer
                                // laissait l'enrichissement du morceau
                                // *précédent* gagner l'arbitrage, l'identité
                                // d'une radio ne changeant pas d'un morceau à
                                // l'autre : l'écran annonçait l'artiste, le
                                // titre et la pochette d'avant pendant toute la
                                // durée du suivant. Le pire des trois états, et
                                // celui que ma spec décrivait sans le voir en
                                // écrivant « ne rien émettre pour ce morceau ».
                                //
                                // Ce qu'on émet dépend de ce qu'on sait :
                                //
                                // * le couple local, quand le motif s'applique.
                                //   MusicBrainz ne connaît pas ce morceau, ce
                                //   qui ne dit rien contre un découpage déjà
                                //   confirmé sur cette station. Sans pochette,
                                //   faute de release à citer.
                                // * sinon la chaîne nettoyée en guise de titre :
                                //   le motif ne s'applique plus (la station a
                                //   changé de forme) ou il n'y en a pas. On
                                //   n'affirme alors aucun découpage — juste ce
                                //   que le flux annonce, débarrassé de sa
                                //   réclame.
                                let (artist, title) = match issue.couple {
                                    Some((a, t)) => (Some(a), Some(t)),
                                    None => (None, Some(icy::nettoie(&issue.brut))),
                                };
                                self.pret = Some(Enrichment {
                                    identity: issue.identite,
                                    artist,
                                    title,
                                    fill_only: false,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    None => std::future::pending().await,
                },
            }
        }
    }
}

/// La station doit-elle être resondée ?
///
/// Extrait en fonction pure pour la même raison que [`meilleur_accepte`] : le
/// réseau n'est pas joignable en test, donc c'est la **décision** qui doit
/// être éprouvée, pas le sondage qu'elle déclenche. Le seuil est en échecs
/// **consécutifs** : voir [`ECHECS_AVANT_RESONDAGE`].
fn doit_resonder(echecs: &HashMap<String, u32>, url: &str) -> bool {
    echecs.get(url).copied().unwrap_or(0) >= ECHECS_AVANT_RESONDAGE
}

/// Diagnostique un encodage douteux, sans le réparer.
///
/// Un titre en mojibake ne validera **jamais** contre MusicBrainz, et
/// ressemblerait sinon à un mauvais découpage alors que le découpage était
/// bon : sans ce diagnostic distinct, on chercherait le défaut du mauvais
/// côté.
fn signale_encodage_douteux(brut: &str) {
    // `U+FFFD` : le caractère de remplacement qu'un décodage UTF-8 forcé sur
    // des octets qui n'en sont pas laisse derrière lui.
    if brut.contains('\u{FFFD}') {
        tracing::warn!("ICY stream title looks mis-decoded (replacement character present): {brut:?}");
        return;
    }
    // Séquence caractéristique d'un texte relu dans le mauvais jeu de
    // caractères : les deux octets d'un caractère accentué UTF-8 (tête
    // 0xC2/0xC3, puis un octet de continuation 0x80-0xBF) se relisent
    // ailleurs comme « Â »/« Ã » suivi d'un symbole Latin-1 Supplement — « Ã©
    // » pour un « é », par exemple.
    let douteux =
        brut.chars().zip(brut.chars().skip(1)).any(|(a, b)| matches!(a, 'Â' | 'Ã') && ('\u{80}'..='\u{BF}').contains(&b));
    if douteux {
        tracing::warn!("ICY stream title looks mis-decoded (latin-1/UTF-8 mismatch): {brut:?}");
    }
}

/// Un candidat est-il validé par cette réponse ?
///
/// Les deux conditions comptent : le score seul est trop généreux, la
/// recherche MusicBrainz rendant presque toujours quelque chose de plausible.
/// L'égalité de titre normalisée est la garde qui porte tout.
fn valide(titre_candidat: &str, e: &musicbrainz::Enregistrement) -> bool {
    e.score >= musicbrainz::SEUIL_RECORDING && musicbrainz::normalise(&e.titre) == musicbrainz::normalise(titre_candidat)
}

/// Choisit le meilleur candidat accepté parmi des réponses déjà obtenues.
///
/// Séparée du réseau exprès : c'est la décision, et c'est elle qui doit être
/// éprouvée. Les paires sont `(candidat, réponse)`, dans l'ordre d'essai.
fn meilleur_accepte(essais: &[(icy::Candidat, Option<musicbrainz::Enregistrement>)]) -> Option<&icy::Candidat> {
    essais
        .iter()
        .filter_map(|(c, reponse)| reponse.as_ref().filter(|e| valide(&c.titre, e)).map(|e| (c, e.score)))
        .max_by_key(|(_, score)| *score)
        .map(|(c, _)| c)
}

/// Valide un couple déjà découpé localement, par une recherche
/// d'enregistrement.
///
/// C'est la validation continue du régime établi (voir la doc du module) :
/// elle sert aussi à trouver la pochette, qu'une radio n'annonce jamais
/// autrement.
async fn valide_par_recherche(artiste: &str, titre: &str) -> Option<(String, String, Option<String>)> {
    let reponse = musicbrainz::cherche_enregistrement(artiste, titre)
        .await
        .unwrap_or_else(|e| {
            tracing::info!("MusicBrainz recording search: {e}");
            None
        })?;
    if valide(titre, &reponse) {
        Some((artiste.to_string(), titre.to_string(), reponse.cover_url))
    } else {
        None
    }
}

/// Traite une chaîne ICY : applique le motif connu, ou sonde la station.
///
/// Détachée dans une tâche, comme les deux autres chemins : une station peut
/// coûter quatre requêtes espacées d'une seconde, et la boucle du greffon ne
/// doit pas attendre.
async fn traite_icy(
    url: String,
    brut: String,
    identite: Value,
    connu: Option<motifs::Motif>,
    resonde: bool,
) -> IssueIcy {
    signale_encodage_douteux(&brut);
    let nettoye = icy::nettoie(&brut);

    if !resonde {
        match &connu {
            Some(motifs::Motif::NePasDecouper) => {
                // La station parlée : coût nul, aucune requête.
                return IssueIcy { url, brut, identite, motif: None, valide: None, couple: None };
            }
            Some(m @ motifs::Motif::Separe { .. }) => {
                // Régime établi : découpage local, une seule requête qui vaut
                // à la fois validation continue et recherche de pochette.
                //
                // Le couple local est rapporté **même si la validation
                // échoue** : c'est notre meilleure connaissance du morceau, et
                // le motif qui l'a produit a déjà été confirmé sur cette
                // station. Voir `IssueIcy::couple`.
                let couple = icy::applique(m, &nettoye);
                let valide = match &couple {
                    Some((artiste, titre)) => valide_par_recherche(artiste, titre).await,
                    None => None,
                };
                return IssueIcy { url, brut, identite, motif: None, valide, couple };
            }
            None => {} // Station jamais sondée : tombe dans le sondage ci-dessous.
        }
    }

    // Sondage : station inconnue, ou resondage déclenché par trois échecs
    // d'affilée.
    let candidats = icy::candidats(&nettoye);
    let mut essais = Vec::with_capacity(candidats.len());
    for c in candidats {
        let reponse = musicbrainz::cherche_enregistrement(&c.artiste, &c.titre).await.unwrap_or_else(|e| {
            tracing::info!("MusicBrainz recording search: {e}");
            None
        });
        essais.push((c, reponse));
    }
    let nb_essayes = essais.len();
    // Un plafond silencieux se lit comme « on a tout essayé » : le dire
    // quand le nombre de candidats sondés touche le plafond de icy::candidats.
    if nb_essayes >= icy::MAX_CANDIDATS {
        tracing::info!(
            "ICY probe for {url}: hit the {}-candidate cap, some derivable candidates may not have been tried",
            icy::MAX_CANDIDATS
        );
    }
    match meilleur_accepte(&essais).cloned() {
        Some(gagnant) => {
            let score = essais.iter().find(|(c, _)| *c == gagnant).and_then(|(_, r)| r.as_ref()).map(|e| e.score);
            let cover_url =
                essais.iter().find(|(c, _)| *c == gagnant).and_then(|(_, r)| r.as_ref()).and_then(|e| e.cover_url.clone());
            tracing::info!(
                "ICY probe for {url}: tried {nb_essayes} candidate(s), kept \"{}\" / \"{}\" (score {:?})",
                gagnant.artiste,
                gagnant.titre,
                score
            );
            IssueIcy {
                url,
                brut,
                identite,
                motif: Some(motifs::Motif::depuis_candidat(&gagnant)),
                valide: Some((gagnant.artiste.clone(), gagnant.titre.clone(), cover_url)),
                couple: Some((gagnant.artiste, gagnant.titre)),
            }
        }
        None => {
            tracing::info!("ICY probe for {url}: tried {nb_essayes} candidate(s), none accepted");
            IssueIcy {
                url,
                brut,
                identite,
                motif: Some(motifs::Motif::NePasDecouper),
                valide: None,
                couple: None,
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let chemin_etat = PathBuf::from(
        std::env::var("RITORNELLO_MUSICBRAINZ_STATE")
            .unwrap_or_else(|_| "/var/lib/ritornello/plugin-musicbrainz.json".to_string()),
    );
    let magasin = Arc::new(RwLock::new(motifs::Magasin::charge(&chemin_etat)));

    // Un greffon `metadata` ne reçoit pas de trame `SetLocale` (elle
    // n'existe que pour `SourcePlugin`) : la langue de la page d'admin vient
    // donc de l'environnement au lancement, comme en generic-input et en
    // mpd — un changement de langue de l'appareil ne s'y voit qu'après un
    // redémarrage du greffon (voir la doc de `admin::MusicBrainzAdmin`).
    let locales_root = PathBuf::from(
        std::env::var("RITORNELLO_LOCALES").unwrap_or_else(|_| "/etc/ritornello/locales".to_string()),
    );
    let locale = std::env::var("RITORNELLO_LOCALE").unwrap_or_else(|_| "en".to_string());
    let catalog = Arc::new(std::sync::RwLock::new(Catalog::load(
        "musicbrainz",
        &locale,
        &locales_root,
        MUSICBRAINZ_EN,
    )));

    Runtime::from_args()?
        .metadata(MusicBrainzPlugin::new(magasin.clone(), chemin_etat.clone()))?
        .admin(admin::MusicBrainzAdmin::new(magasin, chemin_etat, catalog))?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn en_embarque_musicbrainz_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(MUSICBRAINZ_EN).unwrap().is_empty());
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/mb_discid.json");
    const TOC: &str = "3 150 22767 41887 63000";

    fn identite_disque(piste: u64) -> Value {
        json!({ "kind": "disc", "toc": TOC, "tracks": 3, "track": piste })
    }

    fn identite_fichier(chemin: &str) -> Value {
        json!({ "kind": "file", "path": chemin })
    }

    /// Un plugin neuf, magasin vide en mémoire et chemin d'état jetable.
    ///
    /// Le chemin est unique par appel (compteur atomique + PID) : plusieurs
    /// tests tournent en parallèle, et un fichier partagé se ferait voler la
    /// vedette par un autre test qui écrit au même instant.
    fn plugin_test() -> MusicBrainzPlugin {
        static COMPTEUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COMPTEUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let chemin = std::env::temp_dir().join(format!("ritornello-mb-test-{}-{n}.json", std::process::id()));
        MusicBrainzPlugin::new(Arc::new(RwLock::new(motifs::Magasin::default())), chemin)
    }

    /// Plugin dont le disque est déjà connu : évite tout appel réseau dans les
    /// tests, **aucun d'entre eux ne touche le réseau**.
    fn plugin_avec_disque_connu() -> MusicBrainzPlugin {
        let mut p = plugin_test();
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
        let mut p = plugin_test();
        p.connu = Some((TOC.to_string(), None));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        assert!(p.pret.is_none());
        assert!(p.en_vol.is_none());
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(1)), ..Default::default() }).await;
        assert!(p.en_vol.is_none(), "un disque deja interroge ne doit pas l'etre a nouveau");
    }

    #[tokio::test]
    async fn une_toc_inexploitable_ne_declenche_aucun_appel() {
        let mut p = plugin_test();
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
        let mut p = plugin_test();
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
        let mut p = plugin_test();
        let cle = ("Miles Davis".to_string(), "Kind of Blue".to_string());
        // Une URL deja resolue, comme ce que `cherche_release` memorise : c'est
        // le module qui decide du niveau (album ou pressage), jamais ce
        // chemin-ci. Ici celle d'un groupe, le cas courant d'une recherche.
        let pochette = musicbrainz::url_caa_groupe("8e8a594f-2175-38c7-a871-abb68ec363e7");
        p.pochette_connue = Some((cle, Some(pochette.clone())));
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
        assert_eq!(e.cover, Some(CoverRef::Url { url: pochette }));
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
        let mut p = plugin_test();
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
        let mut p = plugin_test();
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
        let mut p = plugin_test();
        p.en_vol = Some(TOC.to_string()); // évite tout appel réseau dans ce test
        p.identite_generique = Some(identite_fichier("/x"));
        p.cle_generique = Some(("A".to_string(), "B".to_string()));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        assert!(p.identite_generique.is_none());
        assert!(p.cle_generique.is_none());
    }

    // --- Chemin ICY (radio) ---------------------------------------------

    fn candidat(artiste: &str, titre: &str, artiste_en_premier: bool) -> icy::Candidat {
        icy::Candidat {
            artiste: artiste.to_string(),
            titre: titre.to_string(),
            separateur: " - ",
            artiste_en_premier,
            titre_au_milieu: false,
        }
    }

    fn enregistrement(score: u64, titre: &str) -> musicbrainz::Enregistrement {
        musicbrainz::Enregistrement { score, titre: titre.to_string(), cover_url: None }
    }

    #[test]
    fn le_meilleur_score_gagne_et_non_le_premier_accepte() {
        // Le gagnant est **second** dans l'ordre d'essai : sans cela, le test
        // passerait aussi avec « prendre le premier accepté ».
        let essais = vec![
            // L'ordre inversé valide quand même (score au-dessus du seuil,
            // mais plus faible) : c'est le cas réel qui rend « prendre le
            // premier accepté » dangereux.
            (candidat("So What", "Miles Davis", false), Some(enregistrement(91, "Miles Davis"))),
            (candidat("Miles Davis", "So What", true), Some(enregistrement(99, "So What"))),
        ];
        let gagnant = meilleur_accepte(&essais).expect("un candidat doit etre retenu");
        assert_eq!((gagnant.artiste.as_str(), gagnant.titre.as_str()), ("Miles Davis", "So What"));
        assert!(gagnant.artiste_en_premier);
    }

    #[test]
    fn un_titre_qui_ne_correspond_pas_est_ecarte_malgre_un_bon_score() {
        // La garde qui porte tout : le score seul est trop généreux, la
        // recherche rendant presque toujours quelque chose de plausible.
        let essais =
            vec![(candidat("So What", "Miles Davis", false), Some(enregistrement(95, "Un Tout Autre Enregistrement")))];
        assert!(meilleur_accepte(&essais).is_none(), "score haut mais titre discordant : doit etre ecarte");
    }

    #[test]
    fn aucun_candidat_accepte_donne_ne_pas_decouper() {
        // Aucun essai (chaîne sans séparateur, cf. `icy::candidats`) ou aucun
        // accepté : le sondage n'a rien retenu, ce que `traite_icy` traduit en
        // `Motif::NePasDecouper` (non rejoué ici, le réseau n'étant pas
        // joignable en test — `meilleur_accepte` porte la décision).
        assert!(meilleur_accepte(&[]).is_none(), "aucun essai, donc aucun accepte");
        let essais = vec![
            (candidat("A", "B", true), None), // hors ligne / rien trouve
            (candidat("B", "A", false), Some(enregistrement(50, "A"))), // sous le seuil
        ];
        assert!(meilleur_accepte(&essais).is_none());
    }

    #[tokio::test]
    async fn une_station_classee_ne_pas_decouper_ne_declenche_aucune_requete() {
        // `traite_icy` avec `connu = NePasDecouper` et `resonde = false` doit
        // rendre son issue **sans** toucher au réseau. Prouvé par le fait que
        // le test passe alors qu'aucun réseau n'est joignable ici : une
        // requête tentée échouerait ou traînerait, et le délai ci-dessous la
        // ferait échouer.
        let r = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            traite_icy(
                "http://f".to_string(),
                "Miles Davis - So What".to_string(),
                json!({"kind": "stream", "url": "http://f"}),
                Some(motifs::Motif::NePasDecouper),
                false,
            ),
        )
        .await;
        let issue = r.expect("aucune requete reseau ne doit etre tentee, donc pas de delai");
        assert_eq!(issue.motif, None);
        assert_eq!(issue.valide, None);
    }

    /// Envoie une issue d'échec (validation ratée) pour `url`/`brut`, et
    /// consomme le tour de boucle qui en résulte.
    ///
    /// **Un échec produit désormais un enrichissement**, et c'est une
    /// correction de relecture : ne rien émettre laissait celui du morceau
    /// *précédent* gagner l'arbitrage, l'identité d'une radio ne changeant pas
    /// d'un morceau à l'autre. L'assertion d'avant — « aucun enrichissement » —
    /// épinglait donc le défaut au lieu de la propriété.
    ///
    /// Avec `couple: None`, ce qui part est la chaîne nettoyée en guise de
    /// titre, sans artiste : on n'affirme aucun découpage, on montre ce que le
    /// flux annonce. Et l'attente est **exacte** (on attend ce qui doit venir)
    /// au lieu de reposer sur une marge de temps.
    async fn envoie_echec(p: &mut MusicBrainzPlugin, url: &str, brut: &str) {
        p.icy_tx
            .send(IssueIcy {
                url: url.to_string(),
                brut: brut.to_string(),
                identite: json!({"kind": "stream", "url": url}),
                motif: None,
                valide: None,
                couple: None,
            })
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.artist, None, "un echec n'affirme aucun artiste");
        assert_eq!(
            e.title.as_deref(),
            Some(icy::nettoie(brut).as_str()),
            "il montre ce que le flux annonce, nettoye"
        );
        assert!(e.cover.is_none(), "et aucune pochette, faute de release a citer");
    }

    #[tokio::test]
    async fn un_echec_isole_ne_resonde_pas_et_trois_daffilee_resondent() {
        // Les deux moitiés. Sans la première, « resonder toujours » passerait ;
        // sans la seconde, « ne resonder jamais » passerait.
        //
        // Le compteur et la décision sont exercés par le vrai chemin de code
        // (l'issue traverse `icy_tx`/`next_enrichment`, comme
        // `un_resultat_pour_un_autre_disque_ne_produit_rien` le fait déjà côté
        // disque) : ce n'est pas une resimulation en dur de l'arithmétique.
        let mut p = plugin_test();
        let url = "http://f";
        p.icy_vu = Some("brut".to_string());

        for n in 1..=2u32 {
            envoie_echec(&mut p, url, "brut").await;
            assert_eq!(p.echecs.get(url), Some(&n));
            assert!(!doit_resonder(&p.echecs, url), "echec numero {n} : ne doit pas encore resonder");
        }

        envoie_echec(&mut p, url, "brut").await;
        assert_eq!(p.echecs.get(url), Some(&3));
        assert!(doit_resonder(&p.echecs, url), "trois echecs d'affilee doivent resonder");
    }

    /// Le resondage **consomme** le compteur d'echecs.
    ///
    /// Sans ca il restait au-dessus du seuil pour la vie du processus, et une
    /// station qui ne valide jamais — un flux en mojibake, par exemple —
    /// repartait en sondage complet a *chaque* titre. La documentation promet
    /// l'inverse, et la limite qu'elle decrit devenait une tempete de requetes
    /// garantie. Constat de la relecture croisee finale.
    ///
    /// Eprouve sur `now_playing` et non sur `doit_resonder` seul : la remise a
    /// zero vit au site de lancement, et c'est le lien entre les deux que ce
    /// test doit tenir. La tache detachee qui suit ne peut pas joindre le
    /// reseau, ce qui ne gene pas — la remise a zero est synchrone et precede
    /// le `spawn`.
    #[tokio::test]
    async fn un_resondage_consomme_le_compteur() {
        let mut p = plugin_test();
        let url = "http://exemple/flux.mp3";
        let identite = json!({"kind": "stream", "url": url});
        p.echecs.insert(url.to_string(), ECHECS_AVANT_RESONDAGE);
        assert!(doit_resonder(&p.echecs, url), "trois echecs arment bien le resondage");

        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(identite),
            known: ritornello_proto::Known {
                stream_title: Some("Miles Davis - So What".into()),
                ..Default::default()
            },
        })
        .await;

        assert_eq!(
            p.echecs.get(url),
            None,
            "le lancement du resondage doit avoir consomme le compteur"
        );
        assert!(
            !doit_resonder(&p.echecs, url),
            "et le titre suivant ne doit pas resonder a son tour"
        );
    }

    #[tokio::test]
    async fn un_succes_remet_le_compteur_a_zero() {
        // Deux échecs, un succès, deux échecs : pas de resondage. C'est la
        // seule assertion qui distingue un compteur consécutif d'un
        // cumulatif — et le cumulatif est le défaut naturel.
        let mut p = plugin_test();
        let url = "http://f";
        p.icy_vu = Some("brut".to_string());

        envoie_echec(&mut p, url, "brut").await;
        envoie_echec(&mut p, url, "brut").await;
        assert_eq!(p.echecs.get(url), Some(&2));

        p.icy_tx
            .send(IssueIcy {
                url: url.to_string(),
                brut: "brut".to_string(),
                // L'identité que le cœur aurait envoyée : c'est elle qui doit
                // repartir en écho, à l'identique.
                identite: json!({"kind": "stream", "url": url}),
                motif: None,
                valide: Some(("Artiste".to_string(), "Titre".to_string(), None)),
                couple: Some(("Artiste".to_string(), "Titre".to_string())),
            })
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.artist.as_deref(), Some("Artiste"));
        assert!(!p.echecs.contains_key(url), "le succes doit remettre le compteur a zero");

        envoie_echec(&mut p, url, "brut").await;
        envoie_echec(&mut p, url, "brut").await;
        assert!(!doit_resonder(&p.echecs, url), "compteur consecutif (2), pas cumulatif (4) : ne doit pas resonder");
    }

    #[test]
    fn une_identite_qui_nest_pas_un_flux_nest_pas_traitee() {
        assert!(url_de_flux(&json!({"kind":"disc","toc":"1 2 3"})).is_none());
        assert!(url_de_flux(&json!({"kind":"stream"})).is_none());
        assert!(url_de_flux(&json!({"kind":"stream","url":""})).is_none());
        assert_eq!(url_de_flux(&json!({"kind":"stream","url":"http://f"})).as_deref(), Some("http://f"));
    }
}
