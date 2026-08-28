//! Plugin `metadata` : reconnaît un disc auprès de MusicBrainz, et sert
//! aussi de relai générique de cover pour tout le reste.
//!
//! Deux intentions cohabitent dans ce seul binaire :
//! - le **path disc** ne réagit qu'aux identités de disc
//!   (`kind: "disc"`), interroge MusicBrainz **une fois par disc**, et émet
//!   ensuite un enrichment par track depuis ce qu'il a appris. Il connaît
//!   la TOC, donc il sait ce qui plays : il écrase (`fill_only: false`).
//! - le **path générique** search une cover dès que le cœur announcement un
//!   artist et un album connus, quelle que soit la Source. Il ne sait rien
//!   de plus que ce qu'on lui a donné, donc il ne fait que **compléter**
//!   (`fill_only: true`) : le cœur ne perd rien à ignorer sa réponse si un
//!   autre contributeur tient déjà une cover.
//!
//! Ce code vivait dans le plugin cd, où un appel réseau de plusieurs secondes
//! partageait le processus qui doit répondre aux commands de track. Ici, son
//! échec ou sa lenteur ne concernent que les métadonnées.

mod admin;
mod icy;
mod patterns;
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
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

/// SourcesCatalog i18n embarqué de la page d'admin (`admin.rs`). Nommé comme
/// `MPD_EN` côté greffon mpd : c'est ce name que `Catalog::load` embarque en
/// dernier recours si aucun pack externe n'est présent.
pub(crate) const MUSICBRAINZ_EN: &str = include_str!("locales/en.toml");

/// Échecs de validation **consécutifs** avant de resonder une station déjà
/// connue.
///
/// Un track que MusicBrainz ne connaît pas est un échec parfaitement
/// légitime sur un pattern juste : resonder au premier échec ferait partir un
/// sondage sur chaque title obscur, et — puisque l'order inverse rend parfois
/// lui aussi un résultat acceptable — pourrait remplacer un bon pattern par un
/// mauvais sur un seul coup de chance. Trois échecs d'affilée décrivent une
/// station qui a changé de forme, pas un title que le sources_catalog ignore.
const FAILURES_BEFORE_REPROBE: u32 = 3;

/// Le name sous lequel le cœur connaît l'en-tête d'un stream.
///
/// Déclaré en `derived_from` par les deux enrichments du path ICY : ce
/// greffon **découpe** cette chaîne, il ne l'apporte pas. Voir
/// `Enrichment::derived_from`.
const SOURCE_ICY: &str = "icy";

/// Délais des reprises différées d'une recherche de cover, en secondes.
///
/// **Trois, très espacées.** `search_release` a déjà réessayé trois fois en
/// interne (2 s puis 4 s) : ce qui arrive ici est une panne qui dure plus que
/// quelques secondes, pas un hoquet.
///
/// Mesuré sur l'appareil le 2026-08-28 : six 503 sur neuf requêtes en une
/// minute, la cover n'arrivant qu'à la sixième — trente-six secondes après
/// le début du track. La cadence, elle, était conforme (1,1 s entre requêtes,
/// étrangleur partagé), donc ces 503 viennent du serveur de recherche de
/// MusicBrainz et rien de ce qu'on fait ne les évitera.
///
/// La troisième reprise à trois minutes est donc un filet pour une mauvaise
/// passe qui dure : elle coûte une requête toutes les trois minutes au pire,
/// très loin de la requête par seconde que le service autorise, et elle tient
/// dans la durée d'un track. Au delà, l'absence cesse d'être une panne et le
/// changement de track reste la reprise ultime.
const COVER_RETRIES_S: &[u64] = &[20, 60, 180];

/// Résultat d'une interrogation : la TOC concernée, et ce qu'on a trouvé.
/// Ce qu'une interrogation de MusicBrainz rapporte.
///
/// Un enum et non un `Option`, et c'est le correctif d'un defaut mesure :
/// « le service n'a pas repondu » et « il a repondu qu'il ne connait pas »
/// demandent deux traitements opposes.
///
/// Le second se **memorise** — c'est meme tout l'interet des caches de ce
/// greffon : ne pas redemander douze fois de suite pour un disc inconnu. Le
/// premier ne doit surtout pas l'etre. Une version anterieure les confondait
/// derriere un `Option`, et un 503 passager de MusicBrainz — leurs serveurs en
/// rendent sous leur propre load, meme a cadence respectee — se figeait alors
/// en « cet album n'a pas de cover » jusqu'au redemarrage du greffon.
/// Symptome rapporte par le owner, et reproduit : redemarrer le greffon
/// faisait apparaitre la cover.
#[derive(Debug, Clone, PartialEq)]
enum Answer<T> {
    /// MusicBrainz a repondu. `None` = il ne connait pas, et c'est definitif.
    Known(Option<T>),
    /// Aucune reponse exploitable apres les tentatives bornees. Rien a
    /// memoriser : le prochain passage relancera la recherche.
    Unavailable,
}

/// Ce qu'un disc interroge a rendition, tel qu'il est **memorise**.
type Found = (String, Option<DiscInfo>);

/// Ce que la tache d'interrogation d'un disc fait traverser au canal.
type DiscOutcome = (String, Answer<DiscInfo>);

/// Couple qui identifie une recherche du relai générique : artist, puis
/// album. C'est aussi la clé de mémorisation (voir `MusicBrainzPlugin`).
type GenericKey = (String, String);

/// Résultat d'une recherche générique : le pair concerné, et le MBID trouvé.
type FoundCover = (GenericKey, Option<String>);

/// Ce que la tache de recherche de cover fait traverser au canal.
type CoverOutcome = (GenericKey, Answer<String>);

/// Ce qu'une identité de disc learn à ce plugin.
#[derive(Debug, Clone, PartialEq)]
struct Disc {
    toc: String,
    track: usize,
}

/// Lit une identité opaque et n'en retient un disc que si elle en décrit un.
///
/// Fonction pure : c'est le point d'entrée de données venues d'un autre
/// processus, donc l'endroit où une forme inattendue doit être écartée sans
/// bruit plutôt que de faire paniquer le plugin.
fn disc_of(identity: &Value) -> Option<Disc> {
    if identity.get("kind").and_then(Value::as_str)? != "disc" {
        return None;
    }
    let toc = identity.get("toc").and_then(Value::as_str)?.trim();
    if toc.is_empty() {
        return None;
    }
    // Une identité de disc sans index de track n'est pas exploitable : on ne
    // saurait pas quel title annoncer.
    let track = identity.get("track").and_then(Value::as_u64)? as usize;
    Some(Disc { toc: toc.to_string(), track })
}

/// Lit une identité opaque et n'en retient l'URL que si elle décrit un stream.
///
/// Fonction pure, même contrat que [`disc_of`] : une forme inattendue est
/// écartée sans bruit plutôt que de faire paniquer le plugin.
fn stream_url(identity: &Value) -> Option<String> {
    if identity.get("kind").and_then(Value::as_str)? != "stream" {
        return None;
    }
    let url = identity.get("url").and_then(Value::as_str)?.trim();
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

/// Faut-il chercher une cover pour cet état partiel ?
///
/// Un artist **et** un album, jamais un title ICY seul : ce dernier est un
/// texte raw, non découpé exprès dans ce projet, et OUI FM émet
/// `Titre - ARTISTE` dans l'order inverse de l'usage — le donner à MusicBrainz
/// rendrait n'importe quoi avec assurance.
///
/// Et rien à faire si une cover est déjà tenue : ce greffon **complète**,
/// donc l'appel serait jeté par l'arbitrage du cœur — une requête dont
/// l'inutilité est connue d'avance.
fn should_search(known: &ritornello_proto::Known) -> bool {
    !known.cover && known.artist.is_some() && known.album.is_some()
}

struct MusicBrainzPlugin {
    /// Identité courante, réémise en écho dans chaque enrichment — c'est le
    /// garde-fou de péremption côté cœur.
    identity: Option<Value>,
    disc: Option<Disc>,
    /// Dernier disc interrogé : TOC brute → résultat (`None` = interrogé,
    /// rien trouvé). Un seul disc suffit : il n'y a qu'un tiroir. Mémoriser
    /// aussi les échecs évite de réinterroger MusicBrainz à chaque changement
    /// de track d'un disc inconnu — douze pistes, douze requêtes inutiles.
    known: Option<Found>,
    /// TOC dont l'interrogation est en vol, pour ne pas la lancer deux fois.
    in_flight: Option<String>,
    /// Enrichissement prêt à partir. Un seul suffit : les deux chemins sont
    /// mutuellement exclusifs (une identité est un disc, ou ne l'est pas).
    ready: Option<Enrichment>,
    found_tx: mpsc::Sender<DiscOutcome>,
    found_rx: mpsc::Receiver<DiscOutcome>,

    // --- Relai générique (fichier sans cover, stream dont les métadonnées
    // textuelles suffisent...) ---
    /// Identité courante pour ce path, réémise en écho. `None` = rien à
    /// compléter maintenant (path disc active, artist/album pas encore
    /// connus tous les deux, ou cover déjà tenue).
    generic_identity: Option<Value>,
    /// Couple (artist, album) actuellement visé. C'est la clé de
    /// mémorisation choisie : c'est exactement ce que porte la requête
    /// MusicBrainz, elle change dès qu'on change d'album (donc jamais de
    /// cover d'un autre album qui survit au changement de track), et elle
    /// reste stable tant que l'album ne change pas (donc pas une requête par
    /// trame reçue). Une identité de Source ne convenait pas : elle peut
    /// rester fixe pendant que artist/album arrivent en plusieurs trames
    /// (ICY), ou changer sans que l'album change (track suivante du même
    /// disc de fichiers).
    generic_key: Option<GenericKey>,
    /// Dernier pair recherché, et l'URL de cover trouvée (`None` =
    /// recherche faite, rien trouvé). Mémoriser aussi les échecs évite de
    /// réinterroger MusicBrainz à chaque trame tant que l'album ne change pas.
    known_cover: Option<FoundCover>,
    /// Couple dont la recherche est en vol, pour ne pas la lancer deux fois.
    cover_in_flight: Option<GenericKey>,
    /// Le pair en cours de **reprise différée** et le nombre de reprises déjà
    /// consommées. Voir [`MusicBrainzPlugin::reschedule_cover`].
    cover_retries: Option<(GenericKey, usize)>,
    cover_tx: mpsc::Sender<CoverOutcome>,
    cover_rx: mpsc::Receiver<CoverOutcome>,

    // --- Chemin ICY (radio) ---
    /// Le store, **partagé avec la page d'admin** : les deux moitiés du
    /// processus le lisent et l'écrivent, comme les deux moitiés du greffon
    /// radio partagent son fichier d'état.
    store: Arc<RwLock<patterns::Store>>,
    state_path: PathBuf,
    /// Dernière chaîne brute traitée. Icecast répète le même en-tête tout au
    /// long d'un track : sans cette garde, chaque répétition relancerait une
    /// requête.
    icy_seen: Option<String>,
    /// Échecs de validation **consécutifs**, par URL de stream. En mémoire et
    /// non persisté : c'est une suite d'événements en cours, pas un fait acquis
    /// sur la station, et un redémarrage est une remise à zéro légitime.
    failures: HashMap<String, u32>,
    /// URL dont un traitement est en vol, pour ne pas le lancer deux fois.
    icy_in_flight: Option<String>,
    icy_tx: mpsc::Sender<IcyOutcome>,
    icy_rx: mpsc::Receiver<IcyOutcome>,
}

/// Ce qu'une tâche de traitement ICY rapporte, en **un seul** message.
///
/// Un message et non deux (« voici le pattern », « voici le pair ») : la
/// boucle doit pouvoir mettre à jour le store, le compteur d'échecs et
/// l'enrichment dans le même tour, sans état intermédiaire où le pattern
/// serait retenu mais le compteur pas encore remis à zéro.
#[derive(Debug)]
struct IcyOutcome {
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
    ///   un rejet **silencieux** de chaque enrichment.
    /// * **Attachée au travail, pas au greffon.** Un champ de `self` serait
    ///   écrasé par une trame plus récente pendant qu'un traitement vole
    ///   encore, et l'issue d'un ancien track repartirait avec l'identité du
    ///   nouveau. La faire voyager lie l'écho à ce qu'il décrit.
    identity: Value,
    /// La chaîne traitée. Sert de garde de péremption : une issue qui ne
    /// décrit pas la chaîne courante est jetée, comme les deux autres chemins
    /// jettent une réponse qui ne décrit plus ce qui plays.
    raw: String,
    /// Le pattern à retenir quand un sondage a eu lieu. `None` = pas de
    /// sondage (régime établi), donc rien à apprendre.
    pattern: Option<patterns::Pattern>,
    /// Le pair validé et sa cover. `None` = validation échouée.
    validated: Option<(String, String, Option<String>)>,
    /// Le pair issu du découpage **local**, que la validation ait abouti ou
    /// non.
    ///
    /// Distinct de `validated`, et la distinction porte une correction de
    /// relecture : un track que MusicBrainz ne connaît pas est un échec de
    /// **validation**, pas une reason de jeter un découpage dont le pattern a
    /// déjà fait ses preuves sur cette station. Sans ce champ, le greffon
    /// n'émettait rien dans ce cas — et comme l'identité d'une radio ne change
    /// pas d'un track à l'autre, l'enrichment du track **précédent**
    /// restait winner : l'écran annonçait l'artist, le title et la cover
    /// d'avant pendant toute la durée du suivant.
    pair: Option<(String, String)>,
}

impl MusicBrainzPlugin {
    fn new(store: Arc<RwLock<patterns::Store>>, state_path: PathBuf) -> Self {
        let (found_tx, found_rx) = mpsc::channel(4);
        let (cover_tx, cover_rx) = mpsc::channel(4);
        let (icy_tx, icy_rx) = mpsc::channel(4);
        Self {
            identity: None,
            disc: None,
            known: None,
            in_flight: None,
            ready: None,
            found_tx,
            found_rx,
            generic_identity: None,
            generic_key: None,
            known_cover: None,
            cover_in_flight: None,
            cover_retries: None,
            cover_tx,
            cover_rx,
            store,
            state_path,
            icy_seen: None,
            failures: HashMap::new(),
            icy_in_flight: None,
            icy_tx,
            icy_rx,
        }
    }

    /// Prépare l'enrichment de la track courante si le disc est known.
    fn prepare(&mut self) {
        let (Some(identity), Some(disc)) = (&self.identity, &self.disc) else { return };
        let Some((toc, Some(info))) = &self.known else { return };
        if toc != &disc.toc {
            return;
        }
        let Some(title) = info.tracks.get(disc.track) else {
            // Index hors bornes : le disc reconnu n'a pas ce nombre de pistes.
            // Mieux vaut se taire que d'annoncer le title d'une autre track.
            tracing::info!("track {} beyond the {} known titles", disc.track, info.tracks.len());
            return;
        };
        self.ready = Some(Enrichment {
            identity: identity.clone(),
            artist: Some(info.artist.clone()),
            title: Some(title.clone()),
            album: Some(info.album.clone()),
            // Le lookup par TOC porte la date du pressage : l'annee est donc
            // gratuite sur le path disc, sans requete de plus.
            year: info.year,
            // MusicBrainz donnerait les durées avec `inc=recordings`, mais la
            // durée n'est pas affichée : rien ne justifie d'alourdir la requête.
            duration_s: None,
            // Ce plugin ne sait pas où en est la playback : il répond
            // sur l'identité d'un track, pas sur son déroulement.
            position_s: None,
            // Le lookup par TOC portait déjà de quoi construire l'URL, et le
            // choix du niveau (ce pressage, ou l'album à défaut de face avant)
            // a été fait à l'analyse. Aucune requête de plus ici.
            cover: info.cover_url.clone().map(|url| CoverRef::Url { url }),
            // Chemin disc : la TOC dit ce qui plays, donc il écrase (défaut).
            ..Default::default()
        });
    }

    /// Prépare l'enrichment générique pour le pair (artist, album)
    /// actuellement visé : la cover trouvée, ou l'aveu de n'avoir rien
    /// trouvé.
    ///
    /// **Le second cas est aussi une réponse**, et c'est ce qui manquait :
    /// « MusicBrainz n'a pas de cover pour cet album » et « MusicBrainz n'a
    /// jamais été interrogé » se voyaient pareil à l'écran — c'est-à-dire pas
    /// du tout. Un enrichment portant `searched` et rien d'autre est le
    /// seul que le cœur accepte clear ; il n'entre dans aucun arbitrage et
    /// n'add qu'une line à la provenance.
    ///
    /// À ne pas confondre avec une **panne** : celle-là n'émet rien et se
    /// reprogramme (voir `reschedule_cover`). Ce qui arrive ici est une
    /// réponse effective du service.
    fn prepare_generic(&mut self) {
        let (Some(identity), Some(key)) = (&self.generic_identity, &self.generic_key) else {
            return;
        };
        let Some((known, trouvee)) = &self.known_cover else { return };
        if known != key {
            return;
        }
        let Some(cover_url) = trouvee else {
            self.ready = Some(Enrichment {
                identity: identity.clone(),
                searched: true,
                // `fill_only` par honnêteté de forme : ce contributeur-là
                // n'apporte rien, il ne peut donc rien vouloir écraser. Sans
                // effet pratique — un enrichment clear est écarté de
                // l'arbitrage des deux côtés — mais un défaut `false`
                // signifierait « j'écrase », ce qui serait faux.
                fill_only: true,
                ..Default::default()
            });
            return;
        };
        self.ready = Some(Enrichment {
            identity: identity.clone(),
            // URL déjà résolue par `search_release` : ce path ne rebâtit
            // rien. Une recherche ne porte pas de bloc `cover-art-archive`,
            // donc c'est la cover de l'album qui en sort.
            cover: Some(CoverRef::Url { url: cover_url.clone() }),
            // Il a cherché, et il a trouvé : le dire aussi, pour que la
            // provenance sache qu'il a été interrogé.
            searched: true,
            // Ce path ne sait rien de plus que ce qu'on lui a donné : il ne
            // fait que compléter, jamais écraser un champ déjà renseigné.
            fill_only: true,
            ..Default::default()
        });
    }

    /// Lance la recherche d'une cover pour ce pair (artist, album), une
    /// seule fois — même pattern que [`Self::search`] pour le disc.
    fn search_cover(&mut self, key: GenericKey) {
        if self.cover_in_flight.as_ref() == Some(&key) {
            return;
        }
        if self.known_cover.as_ref().is_some_and(|(connue, _)| connue == &key) {
            return; // déjà recherché, résultat mémorisé (trouvé ou non)
        }
        self.start_cover_search(key, Duration::ZERO);
    }

    /// Reprogramme la recherche après une panne de MusicBrainz, une fois le
    /// budget de reprises non épuisé.
    ///
    /// **Ce que ceci répare, et pourquoi les trois tentatives ne suffisaient
    /// pas.** `search_release` réessaie déjà trois fois en interne, à 2 s puis
    /// 4 s — une poignée de secondes en tout. Si la panne dure plus longtemps,
    /// la réponse est `Unavailable`, rien n'est mémorisé (c'est bien : un 503
    /// ne doit pas devenir « cet album n'a pas de cover »)... et **plus rien
    /// ne restart**. Le commentaire d'alors disait « la prochaine trame
    /// réessaiera », mais il n'y a pas de prochaine trame : le cœur ne
    /// republie `NowPlaying` que lorsque l'identité ou le `known` changent
    /// (voir `publish_state`), et sur un fichier local les deux se figent dès que
    /// les étiquettes sont lues. Le symptôme rapporté par le propriétaire est
    /// exactement celui-là : rien pendant dix secondes, puis la cover
    /// apparaît **au changement de track** — c'est-à-dire à la seule occasion
    /// qui relançait quoi que ce soit.
    ///
    /// Deux reprises et pas davantage : au-delà, l'absence n'est plus une
    /// panne passagère, et marteler un service tiers gratuit pour une image
    /// serait un abus. Le changement de track reste la reprise ultime, comme
    /// avant.
    ///
    /// Ne reprend que le pair **encore visé** : une reprise pour un album
    /// qu'on n'écoute plus est du travail pur perdu, et sa réponse serait de
    /// toute façon écartée par la garde de péremption.
    fn reschedule_cover(&mut self, key: GenericKey) {
        let Some((rank, timeout)) = self.retry_due(&key) else {
            tracing::info!("MusicBrainz still unavailable, giving up until the track changes");
            return;
        };
        self.cover_retries = Some((key.clone(), rank + 1));
        self.start_cover_search(key, timeout);
    }

    /// Le rank de la prochaine reprise pour ce pair et son délai, ou `None`
    /// — budget épuisé, ou pair qui n'est plus celui qu'on vise.
    ///
    /// Le rank sort d'ici plutôt que d'être recalculé par l'appelant : les deux
    /// valeurs viennent de la même playback, et les séparer laisserait un
    /// compteur avancer sur un rank qu'un autre pair avait posé.
    ///
    /// **Séparée de son application**, et c'est ce qui la rend vérifiable : la
    /// reprise elle-même dort puis interroge un service tiers, donc l'éprouver
    /// de bout en bout demanderait un réseau et une horloge. La décision, elle,
    /// ne read que deux champs.
    ///
    /// Le compteur est porté **par le pair** : un album différent repart de
    /// zéro sans qu'aucune remise à zéro explicite n'ait à exister, donc sans
    /// path où l'oublier.
    fn retry_due(&self, key: &GenericKey) -> Option<(usize, Duration)> {
        if self.generic_key.as_ref() != Some(key) {
            return None;
        }
        let rank = match &self.cover_retries {
            Some((precedent, rank)) if precedent == key => *rank,
            _ => 0,
        };
        COVER_RETRIES_S.get(rank).map(|s| (rank, Duration::from_secs(*s)))
    }

    /// Le vol lui-même, éventuellement précédé d'une attente.
    ///
    /// **`cover_in_flight` est armé avant l'attente**, pas après : sans cela une
    /// trame arrivant pendant la pause relancerait une seconde recherche pour
    /// le même pair, et les deux se répondraient.
    fn start_cover_search(&mut self, key: GenericKey, apres: Duration) {
        self.cover_in_flight = Some(key.clone());
        let (artist, album) = key.clone();
        let tx = self.cover_tx.clone();
        // Le depart d'une recherche, date. Avec l'throttler, les trois
        // tentatives internes et leurs delais de dix secondes, le temps entre
        // l'announcement d'un track et l'arrivee de sa cover se compte parfois
        // en dizaines de secondes : sans cette line, ce timeout n'etait
        // observable que sur l'ecran, et donc pas attribuable.
        if apres.is_zero() {
            tracing::info!("MusicBrainz: looking for a cover for {artist} — {album}");
        } else {
            tracing::info!("MusicBrainz: retrying {artist} — {album} in {apres:?}");
        }
        tokio::spawn(async move {
            if !apres.is_zero() {
                tokio::time::sleep(apres).await;
            }
            // Chronometre, et l'issue nommee : « trouvee », « rien trouve »
            // et « pas de reponse » se distinguent enfin, avec le temps que
            // chacune a coute. L'throttler, les trois tentatives internes et
            // leurs delais de dix secondes peuvent additionner des dizaines de
            // secondes — c'est l'hypothese a confirmer ou a ecarter.
            let debut = std::time::Instant::now();
            let reponse = match musicbrainz::search_release(&artist, &album).await {
                Ok(url) => {
                    let issue = if url.is_some() { "cover found" } else { "no cover" };
                    tracing::info!(
                        "MusicBrainz: {issue} for {artist} — {album} after {:?}",
                        debut.elapsed()
                    );
                    Answer::Known(url)
                }
                Err(e) => {
                    tracing::info!(
                        "MusicBrainz release search unavailable after {:?}: {e}",
                        debut.elapsed()
                    );
                    Answer::Unavailable
                }
            };
            let _ = tx.send((key, reponse)).await;
        });
    }

    /// Lance l'interrogation d'un disc inconnu, une seule fois.
    fn search(&mut self, toc: String) {
        if self.in_flight.as_deref() == Some(toc.as_str()) {
            return;
        }
        if let Some((connue, _)) = &self.known {
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
        self.in_flight = Some(toc.clone());
        let tx = self.found_tx.clone();
        tokio::spawn(async move {
            let reponse = match musicbrainz::lookup(&param, ntracks).await {
                Ok(info) => Answer::Known(info),
                Err(e) => {
                    tracing::info!("MusicBrainz lookup unavailable: {e}");
                    Answer::Unavailable
                }
            };
            let _ = tx.send((toc, reponse)).await;
        });
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for MusicBrainzPlugin {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Toute announcement périme l'enrichment préparé : il portait l'identité
        // précédente, et le cœur le jetterait de toute façon.
        self.ready = None;
        let disc = np.identity.as_ref().and_then(disc_of);
        match disc {
            Some(disc) => {
                self.identity = np.identity;
                // Le path disc est exclusif : sur un disc, rien à
                // compléter par le relai générique.
                self.generic_identity = None;
                self.generic_key = None;
                let toc = disc.toc.clone();
                self.disc = Some(disc);
                self.search(toc);
                self.prepare();
            }
            None => {
                // Ni disc, ni arrêt : une identité de fichier ou de stream
                // radio, par exemple. Le path disc se tait — c'est
                // l'affaire d'un autre plugin — mais le relai générique peut
                // avoir de quoi chercher une cover.
                self.identity = None;
                self.disc = None;
                // Capturés avant que le traitement générique ci-dessous ne
                // déplace `np.identity` : le path ICY en a besoin après.
                let url_flux = np.identity.as_ref().and_then(stream_url);
                let stream_title = np.known.stream_title.clone();
                // Clonée ici, avec ses voisines, parce que le `match` ci-dessous
                // déplace `np.identity` : c'est cette valeur-là qui repartira en
                // écho, jamais une reconstruction. Voir `IcyOutcome::identity`.
                let identite_flux = np.identity.clone();
                match np.identity {
                    Some(identity) if should_search(&np.known) => {
                        let key = (
                            np.known.artist.expect("verifie par should_search"),
                            np.known.album.expect("verifie par should_search"),
                        );
                        self.generic_identity = Some(identity);
                        self.generic_key = Some(key.clone());
                        self.search_cover(key);
                        self.prepare_generic();
                    }
                    _ => {
                        self.generic_identity = None;
                        self.generic_key = None;
                    }
                }

                // --- Chemin ICY : après le traitement générique qui précède,
                // sans y toucher --------------------------------------------
                //
                // Déclenché sur un changement de `stream_title`, pas sur
                // chaque trame : Icecast répète le même en-tête tout au long
                // d'un track, et le retraiter à chaque fois serait une
                // requête pour rien.
                if let Some(url) = url_flux {
                    if stream_title != self.icy_seen {
                        self.icy_seen = stream_title.clone();
                        if let Some(raw) = stream_title {
                            // `icy_in_flight` empêche de lancer un second
                            // traitement pour la même URL pendant qu'un
                            // premier vole encore ; la garde de péremption
                            // dans `next_enrichment` filtre une réponse
                            // devenue hors sujet le temps du vol.
                            if self.icy_in_flight.as_deref() != Some(url.as_str()) {
                                self.icy_in_flight = Some(url.clone());
                                // **Une station au pattern manuel n'est jamais
                                // resondee.** Le store refusait bien de
                                // reecrire l'entry (`Store::learn`), mais
                                // rien n'empechait le sondage de partir — et
                                // alors c'etait *son* decoupage qui s'affichait,
                                // pas celui de l'operateur. La documentation
                                // etait donc vraie du fichier et fausse de
                                // l'ecran. Consulter l'origin ici ferme l'ecart
                                // a la source : si l'operateur a tranche, on
                                // apply ce qu'il a pose, meme quand
                                // MusicBrainz n'en veut pas.
                                let manuel = self
                                    .store
                                    .read()
                                    .await
                                    .entry(&url)
                                    .map(|e| e.origin == patterns::Origin::Manual)
                                    .unwrap_or(false);
                                let reprobe = !manuel && should_reprobe(&self.failures, &url);
                                if reprobe {
                                    // **Le resondage consomme le compteur.**
                                    // Sans ça il restait au-dessus du seuil
                                    // pour la vie du processus : une station
                                    // qui ne validated jamais — un stream en
                                    // mojibake, par exemple — repartait en
                                    // sondage complet à *chaque* title, ce qui
                                    // démentait la documentation et faisait de
                                    // cette limite une tempête de requêtes
                                    // garantie. Un resondage rachète trois
                                    // titres, il ne s'arme pas en permanence.
                                    self.failures.remove(&url);
                                }
                                let store = self.store.clone();
                                let tx = self.icy_tx.clone();
                                let url_tache = url.clone();
                                // `stream_url` a déjà reconnu l'identité, donc
                                // elle est là : l'`unwrap_or` n'est qu'une
                                // totalité de type, pas un cas d'usage.
                                let identity = identite_flux.clone().unwrap_or(Value::Null);
                                tokio::spawn(async move {
                                    let known = store.read().await.entry(&url_tache).map(|e| e.pattern.clone());
                                    let issue = handle_icy(url_tache, raw, identity, known, reprobe).await;
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
            if let Some(e) = self.ready.take() {
                return e;
            }
            // `select!` sur deux `recv` reste annulable sans perte : si un
            // `NowPlaying` arrive d'abord, le runner abandonne ce futur et
            // aucun résultat n'est perdu — chaque branche ne mute `self`
            // qu'une fois son message reçu, jamais avant (l'état durable vit
            // dans `self`, pas dans les variables locales de ce futur).
            tokio::select! {
                r = self.found_rx.recv() => match r {
                    Some((toc, reponse)) => {
                        if self.in_flight.as_deref() == Some(toc.as_str()) {
                            self.in_flight = None;
                        }
                        // Une panne passagère ne se mémorise pas : `in_flight`
                        // vient d'être libéré, donc le prochain changement de
                        // track relancera l'interrogation de ce disc.
                        let Answer::Known(info) = reponse else { continue };
                        // Un résultat n'est retenu que s'il décrit le disc
                        // suivi : deux lookups peuvent se croiser lors d'un
                        // échange rapide de disques (A en vol, B inséré, réponse
                        // de B puis celle de A), et retenir le retardataire
                        // écrasait le cache du disc courant — `prepare()`
                        // protégeait l'affichage, mais le prochain changement de
                        // track relançait une requête MusicBrainz pour rien.
                        if self.disc.as_ref().is_some_and(|d| d.toc == toc) {
                            self.known = Some((toc, info));
                            self.prepare();
                        }
                    }
                    // Impossible en pratique (le plugin garde un Sender) : ne pas
                    // rendre la main plutôt que de boucler à clear.
                    None => std::future::pending().await,
                },
                r = self.cover_rx.recv() => match r {
                    Some((key, reponse)) => {
                        if self.cover_in_flight.as_ref() == Some(&key) {
                            self.cover_in_flight = None;
                        }
                        // Une panne passagère ne se mémorise pas : un 503 de
                        // MusicBrainz ne doit pas devenir « cet album n'a pas de
                        // cover » pour toute la durée de l'album.
                        //
                        // **Et elle est reprogrammée**, ce qui manquait : rien
                        // ne relançait la recherche tant que la track ne
                        // changeait pas, faute de nouvelle trame à attendre
                        // (voir `reschedule_cover`).
                        let Answer::Known(cover_url) = reponse else {
                            self.reschedule_cover(key);
                            continue;
                        };
                        // Même garde que côté disc : ne retenir le résultat
                        // que s'il décrit le pair (artist, album) toujours
                        // visé — un changement de track peut avoir rendition la
                        // recherche en vol obsolète pendant qu'elle volait.
                        if self.generic_key.as_ref() == Some(&key) {
                            self.known_cover = Some((key, cover_url));
                            self.prepare_generic();
                        }
                    }
                    None => std::future::pending().await,
                },
                r = self.icy_rx.recv() => match r {
                    Some(issue) => {
                        if self.icy_in_flight.as_deref() == Some(issue.url.as_str()) {
                            self.icy_in_flight = None;
                        }
                        // **Le pattern est retenu avant la garde de
                        // péremption**, et l'order est le correctif : un pattern
                        // décrit la **station**, pas le track. Une issue de
                        // sondage devenue périmée pendant son vol — la station a
                        // changé de title, ce qui prend quelques secondes et le
                        // sondage en prend quatre — porte quand même un
                        // apprentissage valable, vérifié contre MusicBrainz.
                        //
                        // Jeter l'issue entière avant cette line, comme le
                        // faisait la version d'avant, pouvait faire qu'une
                        // station n'apprenne **jamais rien** : chaque sondage
                        // était invalidé par le changement de title qui l'avait
                        // en partie provoqué.
                        if let Some(m) = issue.pattern {
                            let mut store = self.store.write().await;
                            store.learn(&issue.url, m);
                            if let Err(e) = store.save(&self.state_path) {
                                tracing::warn!("could not save ICY patterns: {e}");
                            }
                        }
                        // Garde de péremption, comme les deux autres chemins,
                        // mais elle ne protège plus que ce qui décrit **le
                        // track** : le pair et la cover.
                        if self.icy_seen.as_deref() != Some(issue.raw.as_str()) {
                            continue;
                        }
                        match issue.validated {
                            Some((artist, title, cover_url)) => {
                                {
                                    let mut store = self.store.write().await;
                                    store.record_success(&issue.url);
                                    if let Err(e) = store.save(&self.state_path) {
                                        tracing::warn!("could not save ICY patterns: {e}");
                                    }
                                }
                                self.failures.remove(&issue.url);
                                self.ready = Some(Enrichment {
                                    // L'identité **reçue**, reportée telle
                                    // quelle. Voir `IcyOutcome::identity`, qui dit
                                    // pourquoi elle voyage avec le travail.
                                    identity: issue.identity,
                                    // **La station reste la source.** Ce
                                    // greffon a decoupe sa chaine et verifie le
                                    // decoupage, il n'a appris le track a
                                    // personne : s'attribuer le title effacerait
                                    // celui qui l'announcement. Le coeur note a part
                                    // qui a retravaille.
                                    derived_from: Some(SOURCE_ICY.to_string()),
                                    artist: Some(artist),
                                    title: Some(title),
                                    // URL déjà résolue par `first_recording`.
                                    cover: cover_url.map(|url| CoverRef::Url { url }),
                                    // Ce path **remplace** la chaîne ICY
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
                                *self.failures.entry(issue.url.clone()).or_default() += 1;
                                // **Émettre quand même.** Ne rien send_frame
                                // laissait l'enrichment du track
                                // *précédent* gagner l'arbitrage, l'identité
                                // d'une radio ne changeant pas d'un track à
                                // l'autre : l'écran annonçait l'artist, le
                                // title et la cover d'avant pendant toute la
                                // durée du suivant. Le pire des trois états, et
                                // celui que ma spec décrivait sans le voir en
                                // écrivant « ne rien émettre pour ce track ».
                                //
                                // Ce qu'on émet dépend de ce qu'on sait :
                                //
                                // * le pair local, quand le pattern s'apply.
                                //   MusicBrainz ne connaît pas ce track, ce
                                //   qui ne dit rien contre un découpage déjà
                                //   confirmé sur cette station. Sans cover,
                                //   faute de release à citer.
                                // * sinon la chaîne nettoyée en guise de title :
                                //   le pattern ne s'apply plus (la station a
                                //   changé de forme) ou il n'y en a pas. On
                                //   n'affirme alors aucun découpage — juste ce
                                //   que le stream announcement, débarrassé de sa
                                //   réclame.
                                let (artist, title) = match issue.pair {
                                    Some((a, t)) => (Some(a), Some(t)),
                                    None => (None, Some(icy::clean(&issue.raw))),
                                };
                                self.ready = Some(Enrichment {
                                    identity: issue.identity,
                                    // Encore plus vrai ici qu'au-dessus : ce
                                    // path ne porte **que** le decoupage
                                    // local, MusicBrainz n'ayant rien validated du
                                    // tout. Le title vient de la station, mot
                                    // pour mot.
                                    derived_from: Some(SOURCE_ICY.to_string()),
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
/// Extrait en fonction pure pour la même reason que [`best_accepted`] : le
/// réseau n'est pas joignable en test, donc c'est la **décision** qui doit
/// être éprouvée, pas le sondage qu'elle déclenche. Le seuil est en échecs
/// **consécutifs** : voir [`FAILURES_BEFORE_REPROBE`].
fn should_reprobe(failures: &HashMap<String, u32>, url: &str) -> bool {
    failures.get(url).copied().unwrap_or(0) >= FAILURES_BEFORE_REPROBE
}

/// Diagnostique un encodage douteux, sans le réparer.
///
/// Un title en mojibake ne validera **jamais** contre MusicBrainz, et
/// ressemblerait sinon à un mauvais découpage alors que le découpage était
/// bon : sans ce diagnostic distinct, on chercherait le défaut du mauvais
/// côté.
fn warn_dubious_encoding(raw: &str) {
    // `U+FFFD` : le caractère de remplacement qu'un décodage UTF-8 forcé sur
    // des bytes qui n'en sont pas laisse derrière lui.
    if raw.contains('\u{FFFD}') {
        tracing::warn!("ICY stream title looks mis-decoded (replacement character present): {raw:?}");
        return;
    }
    // Séquence caractéristique d'un texte relu dans le mauvais jeu de
    // caractères : les deux bytes d'un caractère accentué UTF-8 (tête
    // 0xC2/0xC3, puis un octet de continuation 0x80-0xBF) se relisent
    // ailleurs comme « Â »/« Ã » suivi d'un symbole Latin-1 Supplement — « Ã©
    // » pour un « é », par exemple.
    let douteux =
        raw.chars().zip(raw.chars().skip(1)).any(|(a, b)| matches!(a, 'Â' | 'Ã') && ('\u{80}'..='\u{BF}').contains(&b));
    if douteux {
        tracing::warn!("ICY stream title looks mis-decoded (latin-1/UTF-8 mismatch): {raw:?}");
    }
}

/// Un candidat est-il validé par cette réponse ?
///
/// Les deux conditions comptent : le score seul est trop généreux, la
/// recherche MusicBrainz rendant presque toujours quelque chose de plausible.
/// L'égalité de title normalisée est la garde qui porte tout.
fn validated(titre_candidat: &str, e: &musicbrainz::Recording) -> bool {
    e.score >= musicbrainz::RECORDING_THRESHOLD && musicbrainz::normalize(&e.title) == musicbrainz::normalize(titre_candidat)
}

/// Choisit le meilleur candidat accepté parmi des réponses déjà obtenues.
///
/// Séparée du réseau exprès : c'est la décision, et c'est elle qui doit être
/// éprouvée. Les paires sont `(candidat, réponse)`, dans l'order d'essai.
fn best_accepted(essais: &[(icy::Candidate, Option<musicbrainz::Recording>)]) -> Option<&icy::Candidate> {
    essais
        .iter()
        .filter_map(|(c, reponse)| reponse.as_ref().filter(|e| validated(&c.title, e)).map(|e| (c, e.score)))
        .max_by_key(|(_, score)| *score)
        .map(|(c, _)| c)
}

/// Valide un pair déjà découpé localement, par une recherche
/// d'enregistrement.
///
/// C'est la validation continue du régime établi (voir la doc du module) :
/// elle sert aussi à trouver la cover, qu'une radio n'announcement jamais
/// autrement.
async fn validated_by_search(artist: &str, title: &str) -> Option<(String, String, Option<String>)> {
    let reponse = musicbrainz::search_recording(artist, title)
        .await
        .unwrap_or_else(|e| {
            tracing::info!("MusicBrainz recording search: {e}");
            None
        })?;
    if validated(title, &reponse) {
        Some((artist.to_string(), title.to_string(), reponse.cover_url))
    } else {
        None
    }
}

/// Traite une chaîne ICY : apply le pattern known, ou sonde la station.
///
/// Détachée dans une tâche, comme les deux autres chemins : une station peut
/// coûter quatre requêtes espacées d'une seconde, et la boucle du greffon ne
/// doit pas attendre.
async fn handle_icy(
    url: String,
    raw: String,
    identity: Value,
    known: Option<patterns::Pattern>,
    reprobe: bool,
) -> IcyOutcome {
    warn_dubious_encoding(&raw);
    let nettoye = icy::clean(&raw);

    if !reprobe {
        match &known {
            Some(patterns::Pattern::DoNotSplit) => {
                // La station parlée : coût nul, aucune requête.
                return IcyOutcome { url, raw, identity, pattern: None, validated: None, pair: None };
            }
            Some(m @ patterns::Pattern::Split { .. }) => {
                // Régime établi : découpage local, une seule requête qui vaut
                // à la fois validation continue et recherche de cover.
                //
                // Le pair local est rapporté **même si la validation
                // échoue** : c'est notre meilleure connaissance du track, et
                // le pattern qui l'a produit a déjà été confirmé sur cette
                // station. Voir `IcyOutcome::pair`.
                let pair = icy::apply(m, &nettoye);
                let validated = match &pair {
                    Some((artist, title)) => validated_by_search(artist, title).await,
                    None => None,
                };
                return IcyOutcome { url, raw, identity, pattern: None, validated, pair };
            }
            None => {} // Station jamais sondée : tombe dans le sondage ci-dessous.
        }
    }

    // Sondage : station inconnue, ou resondage déclenché par trois échecs
    // d'affilée.
    let candidates = icy::candidates(&nettoye);
    let mut essais = Vec::with_capacity(candidates.len());
    for c in candidates {
        let reponse = musicbrainz::search_recording(&c.artist, &c.title).await.unwrap_or_else(|e| {
            tracing::info!("MusicBrainz recording search: {e}");
            None
        });
        essais.push((c, reponse));
    }
    let nb_essayes = essais.len();
    // Un cap silencieux se read comme « on a tout essayé » : le dire
    // quand le nombre de candidates sondés touche le cap de icy::candidates.
    if nb_essayes >= icy::MAX_CANDIDATES {
        tracing::info!(
            "ICY probe for {url}: hit the {}-candidate cap, some derivable candidates may not have been tried",
            icy::MAX_CANDIDATES
        );
    }
    match best_accepted(&essais).cloned() {
        Some(winner) => {
            let score = essais.iter().find(|(c, _)| *c == winner).and_then(|(_, r)| r.as_ref()).map(|e| e.score);
            let cover_url =
                essais.iter().find(|(c, _)| *c == winner).and_then(|(_, r)| r.as_ref()).and_then(|e| e.cover_url.clone());
            tracing::info!(
                "ICY probe for {url}: tried {nb_essayes} candidate(s), kept \"{}\" / \"{}\" (score {:?})",
                winner.artist,
                winner.title,
                score
            );
            IcyOutcome {
                url,
                raw,
                identity,
                pattern: Some(patterns::Pattern::from_candidate(&winner)),
                validated: Some((winner.artist.clone(), winner.title.clone(), cover_url)),
                pair: Some((winner.artist, winner.title)),
            }
        }
        None => {
            tracing::info!("ICY probe for {url}: tried {nb_essayes} candidate(s), none accepted");
            IcyOutcome {
                url,
                raw,
                identity,
                pattern: Some(patterns::Pattern::DoNotSplit),
                validated: None,
                pair: None,
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let state_path = PathBuf::from(
        std::env::var("RITORNELLO_MUSICBRAINZ_STATE")
            .unwrap_or_else(|_| "/var/lib/ritornello/plugin-musicbrainz.json".to_string()),
    );
    let store = Arc::new(RwLock::new(patterns::Store::load(&state_path)));

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
        .metadata(MusicBrainzPlugin::new(store.clone(), state_path.clone()))?
        .admin(admin::MusicBrainzAdmin::new(store, state_path, catalog))?
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

    fn identite_disque(track: u64) -> Value {
        json!({ "kind": "disc", "toc": TOC, "tracks": 3, "track": track })
    }

    fn identite_fichier(path: &str) -> Value {
        json!({ "kind": "file", "path": path })
    }

    /// Un plugin neuf, store clear en mémoire et path d'état jetable.
    ///
    /// Le path est unique par appel (compteur atomique + PID) : plusieurs
    /// tests tournent en parallèle, et un fichier partagé se ferait voler la
    /// vedette par un autre test qui écrit au même instant.
    fn plugin_test() -> MusicBrainzPlugin {
        static COMPTEUR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COMPTEUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ritornello-mb-test-{}-{n}.json", std::process::id()));
        MusicBrainzPlugin::new(Arc::new(RwLock::new(patterns::Store::default())), path)
    }

    /// Plugin dont le disc est déjà known : évite tout appel réseau dans les
    /// tests, **aucun d'entre eux ne touche le réseau**.
    fn plugin_avec_disque_connu() -> MusicBrainzPlugin {
        let mut p = plugin_test();
        p.known = Some((TOC.to_string(), musicbrainz::parse_lookup(FIXTURE, 3)));
        p
    }

    #[test]
    fn une_identite_de_disque_est_reconnue() {
        let d = disc_of(&identite_disque(2)).unwrap();
        assert_eq!(d.toc, TOC);
        assert_eq!(d.track, 2);
    }

    #[test]
    fn une_identite_qui_nest_pas_un_disque_est_ignoree() {
        // Le plugin doit se taire sur un stream radio, sans rien inspecter de plus.
        assert!(disc_of(&json!({"kind": "stream", "url": "http://fip"})).is_none());
        assert!(disc_of(&json!({"kind": "disc"})).is_none(), "sans TOC");
        assert!(disc_of(&json!({"kind": "disc", "toc": "  "})).is_none(), "TOC clear");
        assert!(disc_of(&json!({"kind": "disc", "toc": TOC})).is_none(), "sans index de track");
        assert!(disc_of(&json!("pas un objet")).is_none());
        assert!(disc_of(&Value::Null).is_none());
    }

    #[tokio::test]
    async fn emet_le_titre_de_la_piste_annoncee_avec_echo_de_lidentite() {
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(1)), ..Default::default() }).await;
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, identite_disque(1), "l'identity doit etre reemise en echo");
        assert_eq!(e.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(e.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(e.title.as_deref(), Some("Freddie Freeloader"));
        // Le MBID etait deja porte par le lookup TOC : la cover part sans
        // requete de plus, et ce path ecrase (il sait ce qui plays).
        assert_eq!(
            e.cover,
            Some(CoverRef::Url { url: musicbrainz::url_caa("e32a3f0b-1c19-3170-bb1c-650893774744") })
        );
        assert!(!e.fill_only, "le path disc connait la TOC, il ecrase");
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
        assert!(p.in_flight.is_none(), "aucune nouvelle interrogation pour le meme disc");
    }

    #[tokio::test]
    async fn larret_efface_lenrichissement_prepare() {
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        p.now_playing(NowPlaying { source: "cd".into(), identity: None, ..Default::default() }).await;
        assert!(p.ready.is_none(), "un enrichment perime ne doit pas partir apres l'arret");
        assert!(p.identity.is_none());
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
        assert!(p.ready.is_none());
        assert!(p.in_flight.is_none(), "aucun appel reseau pour une identity de stream");
    }

    #[tokio::test]
    async fn une_piste_hors_bornes_ne_produit_rien() {
        // Disc reconnu à 3 pistes, mais l'identité announcement la track 7 : se
        // taire vaut mieux qu'annoncer le title d'une autre track.
        let mut p = plugin_avec_disque_connu();
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(7)), ..Default::default() }).await;
        assert!(p.ready.is_none());
    }

    #[tokio::test]
    async fn un_disque_inconnu_ne_produit_rien_et_nest_interroge_quune_fois() {
        // Résultat mémorisé comme « interrogé, rien trouvé » : les changements
        // de track suivants ne doivent pas relancer de requête.
        let mut p = plugin_test();
        p.known = Some((TOC.to_string(), None));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        assert!(p.ready.is_none());
        assert!(p.in_flight.is_none());
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(1)), ..Default::default() }).await;
        assert!(p.in_flight.is_none(), "un disc deja interroge ne doit pas l'etre a nouveau");
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
        assert!(p.in_flight.is_none());
        assert!(p.ready.is_none());
    }

    #[test]
    fn le_relai_generique_exige_un_artiste_et_un_album_et_se_tait_si_la_pochette_est_tenue() {
        use ritornello_proto::Known;
        // Jamais sur un title ICY seul : c'est un texte raw, non decoupe, et
        // OUI FM emet « Titre - ARTISTE » dans l'order inverse de l'usage.
        assert!(!should_search(&Known { title: Some("X - Y".into()), ..Default::default() }));
        assert!(!should_search(&Known { artist: Some("A".into()), ..Default::default() }));
        assert!(!should_search(&Known { album: Some("B".into()), ..Default::default() }));
        assert!(should_search(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            ..Default::default()
        }));
        // Une cover deja tenue : l'appel serait jete.
        assert!(!should_search(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            cover: true,
            ..Default::default()
        }));
    }

    #[tokio::test]
    async fn un_resultat_pour_un_autre_disque_ne_produit_rien() {
        // Le disc a été changé pendant que la requête volait : le résultat
        // arrive pour une TOC qui n'est plus celle du tiroir.
        let mut p = plugin_test();
        // Interrogation déclarée « en vol » : `search` ne lancera donc aucune
        // requête réseau, et le résultat est injecté à la main ci-dessous.
        p.in_flight = Some(TOC.to_string());
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        p.found_tx
            .send(("42 1 2 3".to_string(), Answer::Known(musicbrainz::parse_lookup(FIXTURE, 3))))
            .await
            .unwrap();
        // `next_enrichment` consomme le résultat périmé puis se remet en attente :
        // on vérifie qu'il ne rend rien dans un délai borné.
        let r = tokio::time::timeout(std::time::Duration::from_millis(200), p.next_enrichment()).await;
        assert!(r.is_err(), "aucun enrichment ne doit sortir d'un resultat hors sujet");
    }

    // `..Default::default()` derrière un littéral pourtant complet : clippy le
    // dit sans effet (`needless_update`), et il a reason **aujourd'hui**. Ce
    // n'est pas de la redondance mais de la compatibilité ascendante — un
    // littéral qui se terminate ainsi survit à l'ajout d'un champ dans la
    // structure, celui qui les énumère tous casse. Le dépôt a payé cette
    // leçon : un champ ajouté à une structure publique a cassé 44 littéraux
    // ailleurs, qu'un `cargo test -p` ne compile jamais. Quand clippy et la
    // compatibilité ascendante se contredisent ici, c'est la seconde qui
    // gagne, et la règle qui reçoit un `allow`.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn le_relai_generique_emet_une_pochette_seule_en_completion() {
        // La recherche est pré-mémorisée pour n'exercer aucun appel réseau :
        // c'est `search_cover` qui décide de ne pas relancer, exactement
        // comme `plugin_avec_disque_connu` le fait côté disc.
        let mut p = plugin_test();
        let key = ("Miles Davis".to_string(), "Kind of Blue".to_string());
        // Une URL deja resolue, comme ce que `search_release` memorise : c'est
        // le module qui decide du niveau (album ou pressage), jamais ce
        // path-ci. Ici celle d'un groupe, le cas courant d'une recherche.
        let cover = musicbrainz::caa_group_url("8e8a594f-2175-38c7-a871-abb68ec363e7");
        p.known_cover = Some((key, Some(cover.clone())));
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
        assert_eq!(e.identity, identite_fichier("/musique/a.flac"), "l'identity doit etre reemise en echo");
        assert_eq!(e.cover, Some(CoverRef::Url { url: cover }));
        assert!(e.fill_only, "ce path ne sait rien de plus que ce qu'on lui a donne, il complete");
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
        let key = ("A".to_string(), "B".to_string());
        p.known_cover = Some((key, None));
        let known = ritornello_proto::Known { artist: Some("A".into()), album: Some("B".into()), ..Default::default() };
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(identite_fichier("/x")),
            known: known.clone(),
            ..Default::default()
        })
        .await;
        // **Il prepare un aveu, pas une cover** : « search, rien trouve »
        // est une reponse, et c'est elle qui permet a l'ecran de distinguer
        // MusicBrainz interroge sans record_success de MusicBrainz jamais interroge.
        // Elle n'apporte rien d'autre — aucun champ, aucune image — donc elle
        // n'entre dans aucun arbitrage.
        let aveu = p.ready.as_ref().expect("une recherche infructueuse doit se declarer");
        assert!(aveu.searched);
        assert!(aveu.artist.is_none() && aveu.title.is_none() && aveu.album.is_none());
        assert!(aveu.cover.is_none() && aveu.year.is_none() && aveu.links.is_empty());
        assert!(p.cover_in_flight.is_none());
        p.now_playing(NowPlaying { source: "files".into(), identity: Some(identite_fichier("/x")), known, ..Default::default() })
            .await;
        assert!(p.cover_in_flight.is_none(), "un pair deja recherche ne doit pas l'etre a nouveau");
    }

    // Voir `le_relai_generique_emet_une_pochette_seule_en_completion` : le
    // `..Default::default()` est de la compatibilité ascendante, pas de la
    // redondance.
    #[allow(clippy::needless_update)]
    #[tokio::test(start_paused = true)]
    async fn une_panne_passagere_de_musicbrainz_ne_se_memorise_pas() {
        // Le defaut rapporte par le owner, en test. Un 503 de
        // MusicBrainz se figeait en « cet album n'a pas de cover » pour
        // toute la duration de l'album : seul un redemarrage du greffon le
        // debloquait. Ici on force la reponse `Unavailable` et on verifie que
        // rien n'est memorise et que la trame suivante restart bien.
        let mut p = plugin_test();
        let key = ("Rhapsody Of Fire".to_string(), "Triumph Or Agony".to_string());
        let known = ritornello_proto::Known {
            artist: Some("Rhapsody Of Fire".into()),
            album: Some("Triumph Or Agony".into()),
            ..Default::default()
        };
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(identite_fichier("/x")),
            known: known.clone(),
            ..Default::default()
        })
        .await;
        assert_eq!(p.cover_in_flight.as_ref(), Some(&key), "prealable : une recherche est partie");

        // La tache repond « pas de reponse ».
        p.cover_tx.send((key.clone(), Answer::Unavailable)).await.unwrap();
        // Clock virtuelle (`start_paused`) : ce `timeout` n'wait aucune
        // duration reelle. Il laisse la boucle depiler le message — ready des
        // qu'il est en file — puis rend la main faute d'enrichment a
        // produire. Le timeout n'est donc pas une hypothese sur la vitesse
        // d'execution, c'est le temps virtuel qui avance seul quand plus rien
        // n'est ready.
        let rien = tokio::time::timeout(std::time::Duration::from_secs(1), p.next_enrichment()).await;
        assert!(rien.is_err(), "une panne passagere ne doit produire aucun enrichment");

        assert!(
            p.known_cover.is_none(),
            "rien ne doit etre memorise : c'est ce qui figeait l'absence jusqu'au redemarrage"
        );
        // **Une reprise est armee, et c'est elle qui tient le marqueur.** La
        // version d'avant liberait `cover_in_flight` en comptant sur « la
        // prochaine trame » pour reessayer -- or il n'y en a pas sur un fichier
        // local, ou identity et `known` se figent des que les etiquettes sont
        // lues. Le marqueur reste donc arme pendant l'attente : c'est ce qui
        // interdit a une trame survenant entre-temps de lancer une seconde
        // recherche pour le meme pair.
        assert_eq!(p.cover_in_flight.as_ref(), Some(&key), "la reprise tient le marqueur");
        assert_eq!(p.cover_retries, Some((key.clone(), 1)), "une reprise doit etre consommee");

        // Une trame de plus ne restart rien : la reprise deja armee s'en
        // load, et deux recherches concurrentes pour le meme pair se
        // repondraient l'une l'autre.
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(identite_fichier("/x")),
            known,
            ..Default::default()
        })
        .await;
        assert_eq!(p.cover_retries, Some((key, 1)), "aucune reprise de plus ne doit partir");
    }

    #[test]
    fn le_budget_de_reprises_est_borne_et_porte_par_le_couple() {
        // La decision seule, sans horloge ni reseau : c'est pour cela qu'elle
        // est separee de son application (voir `retry_due`).
        let mut p = plugin_test();
        let key = ("A".to_string(), "Disc".to_string());
        p.generic_key = Some(key.clone());

        // Trois reprises, de plus en plus espacees, puis plus rien : au-dela,
        // l'absence n'est plus une panne passagere et le changement de track
        // reste la reprise ultime. La troisieme a ete ajoutee sur mesure — six
        // 503 sur neuf requetes en une minute, constates sur l'appareil.
        assert_eq!(p.retry_due(&key), Some((0, Duration::from_secs(20))));
        p.cover_retries = Some((key.clone(), 1));
        assert_eq!(p.retry_due(&key), Some((1, Duration::from_secs(60))));
        p.cover_retries = Some((key.clone(), 2));
        assert_eq!(p.retry_due(&key), Some((2, Duration::from_secs(180))));
        p.cover_retries = Some((key.clone(), 3));
        assert_eq!(p.retry_due(&key), None, "le budget doit etre bounded");

        // Le compteur est porte par le pair : un autre album repart de zero
        // sans qu'aucune remise a zero explicite n'ait a exister.
        let autre = ("A".to_string(), "Autre".to_string());
        p.generic_key = Some(autre.clone());
        assert_eq!(p.retry_due(&autre), Some((0, Duration::from_secs(20))));

        // Et rien n'est repris pour un pair qu'on ne vise plus : ce serait du
        // travail pur perdu, sa reponse etant de toute facon ecartee.
        assert_eq!(p.retry_due(&key), None, "un pair abandonne ne se reprend pas");
    }

    // Voir `le_relai_generique_emet_une_pochette_seule_en_completion` : le
    // `..Default::default()` est de la compatibilité ascendante, pas de la
    // redondance.
    #[allow(clippy::needless_update)]
    #[tokio::test]
    async fn un_changement_dalbum_ne_reutilise_pas_lancienne_pochette() {
        // La mémorisation est clée par (artist, album) : un nouvel album doit
        // changer la clé et ne jamais réafficher la cover de l'ancien.
        let mut p = plugin_test();
        p.known_cover =
            Some((("A".to_string(), "Vieux".to_string()), Some("11111111-1111-1111-1111-111111111111".into())));
        // Recherche du nouvel album déclarée « en vol » : évite tout appel
        // réseau dans ce test, sans changer ce qui est observé (`in_flight`
        // arrête `search_cover` avant le `tokio::spawn`).
        p.cover_in_flight = Some(("A".to_string(), "Nouveau".to_string()));
        p.now_playing(NowPlaying {
            source: "files".into(),
            identity: Some(identite_fichier("/x")),
            known: ritornello_proto::Known { artist: Some("A".into()), album: Some("Nouveau".into()), ..Default::default() },
            ..Default::default()
        })
        .await;
        assert!(p.ready.is_none(), "la cover de l'ancien album ne doit pas s'appliquer au nouveau");
        assert_eq!(p.generic_key, Some(("A".to_string(), "Nouveau".to_string())), "la key suit le nouvel album");
    }

    #[tokio::test]
    async fn une_identite_de_disque_efface_letat_generique() {
        // Les deux chemins sont exclusifs : un disc inséré ne doit rien
        // laisser du relai générique en place.
        let mut p = plugin_test();
        p.in_flight = Some(TOC.to_string()); // évite tout appel réseau dans ce test
        p.generic_identity = Some(identite_fichier("/x"));
        p.generic_key = Some(("A".to_string(), "B".to_string()));
        p.now_playing(NowPlaying { source: "cd".into(), identity: Some(identite_disque(0)), ..Default::default() }).await;
        assert!(p.generic_identity.is_none());
        assert!(p.generic_key.is_none());
    }

    // --- Chemin ICY (radio) ---------------------------------------------

    fn candidat(artist: &str, title: &str, artist_first: bool) -> icy::Candidate {
        icy::Candidate {
            artist: artist.to_string(),
            title: title.to_string(),
            separator: " - ",
            artist_first,
            title_in_middle: false,
        }
    }

    fn enregistrement(score: u64, title: &str) -> musicbrainz::Recording {
        musicbrainz::Recording { score, title: title.to_string(), cover_url: None }
    }

    #[test]
    fn le_meilleur_score_gagne_et_non_le_premier_accepte() {
        // Le winner est **second** dans l'order d'essai : sans cela, le test
        // passerait aussi avec « prendre le premier accepté ».
        let essais = vec![
            // L'order inversé validated quand même (score au-dessus du seuil,
            // mais plus faible) : c'est le cas réel qui rend « prendre le
            // premier accepté » dangereux.
            (candidat("So What", "Miles Davis", false), Some(enregistrement(91, "Miles Davis"))),
            (candidat("Miles Davis", "So What", true), Some(enregistrement(99, "So What"))),
        ];
        let winner = best_accepted(&essais).expect("un candidat doit etre retenu");
        assert_eq!((winner.artist.as_str(), winner.title.as_str()), ("Miles Davis", "So What"));
        assert!(winner.artist_first);
    }

    #[test]
    fn un_titre_qui_ne_correspond_pas_est_ecarte_malgre_un_bon_score() {
        // La garde qui porte tout : le score seul est trop généreux, la
        // recherche rendant presque toujours quelque chose de plausible.
        let essais =
            vec![(candidat("So What", "Miles Davis", false), Some(enregistrement(95, "Un Tout Autre Recording")))];
        assert!(best_accepted(&essais).is_none(), "score haut mais title discordant : doit etre ecarte");
    }

    #[test]
    fn aucun_candidat_accepte_donne_ne_pas_decouper() {
        // Aucun essai (chaîne sans séparateur, cf. `icy::candidates`) ou aucun
        // accepté : le sondage n'a rien retenu, ce que `handle_icy` translate en
        // `Pattern::DoNotSplit` (non rejoué ici, le réseau n'étant pas
        // joignable en test — `best_accepted` porte la décision).
        assert!(best_accepted(&[]).is_none(), "aucun essai, donc aucun accepte");
        let essais = vec![
            (candidat("A", "B", true), None), // hors line / rien trouve
            (candidat("B", "A", false), Some(enregistrement(50, "A"))), // sous le seuil
        ];
        assert!(best_accepted(&essais).is_none());
    }

    #[tokio::test]
    async fn une_station_classee_ne_pas_decouper_ne_declenche_aucune_requete() {
        // `handle_icy` avec `known = DoNotSplit` et `reprobe = false` doit
        // rendre son issue **sans** toucher au réseau. Prouvé par le fait que
        // le test passe alors qu'aucun réseau n'est joignable ici : une
        // requête tentée échouerait ou traînerait, et le délai ci-dessous la
        // ferait échouer.
        let r = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            handle_icy(
                "http://f".to_string(),
                "Miles Davis - So What".to_string(),
                json!({"kind": "stream", "url": "http://f"}),
                Some(patterns::Pattern::DoNotSplit),
                false,
            ),
        )
        .await;
        let issue = r.expect("aucune requete reseau ne doit etre tentee, donc pas de timeout");
        assert_eq!(issue.pattern, None);
        assert_eq!(issue.validated, None);
    }

    /// Envoie une issue d'échec (validation ratée) pour `url`/`raw`, et
    /// consomme le tour de boucle qui en résulte.
    ///
    /// **Un échec produit désormais un enrichment**, et c'est une
    /// correction de relecture : ne rien émettre laissait celui du track
    /// *précédent* gagner l'arbitrage, l'identité d'une radio ne changeant pas
    /// d'un track à l'autre. L'assertion d'avant — « aucun enrichment » —
    /// épinglait donc le défaut au lieu de la propriété.
    ///
    /// Avec `pair: None`, ce qui part est la chaîne nettoyée en guise de
    /// title, sans artist : on n'affirme aucun découpage, on montre ce que le
    /// stream announcement. Et l'attente est **exacte** (on wait ce qui doit venir)
    /// au lieu de reposer sur une marge de temps.
    async fn envoie_echec(p: &mut MusicBrainzPlugin, url: &str, raw: &str) {
        p.icy_tx
            .send(IcyOutcome {
                url: url.to_string(),
                raw: raw.to_string(),
                identity: json!({"kind": "stream", "url": url}),
                pattern: None,
                validated: None,
                pair: None,
            })
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.artist, None, "un echec n'affirme aucun artist");
        assert_eq!(
            e.title.as_deref(),
            Some(icy::clean(raw).as_str()),
            "il montre ce que le stream announcement, nettoye"
        );
        assert!(e.cover.is_none(), "et aucune cover, faute de release a citer");
    }

    #[tokio::test]
    async fn un_echec_isole_ne_resonde_pas_et_trois_daffilee_resondent() {
        // Les deux moitiés. Sans la première, « resonder toujours » passerait ;
        // sans la seconde, « ne resonder jamais » passerait.
        //
        // Le compteur et la décision sont exercés par le vrai path de code
        // (l'issue traverse `icy_tx`/`next_enrichment`, comme
        // `un_resultat_pour_un_autre_disque_ne_produit_rien` le fait déjà côté
        // disc) : ce n'est pas une resimulation en dur de l'arithmétique.
        let mut p = plugin_test();
        let url = "http://f";
        p.icy_seen = Some("raw".to_string());

        for n in 1..=2u32 {
            envoie_echec(&mut p, url, "raw").await;
            assert_eq!(p.failures.get(url), Some(&n));
            assert!(!should_reprobe(&p.failures, url), "echec numero {n} : ne doit pas encore resonder");
        }

        envoie_echec(&mut p, url, "raw").await;
        assert_eq!(p.failures.get(url), Some(&3));
        assert!(should_reprobe(&p.failures, url), "trois failures d'affilee doivent resonder");
    }

    /// Le resondage **consomme** le compteur d'failures.
    ///
    /// Sans ca il restait au-dessus du seuil pour la vie du processus, et une
    /// station qui ne validated jamais — un stream en mojibake, par exemple —
    /// repartait en sondage complet a *chaque* title. La documentation promet
    /// l'inverse, et la limite qu'elle decrit devenait une tempete de requetes
    /// garantie. Constat de la relecture croisee finale.
    ///
    /// Eprouve sur `now_playing` et non sur `should_reprobe` seul : la remise a
    /// zero vit au site de lancement, et c'est le lien entre les deux que ce
    /// test doit tenir. La tache detachee qui suit ne peut pas joindre le
    /// reseau, ce qui ne gene pas — la remise a zero est synchrone et precede
    /// le `spawn`.
    #[tokio::test]
    async fn un_resondage_consomme_le_compteur() {
        let mut p = plugin_test();
        let url = "http://exemple/stream.mp3";
        let identity = json!({"kind": "stream", "url": url});
        p.failures.insert(url.to_string(), FAILURES_BEFORE_REPROBE);
        assert!(should_reprobe(&p.failures, url), "trois failures arment bien le resondage");

        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(identity),
            known: ritornello_proto::Known {
                stream_title: Some("Miles Davis - So What".into()),
                ..Default::default()
            },
        })
        .await;

        assert_eq!(
            p.failures.get(url),
            None,
            "le lancement du resondage doit avoir consomme le compteur"
        );
        assert!(
            !should_reprobe(&p.failures, url),
            "et le title suivant ne doit pas resonder a son tour"
        );
    }

    #[tokio::test]
    async fn un_succes_remet_le_compteur_a_zero() {
        // Deux échecs, un succès, deux échecs : pas de resondage. C'est la
        // seule assertion qui distingue un compteur consécutif d'un
        // cumulatif — et le cumulatif est le défaut naturel.
        let mut p = plugin_test();
        let url = "http://f";
        p.icy_seen = Some("raw".to_string());

        envoie_echec(&mut p, url, "raw").await;
        envoie_echec(&mut p, url, "raw").await;
        assert_eq!(p.failures.get(url), Some(&2));

        p.icy_tx
            .send(IcyOutcome {
                url: url.to_string(),
                raw: "raw".to_string(),
                // L'identité que le cœur aurait envoyée : c'est elle qui doit
                // repartir en écho, à l'identique.
                identity: json!({"kind": "stream", "url": url}),
                pattern: None,
                validated: Some(("Artiste".to_string(), "Titre".to_string(), None)),
                pair: Some(("Artiste".to_string(), "Titre".to_string())),
            })
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.artist.as_deref(), Some("Artiste"));
        assert!(!p.failures.contains_key(url), "le record_success doit remettre le compteur a zero");

        envoie_echec(&mut p, url, "raw").await;
        envoie_echec(&mut p, url, "raw").await;
        assert!(!should_reprobe(&p.failures, url), "compteur consecutif (2), pas cumulatif (4) : ne doit pas resonder");
    }

    #[test]
    fn une_identite_qui_nest_pas_un_flux_nest_pas_traitee() {
        assert!(stream_url(&json!({"kind":"disc","toc":"1 2 3"})).is_none());
        assert!(stream_url(&json!({"kind":"stream"})).is_none());
        assert!(stream_url(&json!({"kind":"stream","url":""})).is_none());
        assert_eq!(stream_url(&json!({"kind":"stream","url":"http://f"})).as_deref(), Some("http://f"));
    }
}
