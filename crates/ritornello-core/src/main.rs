mod audio_output;
mod admin;
mod core;
mod cover;
mod i18n;
mod metadata;
mod placeholder;
mod player;
mod plugins;
mod register;
mod sante;
mod state;
mod status;
mod system;
mod theme;
mod types;
mod web;

use crate::core::MetadataCablage;
use crate::metadata::PlayerState;
use crate::plugins::PluginManifest;
use crate::status::{AppState, LogBuffer, LogBufferWriter, PluginStatus, StatusState};
use crate::types::Event;
use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
// `PluginKind` vient du protocole partagé, pas du cœur : c'est le binaire du
// greffon qui l'annonce, et `plugins.rs` n'a plus à le connaître.
use ritornello_proto::{
    Announcement, Catalogue, Enrichment, InputMessage, Known, NowPlaying, PluginKind,
};
use ritornello_plugin_sdk::{run_input_client, run_metadata_client, DisplayClient, SourceClient, SourceUpdate};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, RwLock};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[async_trait::async_trait]
impl core::Source for SourceClient {
    async fn request(&self, req: ritornello_proto::SourceReq) -> Result<ritornello_proto::SourceAction> {
        SourceClient::request(self, req).await
    }
}

/// Relais de l'état vers **un** afficheur, dans sa propre tâche.
///
/// Une tâche par afficheur, et non une tâche qui boucle sur N clients : c'est
/// ce qui empêche un afficheur lent — console occupée, écran bloqué en I/O — de
/// retarder les autres. La contre-pression reste cloisonnée par socket, ce qui
/// était l'argument retenu pour ne pas fusionner les sockets des genres.
///
/// Une fonction et non deux copies : le démarrage et le câblage à chaud
/// servent un afficheur de la même façon, et un afficheur arrivé en retard ne
/// doit pas être servi par un relais légèrement différent.
///
/// L'état courant est envoyé **d'abord**, avant toute attente : un afficheur
/// câblé à chaud doit montrer ce qui joue sans attendre le prochain changement
/// d'état. Ne compter que sur `changed()` marchait par accident — l'`etat_rx` de
/// `main` n'est jamais avancé, donc le clone héritait d'une version périmée et
/// rendait la main aussitôt. Un `borrow_and_update()` ajouté un jour dans `main`
/// aurait laissé un afficheur tardif **noir** jusqu'au prochain changement, donc
/// indéfiniment en veille où aucun tick n'est armé, et `publie_etat` n'aurait
/// rien réparé puisqu'il est dédupliqué.
///
/// Un échec d'envoi **sort de la boucle**. Sur un socket dont le pair est mort
/// l'erreur est permanente (EPIPE) : sans sortie, la tâche survivait au greffon
/// et journalisait à chaque trame — une par seconde en lecture, par afficheur
/// zombie. Deux relances à la main suffisaient à écraser en moins de quatre
/// minutes le tampon de 500 lignes qui alimente la popin d'erreurs de l'IHM, et
/// à y noyer le vrai diagnostic. Un client d'afficheur dont l'écriture échoue
/// est inutilisable : on le nomme une fois, et on s'en va.
///
/// **Deux** récepteurs, et deux genres de trame : l'état du lecteur, qui change
/// jusqu'à une fois par seconde, et le catalogue des sources, structurel et
/// rare. Deux canaux séparés plutôt qu'une charge utile élargie : élargir
/// republierait l'état à chaque changement de catalogue et l'inverse, ce que la
/// déduplication par égalité ne rattraperait pas — les deux valeurs changeraient
/// ensemble par construction.
///
/// Les deux valeurs courantes partent d'emblée, avant toute attente, pour la
/// même raison que ci-dessus : un afficheur câblé à chaud doit connaître le
/// catalogue sans attendre qu'il change, et le catalogue ne change presque
/// jamais.
/// **Les pochettes ne partent qu'aux afficheurs qui les ont demandées** dans
/// leur annonce (`Announcement::covers`), et **seulement quand la pochette
/// change** : ni sur chaque trame d'état — il y en a une par seconde en lecture
/// —, ni vers l'afficheur de vingt colonnes, qui recevrait des mégaoctets pour
/// les jeter. Le changement est détecté sur le `cover_href` de l'état, qui est
/// justement l'identité de l'image (la clé du cache) et non un horodatage : une
/// même pochette qui reste à l'écran ne repart donc jamais.
///
/// La matérialisation des octets — le seul moment où l'image entière existe en
/// mémoire dans le cœur — est **derrière** ce filtre : un afficheur qui n'en
/// veut pas ne fait pas payer la lecture du fichier non plus.
/// Par où un relais fait savoir que son pair ne répond plus, et sous quel
/// numéro.
///
/// Les deux voyagent **toujours** ensemble — un avis sans son numéro de câblage
/// ne serait pas interprétable par la boucle —, et les réunir garde la
/// signature du relais lisible.
#[derive(Clone)]
struct AvisInjoignable {
    /// Numéro du câblage qui a lancé ce relais. Voir `cablages` dans la boucle
    /// principale : c'est ce qui distingue la fermeture d'un socket courant de
    /// celle d'une incarnation déjà remplacée.
    cablage: u64,
    tx: mpsc::Sender<(String, u64)>,
}

fn relais_afficheur(
    nom: String,
    client: Arc<DisplayClient>,
    veut_pochettes: bool,
    covers: Arc<cover::CoverCache>,
    mut etat_rx: watch::Receiver<PlayerState>,
    mut catalogue_rx: watch::Receiver<Catalogue>,
    avis: AvisInjoignable,
) {
    tokio::spawn(async move {
        /// Nombre de tentatives accordées à un même `cover_href` avant de
        /// l'abandonner pour de bon.
        ///
        /// **Le compromis exact entre deux défauts symétriques.** Marquer la
        /// tentative comme faite avant de l'avoir faite sacrifiait la pochette
        /// pour **toute la piste** sur un seul délai dépassé : un partage SMB
        /// endormi met une poignée de secondes à répondre au premier accès et
        /// répond ensuite, si bien que la seule tentative jamais accordée était
        /// justement celle qui ne pouvait pas réussir. À l'inverse, retenter sans
        /// borne relirait le fichier **une fois par seconde** — la cadence des
        /// trames d'état en lecture — pour une image dont l'absence peut être
        /// définitive (un 404 du Cover Art Archive, un fichier au-delà du
        /// plafond). Trois essais couvrent le réveil d'un partage sans installer
        /// de boucle de relecture.
        const ESSAIS_POCHETTE: u8 = 3;

        /// Ce que le relais retient de ses tentatives de pochette.
        ///
        /// Deux champs et non un, parce que « poussée » et « tentée sans
        /// succès » sont deux faits différents : c'est leur confusion qui faisait
        /// perdre la pochette de toute une piste sur un unique échec.
        #[derive(Default)]
        struct SuiviPochette {
            /// Le `cover_href` de la dernière pochette **réellement écrite** sur
            /// le socket. Une trame d'état qui répète ce href ne redéclenche
            /// rien : c'est cette garde qui évite de pousser des mégaoctets à
            /// chaque seconde de lecture.
            poussee: Option<String>,
            /// Le `cover_href` en échec et le nombre d'essais déjà consommés.
            /// Remis à zéro dès qu'un autre href apparaît : le budget est par
            /// pochette, pas par relais.
            echecs: Option<(String, u8)>,
        }

        /// Pousse la pochette que `href` désigne, si elle a changé depuis le
        /// dernier envoi **réussi**. Rend `Err` comme un envoi d'état, pour que
        /// le contrôle d'erreur de la boucle soit le même : un socket mort doit
        /// faire sortir, quel que soit le genre de trame qui l'a découvert.
        ///
        /// Une pochette introuvable, illisible ou trop grosse n'est **pas** une
        /// erreur d'envoi : rien ne part, la boucle continue, et l'échec est
        /// compté à part du succès (voir `SuiviPochette` et `ESSAIS_POCHETTE`).
        /// Un échec transitoire est donc réessayé à la trame d'état suivante,
        /// jusqu'à épuisement du budget — un échec définitif ne coûte que trois
        /// lectures par piste, pas une par seconde.
        ///
        /// **N'encode rien elle-même** : `covers.ligne` construit la trame et la
        /// rend derrière un `Arc` ; ce relais ne fait qu'écrire ce buffer
        /// (`DisplayClient::send_cover_line`), sans recopie ni réencodage.
        async fn pousse(
            client: &DisplayClient,
            covers: &cover::CoverCache,
            suivi: &mut SuiviPochette,
            href: Option<&str>,
        ) -> anyhow::Result<()> {
            // `None` (plus rien ne joue, ou pochette retirée) n'émet aucune
            // trame : l'afficheur l'apprend par le `cover_href` absent de
            // l'état, qu'il vient de recevoir. Inventer une trame de pochette
            // vide ferait exister une image de zéro octet dans le protocole.
            // Les deux mémoires sont vidées : la prochaine pochette, même
            // identique à la précédente, décrit un nouveau morceau.
            let Some(href) = href else {
                suivi.poussee = None;
                suivi.echecs = None;
                return Ok(());
            };
            if suivi.poussee.as_deref() == Some(href) {
                return Ok(());
            }
            // Budget consommé pour *ce* href : ne plus rien tenter. Un href
            // différent efface l'ardoise, ce que fait le `match` ci-dessous.
            let essais = match &suivi.echecs {
                Some((h, n)) if h == href => {
                    if *n >= ESSAIS_POCHETTE {
                        return Ok(());
                    }
                    *n
                }
                _ => 0,
            };
            let Some(cle) = href.strip_prefix(cover::PREFIXE_HREF) else {
                // Un href sans notre préfixe ne deviendra jamais valide :
                // consommer tout le budget d'un coup plutôt que de réessayer
                // trois fois une chaîne qui ne peut pas changer.
                tracing::debug!("cover href {href} has no key, nothing pushed");
                suivi.echecs = Some((href.to_owned(), ESSAIS_POCHETTE));
                return Ok(());
            };
            let Some(ligne) = covers.ligne(cle, href).await else {
                // Déjà journalisé par `octets` avec sa raison. Compté comme un
                // échec, donc réessayé à la trame suivante : c'est ici que se
                // joue le partage endormi.
                suivi.echecs = Some((href.to_owned(), essais + 1));
                return Ok(());
            };
            client.send_cover_line(&ligne).await?;
            suivi.poussee = Some(href.to_owned());
            suivi.echecs = None;
            Ok(())
        }

        // **Deux sorties de boucle qu'il ne faut surtout pas confondre.** Un
        // envoi en échec veut dire que l'afficheur n'est plus joignable, et
        // c'est ce qui doit devenir visible sur la page de statut. Un
        // `watch::Receiver` fermé veut dire que *le cœur* s'arrête — ses
        // émetteurs sont tombés —, ce qui ne dit rien du greffon et ne doit donc
        // rien signaler : marquer déconnectés tous les afficheurs pendant
        // l'extinction du cœur peindrait une panne sur un arrêt normal.
        //
        // D'où le bloc étiqueté : les quatre chemins « pair injoignable »
        // rendent `true`, la fin naturelle de la boucle rend `false`, et l'avis
        // part d'un seul endroit — sous le bloc — au lieu d'être recopié quatre
        // fois.
        let injoignable_constate = 'vie: {
            let etat = etat_rx.borrow_and_update().clone();
            let cat = catalogue_rx.borrow_and_update().clone();
            if let Err(e) = client.send(&etat).await {
                tracing::warn!("display plugin {nom} relay stopped: {e}");
                break 'vie true;
            }
            if let Err(e) = client.send_catalogue(&cat).await {
                tracing::warn!("display plugin {nom} relay stopped: {e}");
                break 'vie true;
            }
            // La pochette courante part d'emblée, comme l'état et le catalogue
            // et pour la même raison : un afficheur câblé à chaud doit montrer
            // ce qui joue sans attendre le prochain changement de piste.
            let mut suivi_pochette = SuiviPochette::default();
            if veut_pochettes {
                if let Err(e) = pousse(
                    &client,
                    &covers,
                    &mut suivi_pochette,
                    etat.morceau.cover_href.as_deref(),
                )
                .await
                {
                    tracing::warn!("display plugin {nom} relay stopped: {e}");
                    break 'vie true;
                }
            }
            loop {
                let envoi = tokio::select! {
                    r = etat_rx.changed() => match r {
                        Ok(()) => {
                            let e = etat_rx.borrow_and_update().clone();
                            let envoi = client.send(&e).await;
                            // L'état d'abord, la pochette ensuite : l'afficheur
                            // connaît ainsi le `cover_href` avant de recevoir
                            // les octets qui s'en réclament.
                            match (envoi, veut_pochettes) {
                                (Ok(()), true) => {
                                    pousse(
                                        &client,
                                        &covers,
                                        &mut suivi_pochette,
                                        e.morceau.cover_href.as_deref(),
                                    )
                                    .await
                                }
                                (autre, _) => autre,
                            }
                        }
                        // Le cœur s'arrête, pas le greffon : sortir sans rien
                        // signaler.
                        Err(_) => break,
                    },
                    r = catalogue_rx.changed() => match r {
                        Ok(()) => {
                            let c = catalogue_rx.borrow_and_update().clone();
                            client.send_catalogue(&c).await
                        }
                        Err(_) => break,
                    },
                };
                if let Err(e) = envoi {
                    tracing::warn!("display plugin {nom} relay stopped: {e}");
                    break 'vie true;
                }
            }
            false
        };
        if injoignable_constate {
            // `let _` : la boucle du cœur a pu disparaître entre-temps, et son
            // départ n'est pas un incident à journaliser ici.
            let _ = avis.tx.send((nom, avis.cablage)).await;
        }
    });
}

/// Ce qu'une future de supervision rend : nom, génération, statut de sortie,
/// et si la mort avait été demandée.
///
/// Boxée, donc **nommée** : le démarrage et le rallumage poussent tous deux
/// dans le même `FuturesUnordered`, et deux fonctions rendant chacune un
/// `impl Future` rendent deux types opaques distincts, qu'aucune collection
/// n'accepte ensemble. Une allocation par lancement de greffon, huit au
/// démarrage.
type SortieGreffon =
    futures::future::BoxFuture<'static, (String, u64, std::io::Result<std::process::ExitStatus>, bool)>;

/// Surveille un greffon jusqu'à sa mort, qu'elle soit subie ou demandée.
///
/// Une fonction, et non un `async move` recopié aux deux endroits qui lancent
/// un greffon (démarrage et rallumage) : c'est le seul endroit qui sait que
/// `kill_rx` veut dire « termine-le ».
///
/// Le `select!` ne fait que **choisir** — aucun de ses bras ne touche à
/// `child` — pour que l'emprunt mutable des futures soit rendu avant le
/// `termine` qui suit. Rappeler `wait()` après coup est sans risque : tokio
/// mémorise le statut du processus déjà moissonné.
///
/// Rend `(nom, génération, statut, voulue)`. La génération est ce qui permet à
/// la boucle principale d'ignorer la mort d'une incarnation précédente,
/// arrivée après le rallumage de la suivante.
fn supervise(
    nom: String,
    generation: u64,
    child: tokio::process::Child,
    kill_rx: tokio::sync::oneshot::Receiver<()>,
) -> SortieGreffon {
    use futures::FutureExt;
    async move {
        let mut child = child;
        // `r.is_ok()` et non `_` : seul un envoi réel veut dire « demandée ».
        // Un `kill_rx` dont l'émetteur a été abandonné rend aussi `Err`, ce
        // qui arrive quand deux entrées de `plugins.toml` partagent le même
        // `name` — toléré à dessein par le chargeur de manifeste — et que le
        // second `kill_triggers.insert` écrase le `kill_tx` du premier :
        // sans ce test, la mort naturelle du premier serait prise pour une
        // extinction demandée, `termine` enverrait `SIGTERM` à un processus
        // sain, et `mark_plugin_disconnected` ne serait jamais appelé.
        let voulue = tokio::select! {
            r = kill_rx => r.is_ok(),
            _ = child.wait() => false,
        };
        let statut = if voulue {
            plugins::termine(&mut child, plugins::GRACE_ARRET).await
        } else {
            child.wait().await
        };
        (nom, generation, statut, voulue)
    }
    .boxed()
}

/// Les fils que le câblage à chaud doit tenir pour rejouer, après le
/// démarrage, ce que la boucle de câblage initiale fait avec ses variables
/// locales.
/// Combien de temps un greffon qu'on vient de lancer garde le bénéfice du
/// doute avant d'être rapporté « figé ».
///
/// Strictement plus long que `register::DELAI_LECTURE` (5 s), et ce n'est pas
/// une marge de confort : une connexion déjà acceptée qui est **en train**
/// d'écrire sa ligne d'annonce dispose de ces cinq secondes, et la rapporter
/// figée pendant ce temps serait se contredire soi-même. Dix secondes couvrent
/// donc le chargement du binaire depuis une carte SD, la liaison de ses sockets
/// et l'écriture de son annonce, avec de la marge.
///
/// Au-delà, « figé » redevient le mot juste : le greffon est lancé, vivant, et
/// muet — un diagnostic, pas une attente.
const DELAI_DEMARRAGE: std::time::Duration = std::time::Duration::from_secs(10);

/// L'échéance de démarrage est passée : faut-il rétrograder ce greffon en
/// « figé » ?
///
/// **Seulement si sa ligne dit encore « démarrage ».** Entre le lancement et
/// l'échéance, le greffon a pu s'annoncer (sa ligne décrit alors ses genres),
/// mourir (elle dit « déconnecté »), ou être éteint depuis l'IHM (elle dit
/// « désactivé »). Dans les trois cas, écraser remplacerait une information
/// vraie par une fausse — et la fausse serait la plus trompeuse des quatre,
/// puisqu'elle accuse un greffon qui va bien.
///
/// Relire l'état plutôt que tenir un registre à purger à chaque transition :
/// c'est la leçon de `kill_triggers`, dont trois sites de purge étaient déjà un
/// de trop.
fn a_retrograder(statuts: &StatusState, nom: &str) -> bool {
    statuts.plugins.iter().any(|l| l.name == nom && l.starting)
}

struct FilsChaud {
    sockets_dir: PathBuf,
    /// Noms du manifeste dans l'ordre du fichier : autorité sur les noms
    /// acceptés, et priorité d'arbitrage des `metadata`.
    ordre_manifeste: Vec<String>,
    source_update_tx: mpsc::Sender<(String, SourceUpdate)>,
    cmd_tx: mpsc::Sender<InputMessage>,
    enrich_tx: mpsc::Sender<(String, Enrichment)>,
    now_playing_rx: watch::Receiver<NowPlaying>,
    etat_rx: watch::Receiver<PlayerState>,
    /// Le second récepteur de `relais_afficheur` : un afficheur câblé à chaud
    /// doit être servi par un relais identique à celui du démarrage.
    catalogue_rx: watch::Receiver<Catalogue>,
    /// **Le même** `Arc` que celui du cœur et de l'`AppState` HTTP (voir
    /// `assemble_covers_et_core`) : un afficheur câblé à chaud doit lire les
    /// pochettes que le cœur a déjà récupérées, pas un cache neuf et vide.
    covers: Arc<cover::CoverCache>,
    status_state: Arc<RwLock<StatusState>>,
    admin_backends: admin::AdminBackends,
    /// **Le même** `Arc` que celui de l'`AppState` HTTP, pour la même raison que
    /// `covers` : purger un cache neuf et vide n'invaliderait rien de ce que les
    /// routes servent réellement.
    admin_assets: Arc<admin::AssetCache>,
    /// Par où un socket qui se ferme le fait savoir à la boucle principale.
    ///
    /// **C'est le seul chemin par lequel la mort d'un greffon non supervisé
    /// devient visible.** Un greffon relancé à la main échappe à
    /// `plugin_waits` — le cœur n'est pas son parent, il ne verra jamais son
    /// code de sortie —, mais ses sockets, eux, sont bien les nôtres : leur
    /// fermeture est un fait que le cœur observe déjà, et qu'il se contentait de
    /// journaliser. La page continuait donc de l'afficher connecté,
    /// indéfiniment.
    ///
    /// Porte `(nom, génération de câblage)` : voir `cablages` dans la boucle,
    /// qui dit pourquoi le numéro est indispensable.
    injoignable_tx: mpsc::Sender<(String, u64)>,
}

/// Câble un greffon qui s'annonce **après** le rendez-vous de démarrage.
///
/// Chaque genre reprend la forme du câblage initial. Deux différences, imposées
/// par le fait que le cœur tourne déjà : la source passe par
/// `Core::add_source`, et l'ordre d'arbitrage des `metadata` est **recalculé en
/// entier** depuis le manifeste au lieu d'être complété.
///
/// Une ré-annonce d'un greffon déjà câblé suit le même chemin : on recâble.
/// `add_source` remplace le client, et les relais précédents sortent d'eux-mêmes
/// à leur premier échec d'envoi, leur socket ayant disparu — c'est ce que
/// garantit la sortie de boucle de `relais_afficheur`, sans laquelle ils
/// s'accumuleraient à chaque relance en journalisant à chaque trame.
async fn cabler_a_chaud<P: player::Player>(
    annonce: Announcement,
    fils: &FilsChaud,
    core: &mut core::Core<P>,
    rassemble: &mut register::Gathered,
    kill_triggers: &HashMap<String, tokio::sync::oneshot::Sender<()>>,
    non_supervises: &mut HashSet<String>,
    // Numéro de ce câblage-ci, attribué par la boucle. Recopié dans chaque
    // tâche de socket lancée ici, pour que la fermeture d'un socket d'une
    // incarnation précédente soit reconnue comme telle et ignorée. Un `///` est
    // refusé sur un paramètre, d'où le commentaire ordinaire.
    cablage: u64,
) {
    let nom = annonce.name.clone();
    // Le nom fait autorité côté manifeste, à chaud comme au rendez-vous : une
    // annonce qui en porte un autre est nommée puis écartée, jamais câblée.
    if !fils.ordre_manifeste.contains(&nom) {
        tracing::warn!("late announcement from unknown plugin {nom}, ignored");
        return;
    }
    tracing::info!(
        "{nom} announced late {:?} (admin: {}), wiring it now",
        annonce.kinds,
        annonce.admin
    );
    // Le cœur ne tient pas le `child` de ce greffon : `plugin_waits` ne reverra
    // ni son prochain code de sortie ni son `mark_plugin_disconnected`. Le
    // `connected: true` qu'on va poser sera vrai à l'instant où on le pose, et
    // ne se démentira plus jamais tout seul.
    //
    // La condition était `rassemble.morts.contains(&nom)`, deux fois trop
    // étroite : `morts` n'est rempli que par le rendez-vous de démarrage, donc
    // elle manquait les morts observées par la boucle principale **et** les
    // processus que le cœur n'a jamais lancés. `kill_triggers` répond exactement
    // à la question posée, puisqu'il ne signifie que « lancé par nous et pas
    // encore moissonné » — et une annonce prouve la vie de son émetteur.
    //
    // Le nom est **retenu**, et c'est là tout l'apport : les `retain` juste
    // au-dessus effaçaient `morts` dans la foulée du `warn`, donc la seule trace
    // disparaissait à l'instant du recâblage. Un défaut dont le programme avait
    // conscience et dont il détruisait la preuve.
    if vivacite(&nom, kill_triggers, non_supervises) != Vivacite::Supervise {
        // Cet avertissement disait « sa prochaine sortie passera inaperçue, et
        // il ne pourra plus être allumé ni éteint depuis l'IHM avant un
        // redémarrage du cœur ». **Les deux moitiés sont devenues fausses** :
        // la fermeture de ses sockets est désormais observée, ce qui rend sa
        // mort visible sur la page *et* le fait sortir de `non_supervises`,
        // donc redevenir gérable. Ce qui reste vrai — et ce que ce
        // `warn!` dit maintenant — est plus étroit : tant qu'il vit, le cœur ne
        // peut pas l'arrêter, faute de tenir son `child`.
        tracing::warn!(
            "wiring {nom}, which is alive but not supervised by the core: it cannot be stopped from the admin UI while it lives, though the core will notice when its sockets close"
        );
        non_supervises.insert(nom.clone());
    }

    // Le rassemblement et l'ordre d'arbitrage sont mis à jour **avant** de
    // lancer quoi que ce soit. L'ordre d'abord parce que le client `metadata`
    // lancé plus bas peut envoyer un enrichissement dès sa première trame, et
    // le cœur rejette un enrichissement « from an undeclared metadata plugin » :
    // aujourd'hui la boucle principale ne peut pas drainer `enrich_rx` pendant
    // ce bras, mais compter là-dessus, c'est faire dépendre la correction d'une
    // sérialisation implicite qu'un refactor — ce câblage sorti dans une tâche —
    // ferait tomber sans bruit.
    //
    // La liste est recalculée en **entier** depuis le manifeste, jamais
    // complétée en queue : la priorité est celle de `plugins.toml`, et un
    // greffon `metadata` tardif y prend sa place du fichier. La logique d'ordre
    // reste dans `register::metadata_order`, un seul endroit.
    //
    // Les deux `retain` gardent `Gathered` cohérent : un figé qui vient de
    // parler n'est plus figé, un mort qui revient n'est plus mort. Rien ne lit
    // ces deux listes après le démarrage — la page de statut vient de
    // `status_state` — mais la structure est la mémoire de ce que le cœur sait
    // des greffons, et un nom n'y appartient qu'à une seule des trois
    // collections. Deux lignes pour qu'elle ne mente pas au prochain lecteur.
    rassemble.figes.retain(|n| n != &nom);
    rassemble.morts.retain(|n| n != &nom);
    rassemble.announcements.insert(nom.clone(), annonce.clone());
    core.set_metadata_order(register::metadata_order(&fils.ordre_manifeste, rassemble));

    let prefix = fils.sockets_dir.join(&nom);
    // Les lignes de statut sont composées à part puis **substituées** en bloc :
    // voir `status::replace_plugin_lines`.
    let mut lignes: Vec<PluginStatus> = Vec::new();

    for kind in &annonce.kinds {
        let socket = ritornello_plugin_sdk::genre_socket(&prefix, *kind);
        match kind {
            PluginKind::Source => {
                // `connect_avec_fermeture` et non `connect` : la tâche de
                // lecture du SDK se terminait sur EOF en journalisant, sans
                // prévenir personne. Un `oneshot` relayé vers la boucle, parce
                // que le SDK ne doit rien savoir de la comptabilité du cœur.
                let (ferme_tx, ferme_rx) = tokio::sync::oneshot::channel();
                let injoignable = fils.injoignable_tx.clone();
                let nom_ferme = nom.clone();
                tokio::spawn(async move {
                    // `Err` = le client a été détruit sans que le socket ferme,
                    // ce qui n'arrive qu'au remplacement du client : rien à
                    // signaler alors, le remplaçant parle pour lui.
                    if ferme_rx.await.is_ok() {
                        let _ = injoignable.send((nom_ferme, cablage)).await;
                    }
                });
                match SourceClient::connect_avec_fermeture(
                    &socket,
                    nom.clone(),
                    fils.source_update_tx.clone(),
                    Some(ferme_tx),
                )
                    .await
                {
                    Ok(client) => {
                        // Cloné avant que `cable_source_a_chaud` ne le prenne :
                        // la demande de catalogue ci-dessous s'adresse au même
                        // client.
                        let client_catalogue = client.clone();
                        // `cable_source_a_chaud` fait les trois choses que
                        // `add_source` seul ne fait pas : la langue courante
                        // (sinon un `cd` relancé à la main sur un appareil en
                        // français revient en affichant `NO DISC`), le réveil si
                        // c'est la **première** source du cœur (sinon elle est
                        // active et muette), et la publication de l'état.
                        //
                        // Premier câblage ou recâblage : c'est précisément
                        // l'événement que cherche qui débogue un greffon qui
                        // bat, et le booléen le sait.
                        match core.cable_source_a_chaud(nom.clone(), client).await {
                            Ok(true) => {
                                tracing::info!("{nom} source client replaced (plugin rewired)")
                            }
                            Ok(false) => tracing::info!("{nom} source wired for the first time"),
                            // La source **est** câblée : seul son réveil a
                            // échoué (mpv, ou la source elle-même). La ligne de
                            // statut dit donc `connected: true`, et une commande
                            // de la télécommande repassera par le même chemin.
                            Err(e) => tracing::warn!("{nom} source wired, but waking it failed: {e:#}"),
                        }
                        // Son catalogue, comme au démarrage et pour la même
                        // raison : une tâche détachée, la réponse corrélée
                        // (`Noop`) n'apprenant rien — les présélections
                        // arrivent par le canal de mises à jour. Sans cela une
                        // source annoncée en retard entrait dans le catalogue
                        // avec une liste **définitivement vide**, personne ne
                        // redemandant jamais ; et un greffon recâblé après que
                        // sa configuration a changé pendant qu'il était mort
                        // laissait le cœur sur l'ancienne liste.
                        //
                        // Détachée, donc : ce bras tourne dans la boucle
                        // principale, et l'attendre y ajouterait les 5 s du
                        // protocole des sources — la boucle ne traiterait plus
                        // une touche de télécommande pendant ce temps.
                        let nom_catalogue = nom.clone();
                        tokio::spawn(async move {
                            if let Err(e) = client_catalogue
                                .request(ritornello_proto::SourceReq::ListPresets)
                                .await
                            {
                                tracing::debug!("list_presets for {nom_catalogue}: {e}");
                            }
                        });
                        lignes.push(PluginStatus::genre(&nom, "source", true, annonce.admin));
                    }
                    Err(e) => {
                        tracing::warn!("plugin {nom} source unavailable: {e}");
                        lignes.push(PluginStatus::genre(&nom, "source", false, annonce.admin));
                    }
                }
            }
            PluginKind::Display => match DisplayClient::connect(&socket).await {
                Ok(client) => {
                    relais_afficheur(
                        nom.clone(),
                        client,
                        // Le drapeau **de l'annonce de ce greffon-là**, jamais
                        // une valeur par défaut : c'est le binaire qui a dit
                        // s'il voulait les octets (voir `Announcement::covers`).
                        annonce.covers,
                        fils.covers.clone(),
                        fils.etat_rx.clone(),
                        fils.catalogue_rx.clone(),
                        AvisInjoignable { cablage, tx: fils.injoignable_tx.clone() },
                    );
                    lignes.push(PluginStatus::genre(&nom, "display", true, annonce.admin));
                }
                Err(e) => {
                    tracing::warn!("display plugin {nom} unavailable: {e}");
                    lignes.push(PluginStatus::genre(&nom, "display", false, annonce.admin));
                }
            },
            PluginKind::Input => {
                let tx = fils.cmd_tx.clone();
                let socket_for_task = socket.clone();
                let name = nom.clone();
                let injoignable = fils.injoignable_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = run_input_client(&socket_for_task, tx).await {
                        tracing::warn!("input plugin {name} disconnected: {e}");
                    }
                    // `run_input_client` ne rend **jamais** `Ok` : il sort en
                    // erreur sur EOF comme sur canal du cœur fermé. Le second
                    // cas signale dans le vide — la boucle est partie, son
                    // récepteur avec — donc il n'a pas besoin d'être distingué.
                    let _ = injoignable.send((name, cablage)).await;
                });
                lignes.push(PluginStatus::genre(&nom, "input", true, annonce.admin));
            }
            PluginKind::Metadata => {
                let tx = fils.enrich_tx.clone();
                let np_rx = fils.now_playing_rx.clone();
                let socket_for_task = socket.clone();
                let name = nom.clone();
                let injoignable = fils.injoignable_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        run_metadata_client(&socket_for_task, name.clone(), tx, np_rx).await
                    {
                        tracing::warn!("metadata plugin {name} disconnected: {e}");
                    }
                    let _ = injoignable.send((name, cablage)).await;
                });
                lignes.push(PluginStatus::genre(&nom, "metadata", true, annonce.admin));
            }
        }
    }

    // L'ancien dorsal est retiré **avant** la tentative de connexion, et quoi
    // qu'annonce le greffon. Un dorsal survivant à une ré-annonce pointerait
    // vers un socket disparu : `/api/admin/<nom>` rendrait une erreur au bout
    // du budget de la requête, là où un 404 franc dit tout de suite qu'il n'y a
    // rien à cette adresse.
    // Les actifs partent avec le dorsal : une ré-annonce est la fin d'un
    // processus suivie du début d'un autre, et le nouveau peut porter un `ui.js`
    // reconstruit. Les garder servait l'ancien jusqu'au redémarrage du cœur.
    admin::oublie_page(&fils.admin_backends, &fils.admin_assets, &nom).await;
    let mut admin_joint = false;
    if annonce.admin {
        let chemin = ritornello_plugin_sdk::admin_socket(&prefix);
        match ritornello_plugin_sdk::AdminClient::connect(&chemin).await {
            Ok(client) => {
                fils.admin_backends.write().await.insert(nom.clone(), client);
                admin_joint = true;
            }
            Err(e) => tracing::warn!("admin plugin {nom} unreachable: {e}"),
        }
    }
    // Même règle qu'au démarrage : le drapeau suit ce qui a été effectivement
    // **joint**, pas ce que le greffon a annoncé — une annonce `admin: true`
    // dont le `connect` échoue ne doit pas laisser l'IHM pointer vers une page
    // qui répond 404. Réaffirmé sur toutes les lignes plutôt que corrigé dans le
    // seul cas d'échec : une seule vérité, écrite une seule fois.
    for ligne in lignes.iter_mut() {
        ligne.admin = admin_joint;
    }

    // **Remplacer, jamais ajouter** : un greffon qui se réannonce accumulerait
    // sinon une ligne de plus à chaque relance. Le remplacement par une liste
    // vide garde le greffon visible en genre inconnu, voir
    // `status::replace_plugin_lines` : une annonce à `kinds: []` doit signaler
    // un greffon mal compilé, pas le faire disparaître de la page.
    {
        let mut statuts = fils.status_state.write().await;
        status::replace_plugin_lines(&mut statuts, &nom, lignes, admin_joint);
    }
}

/// Construit le `Core` et l'`AppState` HTTP avec **le même** `Arc<CoverCache>`
/// remis aux deux : c'est cette fonction qui construit ce cache, jamais
/// `main` directement, et c'est elle — pas une relecture du code de `main` —
/// qu'un test appelle pour vérifier le partage par `Arc::ptr_eq` (voir
/// `core::tests::le_coeur_et_lappstate_partagent_reellement_le_meme_arc`).
/// Une régression où `main` reconstruirait un second cache pour l'un des
/// deux romprait cette égalité au premier appel, pas seulement à la lecture
/// du diff.
///
/// `squelette.covers` est ignoré : il n'existe que pour éviter à l'appelant
/// de construire l'`AppState` en deux morceaux — tous ses autres champs
/// traversent inchangés.
pub(crate) fn assemble_covers_et_core<P: player::Player>(
    player: P,
    cablage: core::Cablage,
    pochette_tx: mpsc::Sender<(String, bool)>,
    extraction_tx: mpsc::Sender<(String, Option<ritornello_proto::CoverRef>)>,
    squelette: AppState,
) -> (AppState, core::Core<P>) {
    let covers = Arc::new(cover::CoverCache::new());
    let coeur = core::Core::new(player, cablage, covers.clone(), pochette_tx, extraction_tx);
    let app_state = AppState { covers, ..squelette };
    (app_state, coeur)
}

/// Éteint un greffon : on demande sa mort, puis on retire **tout** ce que le
/// cœur tenait de lui.
///
/// Le décâblage est fait ici et non au retour de sa mort : la page attend une
/// réponse, et elle doit décrire un état déjà vrai. Le processus, lui, meurt à
/// son rythme — au pire deux secondes plus tard, `SIGKILL` en main — et sa
/// sortie ne fera plus que produire une ligne de journal.
///
/// Les afficheurs et les entrées n'ont rien d'explicite à retirer : leurs
/// relais sortent de boucle au premier échec d'envoi ou sur EOF, ce que la
/// mort du socket provoque.
///
/// Pour un afficheur, cela vaut bien pour ses **deux** canaux : `relais_afficheur`
/// tient un récepteur d'état et un récepteur de catalogue, et les deux bras de
/// son `select!` reversent leur résultat d'envoi dans le même contrôle d'erreur
/// — quel que soit celui qui se réveille le premier après la mort du socket, la
/// tâche sort.
///
/// Le canal du catalogue ajoute une occasion de le remarquer plus tôt, **mais
/// Ce que le cœur sait d'un greffon, une fois ses **deux** registres croisés.
///
/// `kill_triggers` ne signifie que « lancé par nous et pas encore moissonné » —
/// son propre commentaire le dit. Les deux gardes de la bascule s'en servaient
/// pourtant comme oracle de vie, et manquaient donc exactement le cas pour
/// lequel elles avaient été écrites : un greffon **vivant** que le cœur ne
/// supervise pas. L'allumage relançait un second processus qui volait le préfixe
/// de sockets du premier ; l'extinction décâblait tout, posait `desactive` et
/// rendait `true`, si bien que l'IHM affichait « inactif » pendant que le
/// processus tournait avec ses sockets.
///
/// `announcements` ne pouvait pas servir d'oracle non plus, et c'est contre
/// l'intuition : la branche de décès de la boucle principale **ne le purge
/// pas** (seule `eteindre_a_chaud` le fait). Un greffon planté y garde son
/// annonce, donc s'y fier aurait fait *refuser* l'extinction d'un greffon
/// planté — le cas le plus courant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vivacite {
    /// Aucun processus connu : ni lancé par le cœur, ni annoncé hors de sa
    /// supervision.
    Eteint,
    /// Lancé par le cœur et pas encore moissonné : il tient de quoi l'arrêter.
    Supervise,
    /// Vivant et **hors d'atteinte** : il a parlé, le cœur ne tient pas son
    /// `child`. Relance à la main, superviseur système, ou `child.wait()` déjà
    /// consommée par le rendez-vous de démarrage.
    ///
    /// **Cet état n'est plus définitif.** Le cœur ne verra jamais le code de
    /// sortie d'un tel processus, mais il voit ses sockets se fermer, et il en
    /// déduit qu'il n'est plus joignable : le nom quitte alors `non_supervises`
    /// et redevient `Eteint`, donc allumable. Voir le bras `injoignable_rx` de
    /// la boucle principale.
    HorsAtteinte,
}

/// Croise les deux registres pour un nom.
///
/// `Supervise` l'emporte quand les deux répondent : ce que le cœur peut
/// arrêter primerait sur ce qu'il ne peut que constater. La conjonction est
/// **inatteignable**, mais plus pour la raison qui était écrite ici.
///
/// L'ancien argument était qu'un nom classé `HorsAtteinte` ne peut pas revenir
/// dans `kill_triggers`, la garde d'allumage refusant de lancer un processus
/// pour lui — donc jamais dans les deux tables. Ce n'est plus vrai : depuis que
/// la fermeture des sockets est observée, un nom **sort** de `non_supervises`
/// quand son processus cesse d'être joignable, et il peut être rallumé ensuite.
///
/// La conclusion tient quand même, et par un chemin plus court : la sortie de
/// `non_supervises` précède toujours l'allumage qui l'inscrirait dans
/// `kill_triggers` — c'est ce qui rend cet allumage possible. Les deux
/// appartenances restent donc exclusives à tout instant. L'ordre est écrit
/// quand même : il rend la fonction totale sans dépendre de cet argument.
fn vivacite(
    nom: &str,
    kill_triggers: &HashMap<String, tokio::sync::oneshot::Sender<()>>,
    non_supervises: &HashSet<String>,
) -> Vivacite {
    if kill_triggers.contains_key(nom) {
        Vivacite::Supervise
    } else if non_supervises.contains(nom) {
        Vivacite::HorsAtteinte
    } else {
        Vivacite::Eteint
    }
}

/// seulement pour un greffon qui possédait une source** : c'est son retrait qui
/// republie le catalogue. Pour un greffon purement afficheur — la console, et le
/// greffon MPD lui-même — `remove_source` rend `Ok(false)`, rien n'est republié,
/// et en veille (où aucun tick d'état n'est armé) le relais mort reste garé
/// jusqu'au prochain réveil. Sans conséquence : il ne consomme rien en
/// attendant, et il sortira au premier envoi. Mais la ligne de statut dit
/// « déconnecté » avant que la tâche n'ait constaté quoi que ce soit, et c'est
/// voulu — l'accusé décrit un état déjà vrai, pas l'instant où la tâche
/// l'apprend.
async fn eteindre_a_chaud<P: player::Player>(
    nom: &str,
    fils: &FilsChaud,
    core: &mut core::Core<P>,
    rassemble: &mut register::Gathered,
    kill_triggers: &mut HashMap<String, tokio::sync::oneshot::Sender<()>>,
    non_supervises: &HashSet<String>,
) -> bool {
    // Rien pour l'arrêter : décâbler quand même et poser `desactive` rendrait
    // « inactif » un greffon qui tourne toujours avec son port et ses sockets.
    // Le refus est la seule réponse vraie, et le journal nomme le remède.
    if vivacite(nom, kill_triggers, non_supervises) == Vivacite::HorsAtteinte {
        tracing::warn!(
            "refusing to disable {nom}: it is alive but the core does not own its process, so it cannot be stopped — kill it yourself, or restart the core to let it take ownership again"
        );
        return false;
    }
    tracing::info!("disabling plugin {nom}: killing it and unwiring everything it served");
    if let Some(tx) = kill_triggers.remove(nom) {
        // Le récepteur est dans la future de supervision : une erreur
        // d'envoi voudrait dire qu'elle est déjà finie, donc que le processus
        // est déjà mort. Rien à rattraper.
        let _ = tx.send(());
    }
    if let Err(e) = core.remove_source(nom).await {
        tracing::warn!("unwiring source {nom}: {e:#}");
    }
    // Le nom sort du rassemblement, puis l'ordre d'arbitrage est recalculé en
    // **entier** depuis le manifeste — le chemin qu'emprunte déjà toute
    // annonce tardive, et la seule façon qu'un greffon rallumé retrouve sa
    // priorité de fichier.
    rassemble.announcements.remove(nom);
    rassemble.figes.retain(|n| n != nom);
    rassemble.morts.retain(|n| n != nom);
    core.set_metadata_order(register::metadata_order(&fils.ordre_manifeste, rassemble));
    // Retiré, sinon `/plugins/<nom>/` attendrait le budget de la requête pour
    // finir en erreur, là où un 404 franc dit tout de suite qu'il n'y a rien à
    // cette adresse.
    admin::oublie_page(&fils.admin_backends, &fils.admin_assets, nom).await;
    let mut statuts = fils.status_state.write().await;
    status::replace_plugin_lines(&mut statuts, nom, vec![PluginStatus::desactive(nom)], false);
    statuts.active_source = core.active_source().to_string();
    true
}

/// Rallume un greffon : on relance son binaire, et c'est tout.
///
/// Le câblage n'est **pas** fait ici : le greffon va s'annoncer sur le socket
/// d'enregistrement, que le cœur tient ouvert pour la vie du processus, et
/// `cabler_a_chaud` fera le reste. C'est le chemin d'un greffon relancé à la
/// main, déjà éprouvé.
///
/// C'est aussi ce qui redemande ses présélections à une source rallumée, et il
/// n'y a rien à faire de plus ici : `eteindre_a_chaud` a vidé son entrée de
/// `presets_par_source` (voir `Core::remove_source`), donc le catalogue la
/// donnerait vide — mais `cabler_a_chaud` détache un `ListPresets` sur **tout**
/// câblage de source, premier ou non, et la liste revient par le canal de mises
/// à jour. Un greffon dont la configuration a changé pendant qu'il était éteint
/// est donc relu, jamais hérité.
///
/// D'ici là, la ligne dit « figé » : lancé, pas encore annoncé. C'est
/// exactement ce que le mot veut dire, et la page n'a pas besoin d'un
/// quatrième état pour une poignée de secondes.
///
/// Rend `false` si le binaire n'a pas pu être lancé — le chemin d'`exec` a
/// changé, le fichier n'est plus exécutable. La cause précise part au journal,
/// que l'IHM affiche déjà dans sa popin d'erreurs.
async fn rallume(
    nom: &str,
    exec: &str,
    generation: u64,
    fils: &FilsChaud,
    register_path: &Path,
    locale: Option<&str>,
    kill_triggers: &mut HashMap<String, tokio::sync::oneshot::Sender<()>>,
) -> Option<SortieGreffon> {
    let prefix = fils.sockets_dir.join(nom);
    match plugins::spawn(exec, register_path, nom, &prefix, locale) {
        Ok(child) => {
            tracing::info!("plugin {nom} re-enabled, launched again");
            let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
            kill_triggers.insert(nom.to_string(), kill_tx);
            let mut statuts = fils.status_state.write().await;
            // « Démarrage » et non « figé » : le binaire vient d'être lancé,
            // il n'a pas encore eu le temps de lier ses sockets. C'est la
            // boucle du cœur qui rétrograde en « figé » au bout de
            // `DELAI_DEMARRAGE`, si rien n'est venu.
            status::replace_plugin_lines(&mut statuts, nom, vec![PluginStatus::demarrage(nom)], false);
            Some(supervise(nom.to_string(), generation, child, kill_rx))
        }
        Err(e) => {
            tracing::warn!("failed to launch plugin {nom}: {e:#}");
            None
        }
    }
}

/// Vrai pour une trame que le cœur accepte d'écrire au journal.
///
/// **Elle n'écarte qu'une chose : le bavardage de `lofty` sous le niveau
/// erreur.** `player::mpv::pochette_embarquee` ouvre le fichier joué avec
/// `lofty` pour en extraire une pochette, donc **à chaque changement de
/// piste**, et `lofty` y émet un `WARN` par MP3 sans en-tête Xing —
/// « MPEG: Using bitrate to estimate duration ». Ce n'est pas un incident :
/// c'est la méthode d'estimation normale pour ce format, elle n'appelle aucune
/// action, et elle se répète par piste.
///
/// Le coût est double, et c'est ce qui la rend nuisible plutôt que seulement
/// bruyante : elle noie le journal, **et** elle chasse de vraies erreurs du
/// tampon des « dernières erreurs », qui ne retient que les `WARN` et au-delà.
///
/// Le même filtre existe dans le greffon `files`, qui sonde les durées avec la
/// même bibliothèque. Deux copies d'une règle de trois lignes, plutôt qu'un
/// crate partagé pour l'occasion — mais si une troisième apparaît, c'est le
/// signe qu'il faut le crate.
///
/// `lofty` garde ses `ERROR` : une trame que la bibliothèque juge fautive reste
/// une information.
fn trame_a_journaliser(metadata: &tracing::Metadata<'_>) -> bool {
    // `>` et non `<` : dans `tracing`, l'ordre des niveaux est celui de la
    // verbosité, donc `ERROR` est le plus **petit**. « Plus verbeux qu'erreur »
    // s'écrit bien `> Level::ERROR`.
    !(metadata.target().starts_with("lofty") && *metadata.level() > tracing::Level::ERROR)
}

#[tokio::main]
async fn main() -> Result<()> {
    // 500 et non 50 : l'IHM a désormais une popin qui liste tout le tampon
    // derrière un filtre, et 50 lignes ne remontent pas plus loin que la carte
    // qui en affiche déjà les dernières. 500 lignes pèsent quelques dizaines de
    // kio, relevées une fois par ouverture de popin — pas à chaque sondage.
    let log_buffer = Arc::new(LogBuffer::new(500));
    let log_buffer_for_writer = log_buffer.clone();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(move || LogBufferWriter(log_buffer_for_writer.clone()))
                .with_filter(LevelFilter::WARN),
        )
        // Pose sur le registre et non sur une couche : les deux couches
        // ci-dessus doivent l'ignorer, le terminal comme le tampon.
        .with(tracing_subscriber::filter::filter_fn(trame_a_journaliser))
        .init();

    // Balaie les fichiers temporaires d'une exécution précédente avant que
    // quoi que ce soit ne puisse en recréer : voir `cover::purge_temporaires`
    // pour la raison (accumulation, pas fraîcheur — celle-ci est déjà
    // garantie ailleurs).
    cover::purge_temporaires();

    let plugins_path = PathBuf::from(env_or("RITORNELLO_PLUGINS", "/etc/ritornello/plugins.toml"));
    let state_path = PathBuf::from(env_or("RITORNELLO_STATE", "/var/lib/ritornello/state.json"));
    let mpv_socket = PathBuf::from(env_or("RITORNELLO_MPV_SOCKET", "/run/ritornello/mpv.sock"));
    let mpv_bin = env_or("RITORNELLO_MPV_BIN", "mpv");
    let cd_dev = env_or("RITORNELLO_CD_DEV", "/dev/sr0");
    let http_addr = env_or("RITORNELLO_HTTP", "0.0.0.0:8080");
    let runtime_dir = env_or("RITORNELLO_RUNTIME_DIR", "/run/ritornello");

    let manifest = PluginManifest::load(&plugins_path)
        .with_context(|| format!("loading {}", plugins_path.display()))?;
    let persisted = state::load(&state_path);

    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let catalog = Arc::new(RwLock::new(ritornello_i18n::Catalog::load(
        "core",
        persisted.locale.as_deref().unwrap_or("en"),
        &locales_root,
        i18n::EN,
    )));

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<InputMessage>(32);
    // Événements de mpv : un `mpsc`, pas un `broadcast` — il n'y a qu'un
    // consommateur (la boucle ci-dessous), et la sémantique avec perte de
    // `broadcast` (`Lagged`) pouvait jeter un `PlaybackIdle` que mpv, qui ne
    // signale que les transitions, n'aurait jamais réémis : flux coupé sans
    // relance jusqu'à la prochaine action. Ici, canal plein = contre-pression
    // sur la pompe d'événements, jamais de perte.
    let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(64);
    let (source_update_tx, mut source_update_rx) = mpsc::channel::<(String, SourceUpdate)>(32);
    // Ce qui joue, vers les plugins `metadata` : un `watch`, parce que seule la
    // dernière valeur compte et qu'un plugin lent ne doit pas bloquer le cœur.
    let (now_playing_tx, now_playing_rx) = watch::channel(NowPlaying {
        source: persisted.active_source.clone(),
        identity: None,
        known: Known::default(),
    });
    // État structuré du lecteur : vers la SPA (route SSE) et vers les plugins
    // Display, qui composent eux-mêmes leur mise en page depuis cette même
    // trame (un seul canal depuis la Task 4 de « afficheurs, état structuré »).
    let (etat_tx, etat_rx) = watch::channel(PlayerState {
        source: persisted.active_source.clone(),
        ..Default::default()
    });
    // Catalogue des sources : vers les plugins Display **seulement**, sur son
    // propre canal. Vide au départ, `Core::new` le publie dès qu'il connaît ses
    // sources — le relais d'un afficheur envoie la valeur courante à la
    // connexion, donc un afficheur câblé avant cette publication reçoit le
    // catalogue réel au changement qui suit.
    let (catalogue_tx, catalogue_rx) = watch::channel(Catalogue::default());
    let (enrich_tx, mut enrich_rx) = mpsc::channel::<(String, Enrichment)>(32);
    let (audio_tx, mut audio_rx) = mpsc::channel::<Option<String>>(4);
    let (locale_tx, mut locale_rx) = mpsc::channel::<String>(4);
    let (theme_tx, mut theme_rx) = mpsc::channel::<theme::ThemeState>(4);
    let (settings_tx, mut settings_rx) = mpsc::channel::<state::Settings>(4);
    let (greffon_tx, mut greffon_rx) = mpsc::channel::<status::OrdreGreffon>(4);

    // mpv. Les deux durées de tampon sont réglables sans recompiler : la bonne
    // valeur dépend du réseau et de la charge de la machine, pas du code.
    let audio_buffer_brut = std::env::var("RITORNELLO_AUDIO_BUFFER").ok();
    let readahead_brut = std::env::var("RITORNELLO_NETWORK_READAHEAD").ok();
    let audio_buffer = player::mpv::audio_buffer_regle(audio_buffer_brut.as_deref());
    let readahead = player::mpv::readahead_regle(readahead_brut.as_deref());
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, audio_buffer, readahead, ev_tx)
            .await
            .context("starting mpv")?;

    // Répertoire neuf, puis le socket d'enregistrement lié AVANT tout
    // lancement : un greffon qui démarre vite trouve toujours quelqu'un.
    let sockets_dir = plugins::prepare_sockets_dir(Path::new(&runtime_dir))?;
    let register_path = sockets_dir.join("register.sock");
    let register_listener = tokio::net::UnixListener::bind(&register_path)
        .with_context(|| format!("binding {}", register_path.display()))?;

    let mut plugin_waits: FuturesUnordered<SortieGreffon> = FuturesUnordered::new();
    let mut lances: Vec<String> = Vec::new();
    let mut plugin_statuses = Vec::new();
    // Déclencheurs d'extinction, un par lancement : c'est la seule prise sur
    // un `Child` déplacé dans sa future de supervision. L'invariant visé —
    // une entrée vit exactement le temps d'un processus lancé et non encore
    // moissonné — est tenu par **trois** points de purge, pas un seul : le
    // bras `plugin_waits.next()` la retire dès qu'une mort qu'il traite
    // concerne l'incarnation courante (génération qui correspond, une mort
    // périmée n'y touche pas, l'entrée appartenant déjà au processus
    // relancé) ; le nettoyage juste après le rendez-vous de démarrage
    // (`rassemble.morts`) la retire pour les greffons morts *pendant* ce
    // rendez-vous, dont `plugin_waits` ne reverra jamais la mort — voir
    // `gather` et le commentaire de `cabler_a_chaud` sur les annonces
    // tardives ; et `eteindre_a_chaud` la retire elle-même dès l'extinction
    // demandée depuis l'IHM, sans attendre que le processus tué soit
    // effectivement moissonné. Envoyer malgré tout à une supervision déjà
    // terminée échoue simplement, sans effet : c'est pour cela que le
    // résultat de l'envoi est ignoré partout où on l'utilise.
    let mut kill_triggers: HashMap<String, tokio::sync::oneshot::Sender<()>> = HashMap::new();
    // L'autre moitié de l'oracle de vie : les greffons qui se sont annoncés
    // sans que le cœur tienne leur processus. Voir `Vivacite`, qui dit pourquoi
    // `kill_triggers` seul mentait dans les deux sens.
    //
    // **Ce registre ne se purge jamais**, et c'est assumé plutôt que subi : la
    // mort d'un processus que le cœur ne supervise pas est par définition
    // inobservable, donc aucun site ne pourrait retirer un nom sans deviner. Un
    // greffon classé ici reste donc ingérable depuis l'IHM jusqu'au prochain
    // démarrage du cœur — les deux gardes rendent `false` et le disent au
    // journal. Le gel est honnête ; le faire passer pour un non-événement, non.
    // La vraie réponse est de sortir la vivacité de `kill_triggers`
    // (« suite documentée » du chantier greffons actifs/inactifs), ce qui
    // redessine leur table et appartient à la session qui possède ce code.
    let mut non_supervises: HashSet<String> = HashSet::new();
    // Quand chaque greffon lancé cesse d'avoir le bénéfice du doute.
    //
    // Une entrée y est posée au lancement et retirée **uniquement** par le
    // balayage d'échéance, qui décide alors en relisant la ligne de statut
    // plutôt qu'en se fiant à la table. C'est délibéré, et c'est la leçon de
    // `kill_triggers` : un registre dont la justesse dépend de trois sites de
    // purge finit par mentir sur l'un d'eux. Ici, qu'un greffon se soit
    // annoncé, soit mort ou ait été éteint entre-temps n'a pas besoin d'être
    // signalé à la table — sa ligne le dit déjà.
    let mut demarrages: HashMap<String, tokio::time::Instant> = HashMap::new();
    // Génération de lancement, par nom. Éteindre puis rallumer aussitôt fait
    // arriver la mort de l'**ancien** processus après le câblage du nouveau :
    // sans ce compteur, cette mort effacerait des lignes de statut qui
    // décrivent déjà le nouveau. Voir le bras `plugin_waits.next()`.
    let mut generations: HashMap<String, u64> = HashMap::new();
    // Génération de **câblage**, par nom, et distincte de `generations` juste
    // au-dessus — les confondre casserait l'une ou l'autre.
    //
    // `generations` compte les **lancements de processus** : le bras
    // `plugin_waits` compare la génération que lui rend la supervision à celle
    // de la table, et la bosseler ailleurs qu'au lancement ferait ignorer une
    // mort réelle. `cablages` compte les **câblages de sockets**, qui arrivent
    // en plus : un greffon relancé à la main se recâble sans que le cœur l'ait
    // lancé.
    //
    // Pourquoi ce numéro est indispensable. Un relais d'afficheur n'apprend la
    // mort de son pair qu'au **prochain envoi**, qui peut n'arriver que des
    // minutes plus tard, faute de changement d'état. Soit : le greffon meurt,
    // l'utilisateur le relance à la main trente secondes après, il se
    // réannonce, ses lignes repassent à connecté — puis un morceau change, le
    // *vieux* relais se réveille enfin, échoue et signale. Sans le numéro, ce
    // signal marquerait déconnecté un greffon qui vient de se rebrancher, et
    // seul un nouveau changement de piste l'aurait réparé.
    let mut cablages: HashMap<String, u64> = HashMap::new();
    // Les sockets qui se ferment. Voir `FilsChaud::injoignable_tx`.
    //
    // Borné à 16 : un envoi est une fin de tâche, jamais une cadence. Si le
    // canal se remplissait — huit greffons mourant tous en même temps, deux
    // fois — les émetteurs attendraient leur tour au lieu de perdre l'avis, ce
    // qui est le bon compromis pour un message dont l'oubli laisserait une
    // ligne mentir indéfiniment.
    let (injoignable_tx, mut injoignable_rx) = mpsc::channel::<(String, u64)>(16);

    for p in &manifest.plugins {
        generations.insert(p.name.clone(), 0);
        if !p.enabled {
            // Éteint : on ne lance rien, mais la ligne reste — sans elle, la
            // page ne le montrerait plus et il serait irrécupérable.
            tracing::info!("plugin {} is disabled, not launching it", p.name);
            plugin_statuses.push(PluginStatus::desactive(&p.name));
            continue;
        }
        let prefix = sockets_dir.join(&p.name);
        match plugins::spawn(
            &p.exec,
            &register_path,
            &p.name,
            &prefix,
            persisted.locale.as_deref(),
        ) {
            Ok(child) => {
                let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
                kill_triggers.insert(p.name.clone(), kill_tx);
                plugin_waits.push(supervise(p.name.clone(), 0, child, kill_rx));
                lances.push(p.name.clone());
            }
            Err(e) => {
                // `{e:#}` et non `{e}` : la chaîne de contexte porte le chemin
                // cherché, que le seul message d'erreur système n'indique pas.
                tracing::warn!("failed to launch plugin {}: {e:#}", p.name);
                // Un greffon qui n'a pas démarré n'a jamais annoncé de genre,
                // et le manifeste ne le porte plus : la page de statut affiche
                // un genre inconnu plutôt que d'en inventer un.
                plugin_statuses.push(PluginStatus::genre_inconnu(&p.name, false));
            }
        }
    }

    // Le canal des annonces, **unique pour les deux étages** : le rendez-vous
    // l'emprunte, la tâche permanente en garde l'émetteur, et la boucle de
    // sélection en consomme le reste. Un seul canal, et une annonce ne peut plus
    // se perdre entre les deux : ce que `gather` n'a pas eu le temps de lire
    // reste en file, et sera câblé à chaud un instant plus tard. Voir la doc de
    // `register::gather` pour la course que cela supprime.
    let (tardives_tx, mut tardives_rx) = mpsc::channel::<Announcement>(16);

    // Une annonce par greffon lancé. Les morts précoces écourtent l'attente ;
    // `plugin_waits` reste utilisable ensuite, seules les entrées consommées
    // ici en sortent — et ce sont précisément celles dont on a déjà appris la
    // mort.
    let mut rassemble = register::gather(
        &register_listener,
        &lances,
        (&mut plugin_waits).map(|(nom, _gen, _statut, _voulue)| nom),
        std::time::Duration::from_secs(10),
        &tardives_tx,
        &mut tardives_rx,
    )
    .await;

    // Les morts de ce rassemblement ont été consommées directement sur
    // `plugin_waits` par `gather` (voir sa doc, et le commentaire de
    // `cabler_a_chaud` sur les annonces tardives) : la boucle principale ne
    // les reverra donc jamais sur son bras `plugin_waits.next()`, qui est
    // l'un des deux autres endroits purgeant `kill_triggers` (le troisième
    // étant `eteindre_a_chaud`). Sans ce nettoyage-ci, un greffon mort
    // *pendant* le rendez-vous laisserait une entrée périmée, et un allumage
    // ultérieur la prendrait pour un processus vivant sans jamais relancer le
    // binaire.
    for nom in &rassemble.morts {
        kill_triggers.remove(nom);
    }

    // `gather` a pris le listener par **référence** : le cœur en garde donc la
    // propriété, et le socket d'enregistrement ne se ferme pas avec le
    // rendez-vous. L'échéance ci-dessus ne condamne plus personne — elle sert à
    // ne pas bloquer le démarrage et à nommer un greffon figé. Un greffon qui
    // s'annonce à t+12 s (démarrage à froid sur carte SD, huit binaires qui
    // montent leur runtime en même temps) est câblé à chaud, et un greffon
    // relancé à la main est repris.
    tokio::spawn(register::accept_forever(register_listener, tardives_tx));

    // Une ligne « genre inconnu » par greffon non annoncé, en distinguant le
    // figé du mort : le premier tourne toujours et peut encore s'annoncer, le
    // second n'a plus rien à dire. C'est la différence que l'opérateur doit
    // voir avant d'aller relancer quoi que ce soit.
    for (nom, fige) in rassemble
        .figes
        .iter()
        .map(|n| (n, true))
        .chain(rassemble.morts.iter().map(|n| (n, false)))
    {
        plugin_statuses.push(PluginStatus::genre_inconnu(nom, fige));
    }

    // Plugins `metadata` annoncés, **dans l'ordre du manifeste** : cet ordre
    // est la priorité d'arbitrage, et c'est une propriété de configuration,
    // pas d'exécution. La liste est donc reconstruite depuis le manifeste et
    // jamais depuis l'ordre d'arrivée des annonces, qui rendrait l'affichage
    // non reproductible d'un démarrage à l'autre.
    let ordre_manifeste: Vec<String> = manifest.plugins.iter().map(|p| p.name.clone()).collect();
    let metadata_plugins = register::metadata_order(&ordre_manifeste, &rassemble);
    // L'ordre du fichier arbitre les `metadata` ; l'`exec`, lui, ne servait
    // qu'au lancement initial. Rallumer un greffon le redemande.
    let execs: HashMap<String, String> =
        manifest.plugins.iter().map(|p| (p.name.clone(), p.exec.clone())).collect();

    // La page d'admin est **annoncée** par le binaire, plus observée par une
    // fenêtre d'attente : le drapeau des statuts part de la ligne
    // d'enregistrement. Mais l'annonce n'est qu'une déclaration de fichier —
    // c'est une capacité **observée** que l'IHM doit voir au final : si la
    // connexion admin échoue plus bas, le drapeau est repassé à `false` sur
    // toutes les lignes de ce nom, quel que soit leur genre.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    // Le nom voyage avec le client : c'est lui qui nomme le greffon dans le
    // journal quand son relais s'arrête.
    // Le drapeau `covers` de l'annonce voyage avec le client : au moment de
    // spawner les relais (plus bas, après `Core::new`), l'annonce n'est plus
    // sous la main, et rien ne doit reconstruire ce drapeau autrement qu'en le
    // recopiant depuis ce que le greffon a annoncé.
    let mut display_clients: Vec<(String, Arc<DisplayClient>, bool)> = Vec::new();
    let mut admin_backends: HashMap<String, Arc<dyn admin::AdminBackend>> = HashMap::new();

    for nom in &ordre_manifeste {
        let Some(annonce) = rassemble.announcements.get(nom) else {
            continue;
        };
        let prefix = sockets_dir.join(nom);

        for kind in &annonce.kinds {
            let socket = ritornello_plugin_sdk::genre_socket(&prefix, *kind);
            // L'annonce prouve que le socket est lié : un `connect` nu suffit,
            // plus de boucle de reprise. Un échec ici est une vraie anomalie,
            // pas une course au démarrage — et il reste cantonné à ce genre,
            // les autres genres du même greffon continuant d'être câblés.
            match kind {
                PluginKind::Source => {
                    // Voir le même geste dans `cabler_a_chaud`, qui dit
                    // pourquoi le SDK ne connaît pas la comptabilité du cœur.
                    let (ferme_tx, ferme_rx) = tokio::sync::oneshot::channel();
                    let injoignable = injoignable_tx.clone();
                    let nom_ferme = nom.clone();
                    tokio::spawn(async move {
                        if ferme_rx.await.is_ok() {
                            let _ = injoignable.send((nom_ferme, 0)).await;
                        }
                    });
                    match SourceClient::connect_avec_fermeture(
                        &socket,
                        nom.clone(),
                        source_update_tx.clone(),
                        Some(ferme_tx),
                    )
                    .await
                    {
                        Ok(client) => {
                            sources.insert(nom.clone(), client);
                            plugin_statuses.push(PluginStatus::genre(nom, "source", true, annonce.admin));
                        }
                        Err(e) => {
                            tracing::warn!("plugin {nom} source unavailable: {e}");
                            plugin_statuses.push(PluginStatus::genre(nom, "source", false, annonce.admin));
                        }
                    }
                }
                PluginKind::Display => match DisplayClient::connect(&socket).await {
                    Ok(client) => {
                        display_clients.push((nom.clone(), client, annonce.covers));
                        plugin_statuses.push(PluginStatus::genre(nom, "display", true, annonce.admin));
                    }
                    Err(e) => {
                        tracing::warn!("display plugin {nom} unavailable: {e}");
                        plugin_statuses.push(PluginStatus::genre(nom, "display", false, annonce.admin));
                    }
                },
                PluginKind::Input => {
                    let tx = cmd_tx.clone();
                    let socket_for_task = socket.clone();
                    let name = nom.clone();
                    let injoignable = injoignable_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = run_input_client(&socket_for_task, tx).await {
                            tracing::warn!("input plugin {name} disconnected: {e}");
                        }
                        // `0` : c'est le câblage du rendez-vous de démarrage, et
                        // `cablages` ne porte encore aucune entrée — sa valeur
                        // par défaut est donc lue comme `0` du côté de la
                        // boucle, ce qui fait de ce socket l'incarnation
                        // courante tant que personne n'a recâblé ce nom.
                        let _ = injoignable.send((name, 0)).await;
                    });
                    plugin_statuses.push(PluginStatus::genre(nom, "input", true, annonce.admin));
                }
                PluginKind::Metadata => {
                    // Relais dans les deux sens, dans sa propre tâche : sa
                    // panne ne concerne que les métadonnées. **La lecture
                    // n'est jamais affectée** par un plugin `metadata`.
                    let tx = enrich_tx.clone();
                    let np_rx = now_playing_rx.clone();
                    let socket_for_task = socket.clone();
                    let name = nom.clone();
                    let injoignable = injoignable_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            run_metadata_client(&socket_for_task, name.clone(), tx, np_rx).await
                        {
                            tracing::warn!("metadata plugin {name} disconnected: {e}");
                        }
                        let _ = injoignable.send((name, 0)).await;
                    });
                    plugin_statuses.push(PluginStatus::genre(nom, "metadata", true, annonce.admin));
                }
            }
        }

        if annonce.admin {
            let chemin = ritornello_plugin_sdk::admin_socket(&prefix);
            match ritornello_plugin_sdk::AdminClient::connect(&chemin).await {
                Ok(client) => {
                    admin_backends.insert(nom.clone(), client);
                }
                Err(e) => {
                    tracing::warn!("admin plugin {nom} unreachable: {e}");
                    // Le drapeau des statuts suit ce qui a ete effectivement
                    // joint, pas ce que le greffon a annonce : une annonce
                    // `admin: true` suivie d'un `connect` en echec ne doit
                    // jamais laisser l'IHM pointer vers une page qui repond
                    // 404. On repasse ici a `false` toutes les lignes de ce
                    // nom, quel que soit leur genre, poussees plus haut dans
                    // la boucle des genres qui precede cette connexion.
                    for statut in plugin_statuses.iter_mut().filter(|s| s.name == *nom) {
                        statut.admin = false;
                    }
                }
            }
        }
    }

    // Sous verrou à partir d'ici : le câblage de démarrage est fini, mais la
    // table n'est plus figée pour autant — un greffon qui s'annonce en retard
    // doit voir sa page d'admin apparaître sans redémarrage du cœur.
    let admin_backends: admin::AdminBackends = Arc::new(RwLock::new(admin_backends));
    // Nommé plutôt que construit dans le littéral de l'`AppState` : la boucle
    // principale et `cabler_a_chaud` doivent purger **ce** cache-là, celui que
    // les routes lisent, et non une copie neuve.
    let admin_assets: Arc<admin::AssetCache> = Arc::new(Default::default());

    // Démarrer **sans aucune source** est légitime depuis l'enregistrement à
    // chaud, et c'était la dernière échéance qui condamnait : refuser de
    // démarrer à t+10 s contredit l'idée qu'une source peut arriver à t+30 s, et
    // supprime la page de statut précisément quand on voudrait y voir le greffon
    // figé. Il n'y aura rien à lire, mais on peut déjà voir ce qui se passe.
    //
    // Reste un seul refus, qui n'est pas une lenteur mais une erreur de
    // configuration : des greffons déclarés actifs, et plus **aucun
    // processus vivant** pour s'annoncer. Voir `register::demarrage_refuse`.
    let actifs_declares = manifest.plugins.iter().filter(|p| p.enabled).count();
    if register::demarrage_refuse(actifs_declares, &lances, &rassemble) {
        anyhow::bail!(
            "no plugin process alive (every enabled plugin failed to launch or exited)"
        );
    }
    if actifs_declares == 0 {
        tracing::warn!(
            "every plugin is disabled in plugins.toml: starting anyway so they can be re-enabled from the admin UI"
        );
    }
    if sources.is_empty() {
        tracing::warn!(
            "no source plugin connected, starting anyway: a source that announces itself later will be wired without a restart"
        );
    }

    // Page de statut du cœur (plugins, source active, dernières erreurs, sortie audio).
    let status_state = Arc::new(RwLock::new(StatusState {
        plugins: plugin_statuses,
        active_source: persisted.active_source.clone(),
    }));
    let audio_current = Arc::new(RwLock::new(persisted.audio_device.clone()));
    let locale_current = Arc::new(RwLock::new(persisted.locale.clone()));
    // `state.json` est relu sans garantie : `theme_put` valide le chemin HTTP,
    // mais un fichier d'etat corrompu ou edite a la main peut porter n'importe
    // quoi. Un nom de theme inconnu fait sortir `applyTheme` cote SPA sans
    // poser une seule variable CSS, et `theme.css` n'a pas de valeur de repli :
    // l'IHM s'affiche entierement non themee. `from_persisted` valide et
    // retombe sur les defauts en journalisant un avertissement.
    let theme_current = Arc::new(RwLock::new(theme::from_persisted(
        persisted.theme.as_deref(),
        persisted.mode.as_deref(),
    )));
    let settings_current = Arc::new(RwLock::new(persisted.settings.clone()));
    // Résultats des récupérations détachées du cœur : la tâche que
    // `Core::lance_pochette` détache y dépose une clé une fois les octets en
    // main (ou déjà en cache), et la boucle `select!` ci-dessous les
    // consomme pour publier l'URL locale au bon morceau.
    let (pochette_tx, mut pochette_rx) = mpsc::channel::<(String, bool)>(4);
    // Résultat d'une extraction détachée de pochette embarquée (voir
    // `Core::handle_path`) : même principe que `pochette_tx` ci-dessus, sur
    // un canal séparé plutôt qu'un enrichissement du même — les deux portent
    // des charges utiles différentes, et rien ne les synchronise entre elles.
    let (extraction_tx, mut extraction_rx) =
        mpsc::channel::<(String, Option<ritornello_proto::CoverRef>)>(4);

    // Après le câblage : demander son catalogue à chaque source, **sans
    // attendre**.
    //
    // Une tâche détachée par source, et aucune n'est jointe. La réponse
    // corrélée à `ListPresets` est un `Noop` : elle n'apprend rien au cœur, les
    // présélections arrivant par `source_update_rx` comme `preset_count`.
    // Attendre ces réponses mettrait donc le délai de 5 s du protocole des
    // sources sur le chemin de démarrage, une fois par source injoignable — et
    // supprimer ces fenêtres-là était tout l'objet du chantier précédent.
    for (nom, client) in &sources {
        let (c, n) = (client.clone(), nom.clone());
        tokio::spawn(async move {
            if let Err(e) = c.request(ritornello_proto::SourceReq::ListPresets).await {
                tracing::debug!("list_presets for {n}: {e}");
            }
        });
    }

    // Cœur. La source active affichée est tenue à jour en direct par la boucle
    // ci-dessous (mise à jour de status_state.active_source après chaque commande).
    let mut core;
    // Le cache de pochettes que `assemble_covers_et_core` construit, sorti du
    // bloc ci-dessous : les relais d'afficheur (plus bas) et le câblage à chaud
    // doivent lire **le même** `Arc` que le cœur et la route HTTP, jamais un
    // second cache — c'est là que le cœur dépose les octets qu'il récupère.
    let app_covers;
    {
        // Asked once, before serving: the answer gates the System tab's two
        // OS buttons, and asking per request would mean spawning `busctl`
        // twice every five seconds.
        let sonde = system::probe_capabilities().await;
        // `covers` ci-dessous n'est qu'un squelette : `assemble_covers_et_core`
        // l'écrase par le seul `Arc<CoverCache>` qu'elle construit, remis
        // identique au `Core` qu'elle renvoie — voir sa doc. Construire
        // l'`AppState` en un seul littéral ici, plutôt qu'en deux morceaux,
        // évite de dupliquer sa quinzaine de champs sans rapport avec les
        // pochettes.
        let app_state_squelette = AppState {
            status: status_state.clone(),
            logs: log_buffer.clone(),
            audio_current: audio_current.clone(),
            audio_tx: audio_tx.clone(),
            catalog: catalog.clone(),
            locale_current: locale_current.clone(),
            locale_tx: locale_tx.clone(),
            locales_root: locales_root.clone(),
            admin_backends: admin_backends.clone(),
            admin_assets: admin_assets.clone(),
            cmd_tx: cmd_tx.clone(),
            theme_current: theme_current.clone(),
            theme_tx: theme_tx.clone(),
            settings_current: settings_current.clone(),
            settings_tx: settings_tx.clone(),
            player: etat_rx.clone(),
            catalogue: catalogue_rx.clone(),
            system: Arc::new(system::SystemInfo {
                can_power_off: sonde.can_power_off,
                can_reboot: sonde.can_reboot,
                logind_reachable: sonde.logind_reachable,
                // Le crochet de relance tue mpv **avant** de sortir. Sans
                // cela, mpv survivait au cœur et continuait de jouer : il est
                // lancé en `kill_on_drop(true)`, mais `std::process::exit` ne
                // déroule pas la pile et n'exécute donc aucun `Drop` — la
                // garantie annoncée par `kill_on_drop` ne valait rien sur ce
                // chemin.
                //
                // Le service ne le montrait pas : quand le processus principal
                // d'une unité sort, systemd tue le reste du groupe de contrôle
                // avant de relancer. C'est en développement, sans superviseur,
                // que l'orphelin restait — à jouer, et à tenir le périphérique
                // audio que le cœur relancé voulait reprendre.
                //
                // La mort de mpv fait aussi sortir la boucle principale (voir
                // `mpv_child.wait()` plus bas) : les deux chemins courent,
                // mais ils mènent au même endroit, et c'est l'`exit(0)`
                // ci-dessous qui gagne en pratique. Le détail du signal et sa
                // justification vivent dans `system::terminate_process`, où un
                // test les épingle sur un vrai processus.
                restart: {
                    let pid = mpv_child.id();
                    Arc::new(move || {
                        system::terminate_process(pid);
                        std::process::exit(0)
                    })
                },
                ..Default::default()
            }),
            covers: Arc::new(cover::CoverCache::default()),
            greffons: Arc::new(status::GreffonsControle {
                manifeste: plugins_path.clone(),
                noms: ordre_manifeste.clone(),
                tx: greffon_tx,
            }),
        };
        let (app_state, coeur) = assemble_covers_et_core(
            mpv_player,
            core::Cablage {
                sources,
                persisted,
                state_path,
                catalog: catalog.clone(),
                locales_root: locales_root.clone(),
                metadata: MetadataCablage {
                    plugins: metadata_plugins,
                    now_playing: now_playing_tx,
                    etat: etat_tx,
                },
                catalogue: catalogue_tx,
            },
            pochette_tx,
            extraction_tx,
            app_state_squelette,
        );
        core = coeur;
        app_covers = app_state.covers.clone();
        let app = status::router(app_state);
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("web interface on http://{http_addr}/");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("status server: {e}");
            }
        });
    }
    // Best-effort, like the wake via `Power` (see the comment below): startup
    // must never put systemd in a restart loop. `demarrage` reads
    // `settings.startup_power`; its standby branch skips the source wake but
    // still configures mpv, so the first `Power` starts right.
    if let Err(e) = core.demarrage().await {
        tracing::warn!("startup wake: {e}");
    }

    // Relais de l'état vers chaque afficheur connecté : le même canal qui
    // alimente la route SSE de la SPA, chaque plugin composant lui-même sa
    // mise en page depuis la trame reçue.
    //
    // **Une tâche par afficheur**, et non une tâche qui boucle sur N clients :
    // c'est ce qui empêche un afficheur lent — console occupée, écran bloqué
    // en I/O — de retarder les autres. La contre-pression reste cloisonnée par
    // socket, ce qui était l'argument retenu pour ne pas fusionner les sockets
    // des genres.
    //
    // **Après `Core::new`**, et c'est voulu : c'est lui qui publie le premier
    // catalogue. Spawnés avant, les relais envoyaient à chaque afficheur un
    // `Catalogue` vide suivi du vrai — sans conséquence pour un afficheur qui
    // dessine, mais un client MPD connecté dans cette fenêtre lisait un
    // `listplaylists` vide et pouvait le mettre en cache. L'ordre supprime la
    // fenêtre au lieu de la rattraper en aval.
    //
    // Avant, cette variable était un `Option` : déclarer deux afficheurs ne
    // produisait aucune erreur, mais le cœur ne gardait que le client du
    // dernier déclaré et le premier attendait des lignes qui n'arrivaient
    // jamais.
    if display_clients.is_empty() {
        tracing::warn!("no display plugin connected, continuing without display");
    }
    for (nom, display_client, veut_pochettes) in display_clients {
        relais_afficheur(
            nom,
            display_client,
            veut_pochettes,
            app_covers.clone(),
            etat_rx.clone(),
            catalogue_rx.clone(),
            AvisInjoignable { cablage: 0, tx: injoignable_tx.clone() },
        );
    }

    // Tout ce qu'il faut pour câbler un greffon qui parlera plus tard : les
    // mêmes fils que la boucle de câblage de démarrage, tenus au-delà d'elle.
    let fils_chaud = FilsChaud {
        sockets_dir: sockets_dir.clone(),
        ordre_manifeste,
        source_update_tx: source_update_tx.clone(),
        cmd_tx: cmd_tx.clone(),
        enrich_tx: enrich_tx.clone(),
        now_playing_rx: now_playing_rx.clone(),
        etat_rx: etat_rx.clone(),
        catalogue_rx: catalogue_rx.clone(),
        covers: app_covers.clone(),
        status_state: status_state.clone(),
        admin_backends: admin_backends.clone(),
        admin_assets: admin_assets.clone(),
        injoignable_tx: injoignable_tx.clone(),
    };

    let mut retry_at: Option<tokio::time::Instant> = None;
    // Échéance du prochain rafraîchissement de position. Absolue, comme
    // `retry_at` : voir la raison au point d'armement, dans la boucle.
    let mut prochain_tick: Option<tokio::time::Instant> = None;

    loop {
        let retry_sleep = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // Échéance du plus proche « fin de bénéfice du doute ». Le minimum, et
        // recalculé à chaque tour comme les trois autres : plusieurs greffons
        // démarrent ensemble au lancement du service, et c'est le premier
        // arrivé à échéance qui doit réveiller la boucle.
        let demarrage_at = demarrages.values().copied().min();
        let demarrage_sleep = async {
            match demarrage_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // Echeance de l'overlay volume/muet, lue dans une variable locale
        // avant le `select!` (comme `retry_at`) pour ne pas garder d'emprunt
        // sur `core` pendant l'attente.
        let overlay_at = core.overlay_deadline().map(tokio::time::Instant::from);
        let overlay_sleep = async {
            match overlay_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // Tick de position : une seconde, armé seulement quand il y a une
        // position à publier (voir `Core::tick_position`).
        //
        // L'échéance est **absolue**, comme `retry_at` et `overlay_at`, et
        // c'est un défaut trouvé en relecture qui l'impose. Les trois futurs
        // d'attente sont recréés à chaque tour de boucle, donc chaque fois
        // qu'un bras quelconque se résout — une commande, un événement mpv,
        // un enrichissement, un changement de réglage. Recréer un
        // `sleep_until(at)` sur la même échéance ne change rien ; recréer un
        // `sleep(1 s)` relatif relance le compte à rebours depuis zéro. Le
        // tick n'aurait donc pas lieu une fois par seconde mais une seconde
        // après le dernier réveil du `select!`, et sur un appareil où les
        // événements se succèdent plus vite que cela, il serait repoussé
        // indéfiniment — la position ne bougerait jamais, précisément quand
        // il se passe quelque chose. Le calcul est extrait dans la fonction
        // pure `core::prochaine_echeance`, testée : cette boucle `select!`
        // elle-même n'a aucun filet.
        prochain_tick = core::prochaine_echeance(
            core.tick_position(),
            prochain_tick.map(tokio::time::Instant::into_std),
            tokio::time::Instant::now().into_std(),
        )
        .map(tokio::time::Instant::from);
        // Copie locale (`Instant` est `Copy`) : le futur ci-dessous n'emprunte
        // donc ni `core` ni la variable réassignée dans le bras.
        let position_at = prochain_tick;
        let position_sleep = async {
            match position_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(msg) = cmd_rx.recv() => {
                if let Err(e) = core.handle_input(msg).await {
                    tracing::warn!("command: {e}");
                }
                status_state.write().await.active_source = core.active_source().to_string();
            }
            // Canal fermé (pompe mpv morte) : le motif `Some(..)` cesse de
            // matcher et `tokio::select!` désactive le bras — le bras
            // `mpv_child.wait()` prendra le relais pour sortir proprement.
            Some(ev) = ev_rx.recv() => {
                // C'est le cœur qui qualifie l'événement (voir `EventOutcome`) :
                // la liste des variantes qui attestent la vivacité du flux
                // n'existe qu'à un seul endroit.
                match core.handle_event(ev).await {
                    core::EventOutcome::StreamAlive => retry_at = None,
                    core::EventOutcome::RetryIn(delay) => {
                        retry_at = Some(tokio::time::Instant::now() + delay);
                    }
                    core::EventOutcome::Nothing => {}
                }
            }
            // Annonce arrivée **après** le rendez-vous : greffon lent au
            // démarrage, ou relancé à la main. Le câblage est le même, genre
            // par genre, et une ré-annonce est traitée comme une annonce
            // tardive — on recâble.
            Some(annonce) = tardives_rx.recv() => {
                // Un numéro neuf **avant** de câbler : les sockets de
                // l'incarnation précédente, s'il en reste, deviennent périmés
                // à cet instant précis, et leur fermeture sera ignorée.
                let cablage = cablages.entry(annonce.name.clone()).or_insert(0);
                *cablage += 1;
                let cablage = *cablage;
                cabler_a_chaud(
                    annonce,
                    &fils_chaud,
                    &mut core,
                    &mut rassemble,
                    &kill_triggers,
                    &mut non_supervises,
                    cablage,
                )
                .await;
                status_state.write().await.active_source = core.active_source().to_string();
            }
            // Un socket de greffon s'est fermé. **C'est ce qui rend visible la
            // mort d'un greffon non supervisé**, laquelle ne produisait jusqu'ici
            // qu'une ligne de journal : la page continuait de l'afficher
            // connecté, indéfiniment.
            //
            // Ce que la fermeture prouve exactement : le pair a fermé. Soit son
            // processus est mort, soit il a fermé son socket. Dans les deux cas
            // il n'est plus joignable, donc « déconnecté » est honnête — mais ce
            // n'est pas une preuve stricte de décès, et rien ici n'en déduit un
            // code de sortie.
            Some((nom, cablage)) = injoignable_rx.recv() => {
                if cablages.get(&nom).copied().unwrap_or(0) != cablage {
                    // Le socket d'une incarnation précédente, arrivé après le
                    // recâblage de la suivante. Voir `cablages`.
                    tracing::debug!(
                        "plugin {nom} socket from wiring {cablage} closed after it was rewired"
                    );
                } else {
                    tracing::info!("plugin {nom} is no longer reachable, reporting it disconnected");
                    // **Le nom sort de `non_supervises`, et c'est la moitié
                    // utile.** Il n'y était que parce qu'un processus vivant
                    // échappait à la supervision du cœur ; ce processus n'est
                    // plus joignable, donc le greffon redevient gérable — un
                    // allumage depuis l'IHM en lancera un vrai, supervisé, au
                    // lieu d'être refusé par `eteindre_a_chaud`.
                    //
                    // C'est aussi pourquoi ce registre-ci se purge alors que
                    // `demarrages` ne se purge pas : `non_supervises` décrit une
                    // **capacité** du cœur sur un processus, et cette capacité
                    // vient de changer. Aucune ligne de statut ne porte cette
                    // information, donc rien ne pourrait la relire.
                    non_supervises.remove(&nom);
                    // **Décâblée si c'était une Source**, exactement comme le
                    // fait la branche de décès supervisée. Sans cette ligne, ce
                    // chemin serait une demi-action, et une demi-action est pire
                    // que rien ici : la page dirait « non joint » pendant que le
                    // cœur garderait la source câblée sur un socket fermé, donc
                    // toujours offerte au catalogue et à la télécommande. Les
                    // deux chemins de décès doivent produire le **même** état,
                    // sous peine que le comportement dépende de qui a lancé le
                    // processus.
                    //
                    // `oublie_source_morte` et non `remove_source` : la
                    // distinction est écrite dans sa doc, et elle vaut ici pour
                    // la même raison — personne n'a demandé cette extinction,
                    // donc rien ne bascule vers une autre source. La musique
                    // continue, `active_source` garde son nom, et c'est la
                    // conjonction « source active X, greffon X non joint » qui
                    // porte le diagnostic.
                    if !core.oublie_source_morte(&nom) {
                        tracing::debug!("plugin {nom} was not a wired source, nothing to unwire");
                    }
                    // Un seul verrou pour les deux écritures, comme
                    // `eteindre_a_chaud` : la ligne « déconnecté » et le nom de
                    // la source active décrivent le même instant.
                    let mut statuts = status_state.write().await;
                    // Idempotent, et il faut qu'il le soit : pour un greffon
                    // *supervisé*, le bras `plugin_waits` marquera aussi, dans
                    // un ordre que rien ne fixe. `mark_plugin_disconnected` ne
                    // fait que poser des booléens, et `remove` sur une clé
                    // absente est un non-événement — vérifié, pas supposé.
                    crate::status::mark_plugin_disconnected(&mut statuts, &nom);
                    statuts.active_source = core.active_source().to_string();
                    // Après le verrou des statuts, et non avant : `oublie_page`
                    // prend deux autres verrous, et les imbriquer ferait dépendre
                    // la sûreté d'un ordre à ne jamais inverser ailleurs.
                    drop(statuts);
                    admin::oublie_page(&admin_backends, &admin_assets, &nom).await;
                }
            }
            Some((name, update)) = source_update_rx.recv() => {
                core.handle_source_update(&name, update);
            }
            Some((plugin, enrichment)) = enrich_rx.recv() => {
                core.handle_enrichment(&plugin, enrichment);
            }
            // Une récupération détachée par `Core::lance_pochette` s'est
            // terminée, avec ou sans succès : `pochette_arrivee` libère le
            // marqueur en vol dans tous les cas, et ne publie l'URL locale
            // que sur succès et si elle décrit encore ce qui joue.
            Some((cle, succes)) = pochette_rx.recv() => {
                core.pochette_arrivee(cle, succes).await;
            }
            // Une extraction détachée par `Core::handle_path` s'est terminée
            // (résultat borné par `Sante::borne`, voir `sante.rs`) :
            // `extraction_arrivee` libère le marqueur en vol dans tous les
            // cas, et ne retient le résultat que s'il décrit encore ce que
            // mpv est en train de jouer.
            Some((chemin, r)) = extraction_rx.recv() => {
                core.extraction_arrivee(chemin, r).await;
            }
            Some(device) = audio_rx.recv() => {
                if let Err(e) = core.set_audio_device(device).await {
                    tracing::warn!("audio output change: {e}");
                }
            }
            Some(locale) = locale_rx.recv() => {
                if let Err(e) = core.set_locale(locale).await {
                    tracing::warn!("locale change: {e}");
                }
            }
            Some(t) = theme_rx.recv() => {
                core.set_theme(t);
            }
            Some(s) = settings_rx.recv() => {
                core.set_settings(s);
            }
            Some(ordre) = greffon_rx.recv() => {
                let ok = if ordre.actif {
                    // Un allumage redondant (double clic, page restée ouverte)
                    // doit être un non-événement, pas un second processus
                    // volant le préfixe de sockets du premier : le cœur ne
                    // peut pas compter sur l'appelant pour ne jamais renvoyer
                    // un ordre déjà en vigueur.
                    //
                    // Le prédicat était `kill_triggers.contains_key`, donc faux
                    // précisément dans le cas que cette garde existe pour
                    // couvrir. Voir `Vivacite`, qui écrit pourquoi il fallait
                    // croiser deux registres.
                    match vivacite(&ordre.nom, &kill_triggers, &non_supervises) {
                        // Lancé par le cœur : l'ordre est déjà en vigueur, et
                        // l'accusé décrit un état vrai.
                        Vivacite::Supervise => true,
                        // Un processus tourne pour ce nom et le cœur n'a aucune
                        // prise sur lui. En lancer un second lui volerait son
                        // préfixe de sockets — bruyant sur le greffon MPD, qui
                        // échoue à lier son port et meurt, mais silencieux
                        // partout ailleurs. Refuser, et nommer le remède.
                        Vivacite::HorsAtteinte => {
                            tracing::warn!(
                                "refusing to enable {}: a process for it is already running outside the core's control — kill it yourself, or restart the core to let it take ownership again",
                                ordre.nom
                            );
                            false
                        }
                        Vivacite::Eteint => {
                            let generation = generations.entry(ordre.nom.clone()).or_insert(0);
                            *generation += 1;
                            let generation = *generation;
                            match execs.get(&ordre.nom) {
                                Some(exec) => {
                                    match rallume(
                                        &ordre.nom,
                                        exec,
                                        generation,
                                        &fils_chaud,
                                        &register_path,
                                        core.locale_courante().as_deref(),
                                        &mut kill_triggers,
                                    )
                                    .await
                                    {
                                        Some(fut) => {
                                            plugin_waits.push(fut);
                                            // Le bénéfice du doute part d'ici,
                                            // et non du lancement du service :
                                            // c'est le rallumage depuis l'IHM
                                            // que ce délai couvre. Le
                                            // rendez-vous de démarrage a sa
                                            // propre échéance et son propre
                                            // rapport (`figes`).
                                            demarrages.insert(
                                                ordre.nom.clone(),
                                                tokio::time::Instant::now() + DELAI_DEMARRAGE,
                                            );
                                            true
                                        }
                                        None => false,
                                    }
                                }
                                // Nom refusé bien avant ici par la couche HTTP :
                                // c'est une garde, pas un cas d'usage.
                                None => false,
                            }
                        }
                    }
                } else {
                    eteindre_a_chaud(
                        &ordre.nom,
                        &fils_chaud,
                        &mut core,
                        &mut rassemble,
                        &mut kill_triggers,
                        &non_supervises,
                    )
                    .await
                };
                // Le demandeur attend : un accusé perdu laisserait sa requête
                // HTTP pendre jusqu'au bout de son propre délai.
                let _ = ordre.ack.send(ok);
            }
            _ = demarrage_sleep => {
                let maintenant = tokio::time::Instant::now();
                let echus: Vec<String> = demarrages
                    .iter()
                    .filter(|(_, at)| **at <= maintenant)
                    .map(|(nom, _)| nom.clone())
                    .collect();
                let mut statuts = fils_chaud.status_state.write().await;
                for nom in echus {
                    // Retirée dans tous les cas : l'entrée a fait son office,
                    // et la laisser ferait croître la table pour la vie du
                    // processus.
                    demarrages.remove(&nom);
                    // Mais la rétrogradation n'a lieu que si la ligne dit
                    // **encore** « démarrage ». Le greffon a pu s'annoncer
                    // entre-temps (sa ligne décrit alors ses genres), mourir
                    // (elle dit « déconnecté »), ou être éteint depuis l'IHM
                    // (elle dit « désactivé ») : dans les trois cas, écraser
                    // serait remplacer une information vraie par une fausse.
                    // Relire l'état plutôt que tenir un registre à purger en
                    // trois endroits — voir le commentaire de `demarrages`.
                    if a_retrograder(&statuts, &nom) {
                        tracing::warn!(
                            "plugin {nom} still silent {}s after launch, reporting it as stalled",
                            DELAI_DEMARRAGE.as_secs()
                        );
                        status::replace_plugin_lines(
                            &mut statuts,
                            &nom,
                            vec![PluginStatus::genre_inconnu(&nom, true)],
                            false,
                        );
                    }
                }
            }
            _ = retry_sleep => {
                retry_at = None;
                if let Err(e) = core.retry_stream().await {
                    tracing::warn!("stream retry: {e}");
                }
            }
            _ = overlay_sleep => {
                core.expire_overlay();
            }
            _ = position_sleep => {
                // Réarmer d'abord, depuis maintenant : la cadence reste d'une
                // seconde quoi qu'il arrive sur les autres bras.
                prochain_tick =
                    Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1));
                // Rafraîchir puis publier : la position ayant changé, la trame
                // franchit la déduplication et part vers la SPA comme vers les
                // afficheurs. L'incrustation éventuellement en cours voyage
                // dans cette même trame, intacte — c'est l'afficheur qui
                // décide de sa place, et le cœur garde la main sur son
                // échéance (bras `overlay_sleep`).
                core.rafraichit_position().await;
                core.publie_etat();
            }
            // `next()` et non `select_next_some()` : `tokio::select!` ne
            // consulte pas `is_terminated`, et re-poller un `FuturesUnordered`
            // épuisé via `select_next_some` panique (`SelectNextSome polled
            // after terminated`) — c'est-à-dire que la mort du **dernier**
            // plugin tuait le cœur à l'itération suivante, l'inverse exact de
            // la dégradation voulue. Avec `next()`, l'épuisement rend `None`,
            // le motif ne matche pas, et le bras est simplement désactivé.
            Some((name, generation, status, voulue)) = plugin_waits.next() => {
                // Mort d'une incarnation périmée : le greffon a été rallumé
                // entre-temps, et les lignes de statut décrivent déjà le
                // nouveau processus. Marquer « déconnecté » ici les
                // effacerait au profit d'une mort qui n'a plus cours.
                if generations.get(&name).copied() != Some(generation) {
                    tracing::debug!("plugin {name} generation {generation} exited after being replaced");
                } else {
                    // Une entrée vit exactement le temps d'un processus lancé
                    // et non encore moissonné : celui-ci vient de l'être. La
                    // retirer ici, et seulement ici (jamais dans la branche
                    // périmée ci-dessus), c'est ce qui permet à un allumage
                    // ultérieur de distinguer un greffon vivant d'un greffon
                    // mort — `eteindre_a_chaud` l'a déjà retirée quand
                    // `voulue` est vrai, donc ce retrait est alors un second
                    // passage sans effet.
                    kill_triggers.remove(&name);
                    if voulue {
                        tracing::info!("plugin {name} stopped: disabled from the admin UI");
                    } else {
                        tracing::warn!("plugin {name} exited: {status:?}");
                        // Décâblage de la source, mais **pas** avec la fonction
                        // du chemin volontaire, et c'est le cœur de la décision.
                        //
                        // Ce qu'il faut oublier est le même dans les deux cas :
                        // sans éviction, un greffon mort laissait son nom dans
                        // `source_order` et ses présélections dans
                        // `presets_par_source`, si bien qu'un client MPD gardait
                        // une liste enregistrée pour une source qui n'existe plus
                        // et qu'un `load` dessus **passait** le garde de
                        // `Command::SelectSource` (qui ne consulte que
                        // `source_order`).
                        //
                        // Ce qui diffère est la conséquence sur ce qui joue.
                        // `remove_source` bascule vers la source suivante quand
                        // c'était l'active : c'est juste quand **l'opérateur** a
                        // demandé l'extinction, la bascule étant la suite de son
                        // geste. Ici personne n'a rien demandé. Un greffon de
                        // Source est un *contrôleur* — le flux est tenu par mpv,
                        // enfant du cœur, que sa mort ne touche pas —, donc
                        // basculer transformait la panne d'un contrôleur en
                        // silence, puis affichait « cd » sur un appareil dont
                        // l'utilisateur avait choisi la radio. La musique
                        // continue, `active_source` garde son nom, et la page de
                        // statut porte le diagnostic complet : source active,
                        // greffon non joint. Voir la doc d'`oublie_source_morte`,
                        // qui écrit la comparaison des deux chemins.
                        if !core.oublie_source_morte(&name) {
                            tracing::debug!("plugin {name} was not a wired source, nothing to unwire");
                        }
                        // Un seul verrou pour les deux écritures, comme
                        // `eteindre_a_chaud` : la ligne « déconnecté » et le nom
                        // de la source active décrivent le même instant.
                        //
                        // Réaffirmé même si ce chemin ne change plus
                        // `active_source` : c'est la page de statut qui doit
                        // montrer les deux faits **ensemble** — la source active
                        // est « radio » et le greffon « radio » n'est plus joint.
                        // C'est cette conjonction qui est le diagnostic, et la
                        // relire du cœur plutôt que de supposer qu'elle n'a pas
                        // bougé garde la ligne juste si la décision de basculer
                        // était un jour reprise.
                        let mut statuts = status_state.write().await;
                        crate::status::mark_plugin_disconnected(&mut statuts, &name);
                        statuts.active_source = core.active_source().to_string();
                        // Même geste que sur le chemin voisin : les deux morts
                        // doivent laisser le même état, sous peine que le
                        // comportement dépende de qui a lancé le processus.
                        drop(statuts);
                        admin::oublie_page(&admin_backends, &admin_assets, &name).await;
                    }
                }
            }
            status = mpv_child.wait() => {
                anyhow::bail!("mpv exited ({status:?}), stopping for restart by systemd");
            }
        }
    }
}

#[cfg(test)]
mod injoignable_tests {
    //! Ce que le cœur signale quand un socket d'afficheur se ferme — et ce
    //! qu'il ne signale pas.
    //!
    //! **Aucune marge de temps ici non plus, et pas même un plafond.** Les deux
    //! sens se prouvent par la fermeture des émetteurs : le relais détient le
    //! *seul* émetteur du canal, donc `recv()` rend `Some` s'il signale et
    //! `None` dès qu'il se termine sans le faire. L'attente est exacte dans les
    //! deux cas, et un relais qui se tromperait de sens ne ferait pas expirer le
    //! test — il le ferait échouer sur la valeur.

    use super::*;
    use ritornello_plugin_sdk::{bind_display, serve_display, DisplayPlugin};

    /// Un afficheur qui lit et jette : ce module n'éprouve pas ce qui est reçu,
    /// seulement qui est averti de la fermeture.
    struct Muet;

    #[async_trait::async_trait]
    impl DisplayPlugin for Muet {
        async fn show(&mut self, _etat: PlayerState) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn un_afficheur_qui_ne_repond_plus_est_signale_avec_sa_generation_de_cablage() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let client = DisplayClient::connect(&socket).await.unwrap();
        // Le pair accepte puis **disparaît**. C'est la mort qu'un greffon
        // relancé à la main produit, et que le cœur ne voyait que passer dans
        // son journal.
        let (flux, _) = listener.accept().await.unwrap();
        drop(flux);
        drop(listener);

        let (_etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let (_cat_tx, catalogue_rx) = watch::channel(Catalogue::default());
        let (tx, mut rx) = mpsc::channel(4);
        // Les deux `watch` restent vivants : sans cela le relais pourrait sortir
        // par le chemin « le cœur s'arrête », et le test confondrait les deux
        // sens qu'il existe précisément pour séparer.
        relais_afficheur(
            "mort".into(),
            client,
            false,
            Arc::new(cover::CoverCache::default()),
            etat_rx,
            catalogue_rx,
            AvisInjoignable { cablage: 7, tx },
        );

        // Le relais écrit l'état d'emblée, avant sa boucle : cette écriture-là
        // suffit, le pair ayant fermé. `None` ici voudrait dire qu'il s'est
        // terminé sans rien dire — le défaut même que ce chemin corrige.
        assert_eq!(
            rx.recv().await,
            Some(("mort".to_string(), 7)),
            "la fermeture du socket doit etre signalee, avec le numero de cablage recu"
        );
    }

    #[tokio::test]
    async fn larret_du_coeur_ne_signale_aucun_greffon_injoignable() {
        // Le constat que ce test existe pour empêcher : marquer déconnectés tous
        // les afficheurs pendant l'extinction du cœur, c'est-à-dire peindre une
        // panne sur un arrêt normal. Les deux sorties de boucle du relais se
        // ressemblent — l'une est un envoi en échec, l'autre un `watch` fermé —
        // et rien d'autre ne les distingue.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        tokio::spawn(async move {
            let _ = serve_display(listener, Muet).await;
        });
        let client = DisplayClient::connect(&socket).await.unwrap();

        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let (cat_tx, catalogue_rx) = watch::channel(Catalogue::default());
        let (tx, mut rx) = mpsc::channel(4);
        relais_afficheur(
            "vivant".into(),
            client,
            false,
            Arc::new(cover::CoverCache::default()),
            etat_rx,
            catalogue_rx,
            AvisInjoignable { cablage: 3, tx },
        );

        // Le cœur s'arrête : ses émetteurs tombent. Le pair, lui, est toujours
        // là et lit.
        drop(etat_tx);
        drop(cat_tx);

        // Le relais détient le seul émetteur restant du canal. `None` prouve
        // donc qu'il s'est terminé **sans** signaler, et l'attente est exacte :
        // elle se résout à la seconde où sa tâche se termine, sans qu'aucune
        // durée soit supposée nulle part.
        assert_eq!(
            rx.recv().await,
            None,
            "l'arret du coeur n'est pas la mort d'un greffon : rien ne doit etre signale"
        );
    }
}

#[cfg(test)]
mod bascule_tests {
    //! La bascule allumer/éteindre, et ce que le cœur sait de la vie d'un
    //! greffon.
    //!
    //! Ce qui est éprouvé ici : la classification (`vivacite`), et sur le
    //! **vrai** chemin le refus d'`eteindre_a_chaud` quand le processus est
    //! hors d'atteinte. Ce refus doit arriver **avant** toute mutation, et
    //! c'est tout le constat : décâbler puis rendre `false` laisserait un
    //! greffon vivant, décâblé, et affiché « inactif » — le pire des trois
    //! états. Le contrôle positif juste à côté est ce qui donne son mordant
    //! au test du refus : sans le retour anticipé, il échoue.
    //!
    //! Ce qui **ne l'est pas**, et autant l'écrire : la garde d'*allumage* vit
    //! dans le `select!` de `main`, hors d'atteinte d'un test. Elle consulte la
    //! même fonction, mais son câblage n'est vérifié que par lecture.

    use super::*;
    use crate::core::{Cablage, MetadataCablage};
    use crate::cover::CoverCache;
    use ritornello_proto::{Announcement, PluginKind};

    /// Un `Player` qui ne fait rien : aucun test d'ici ne regarde le lecteur.
    /// `eteindre_a_chaud` ne le touche que par `remove_source`, et la carte des
    /// sources est vide — ce qui est délibéré, faute de quoi il faudrait aussi
    /// un bouchon de `Source` pour un chemin que ces tests ne visitent pas.
    struct LecteurMuet;

    #[async_trait::async_trait]
    impl crate::player::Player for LecteurMuet {
        async fn play(&self, _uri: &str) -> Result<()> {
            Ok(())
        }
        async fn load_list(&self, _uri: &str) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<()> {
            Ok(())
        }
        async fn toggle_pause(&self) -> Result<()> {
            Ok(())
        }
        async fn next(&self) -> Result<()> {
            Ok(())
        }
        async fn prev(&self) -> Result<()> {
            Ok(())
        }
        async fn set_playlist_pos(&self, _n: i64) -> Result<()> {
            Ok(())
        }
        async fn set_volume(&self, _volume: u8) -> Result<()> {
            Ok(())
        }
        async fn set_mute(&self, _mute: bool) -> Result<()> {
            Ok(())
        }
        async fn set_audio_device(&self, _device: &str) -> Result<()> {
            Ok(())
        }
        async fn progression(&self) -> Result<crate::player::Progression> {
            Ok(crate::player::Progression::default())
        }
        async fn seek_relative(&self, _delta_s: i64) -> Result<()> {
            Ok(())
        }
        async fn seek_absolute(&self, _position_s: u32) -> Result<()> {
            Ok(())
        }
    }

    struct Banc {
        fils: FilsChaud,
        core: core::Core<LecteurMuet>,
        rassemble: register::Gathered,
        kill_triggers: HashMap<String, tokio::sync::oneshot::Sender<()>>,
        non_supervises: HashSet<String>,
        /// Tenu jusqu'à la fin du test : `state_path` et `locales_root` en
        /// dépendent.
        _dir: tempfile::TempDir,
    }

    /// Un greffon `mpd` **annoncé et câblé**, dans l'état où le laisse
    /// `cabler_a_chaud` : présent dans `announcements`, une ligne de statut
    /// connectée, et le manifeste qui reconnaît son nom.
    ///
    /// C'est cet état-là qui rend le test réaliste. Un `Gathered::default()`
    /// prouverait un refus sur une forme que le producteur n'émet pas.
    fn banc() -> Banc {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let (now_playing_tx, now_playing_rx) = watch::channel(NowPlaying::default());
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let (catalogue_tx, catalogue_rx) = watch::channel(Catalogue::default());

        let covers = Arc::new(CoverCache::new());
        let catalog = Arc::new(RwLock::new(ritornello_i18n::Catalog::load(
            "core",
            "en",
            &root,
            crate::i18n::EN,
        )));

        let core = core::Core::new(
            LecteurMuet,
            Cablage {
                sources: HashMap::new(),
                persisted: Default::default(),
                state_path: root.join("state.json"),
                catalog,
                locales_root: root.clone(),
                catalogue: catalogue_tx,
                metadata: MetadataCablage {
                    plugins: vec![],
                    now_playing: now_playing_tx,
                    etat: etat_tx,
                },
            },
            covers.clone(),
            mpsc::channel(4).0,
            mpsc::channel(4).0,
        );

        let fils = FilsChaud {
            sockets_dir: root.clone(),
            ordre_manifeste: vec!["mpd".to_string()],
            source_update_tx: mpsc::channel(4).0,
            cmd_tx: mpsc::channel(4).0,
            enrich_tx: mpsc::channel(4).0,
            injoignable_tx: mpsc::channel(4).0,
            now_playing_rx,
            etat_rx,
            catalogue_rx,
            covers,
            status_state: Arc::new(RwLock::new(StatusState {
                // Le constructeur et non un littéral : un champ ajouté à
                // `PluginStatus` ne doit pas casser ce banc, dont le sujet
                // n'est pas la forme de la ligne de statut.
                plugins: vec![PluginStatus::genre("mpd", "display", true, true)],
                active_source: String::new(),
            })),
            admin_backends: Arc::new(RwLock::new(HashMap::new())),
            admin_assets: Arc::new(Default::default()),
        };

        let mut rassemble = register::Gathered::default();
        rassemble.announcements.insert(
            "mpd".to_string(),
            Announcement {
                name: "mpd".into(),
                kinds: vec![PluginKind::Display],
                admin: true,
                covers: false,
            },
        );

        Banc {
            fils,
            core,
            rassemble,
            kill_triggers: HashMap::new(),
            non_supervises: HashSet::new(),
            _dir: dir,
        }
    }

    async fn ligne(b: &Banc) -> PluginStatus {
        let statuts = b.fils.status_state.read().await;
        statuts.plugins.iter().find(|l| l.name == "mpd").cloned().expect("ligne mpd")
    }

    async fn eteindre(b: &mut Banc) -> bool {
        eteindre_a_chaud(
            "mpd",
            &b.fils,
            &mut b.core,
            &mut b.rassemble,
            &mut b.kill_triggers,
            &b.non_supervises,
        )
        .await
    }

    fn statuts_de(lignes: Vec<PluginStatus>) -> StatusState {
        StatusState { plugins: lignes, active_source: String::new() }
    }

    /// Le mot juste au bon moment : « démarrage » constate, « figé » accuse.
    /// Les deux disent que le greffon n'a pas parlé, et les échanger ferait
    /// accuser un binaire parfaitement sain — le défaut signalé à l'usage.
    #[test]
    fn la_ligne_de_demarrage_nest_pas_la_ligne_de_fige() {
        let d = PluginStatus::demarrage("mpd");
        assert!(d.starting, "elle doit dire « démarrage »");
        assert!(!d.stalled, "et surtout pas « figé » en même temps");
        assert!(!d.connected);
        assert!(!d.disabled);

        let f = PluginStatus::genre_inconnu("mpd", true);
        assert!(f.stalled);
        assert!(!f.starting, "les deux etats sont exclusifs");
    }

    /// La propriété qui compte de l'échéance de démarrage : elle ne remplace
    /// **jamais** une information vraie par une accusation.
    #[test]
    fn lecheance_ne_retrograde_que_ce_qui_demarre_encore() {
        assert!(
            a_retrograder(&statuts_de(vec![PluginStatus::demarrage("mpd")]), "mpd"),
            "un greffon toujours muet a l'echeance doit passer en « fige »"
        );
        assert!(
            !a_retrograder(&statuts_de(vec![PluginStatus::genre("mpd", "display", true, true)]), "mpd"),
            "il s'est annonce entre-temps : sa ligne decrit ses genres, ne pas l'ecraser"
        );
        assert!(
            !a_retrograder(&statuts_de(vec![PluginStatus::genre("mpd", "display", false, true)]), "mpd"),
            "annonce puis mort : la ligne dit « deconnecte », plus vrai que « fige »"
        );
        assert!(
            !a_retrograder(&statuts_de(vec![PluginStatus::desactive("mpd")]), "mpd"),
            "eteint depuis l'IHM pendant son demarrage : « desactive » doit tenir"
        );
        assert!(
            !a_retrograder(&statuts_de(vec![PluginStatus::genre_inconnu("mpd", true)]), "mpd"),
            "deja fige : rien a faire, et surtout pas une seconde ligne de journal"
        );
        assert!(
            !a_retrograder(&statuts_de(vec![]), "mpd"),
            "plus aucune ligne pour ce nom : rien a retrograder"
        );
    }

    #[test]
    fn un_greffon_lance_par_le_coeur_est_supervise() {
        let mut kt = HashMap::new();
        kt.insert("mpd".to_string(), tokio::sync::oneshot::channel::<()>().0);
        assert_eq!(vivacite("mpd", &kt, &HashSet::new()), Vivacite::Supervise);
    }

    #[test]
    fn un_greffon_annonce_hors_supervision_est_hors_datteinte() {
        let non_supervises: HashSet<String> = ["mpd".to_string()].into_iter().collect();
        assert_eq!(
            vivacite("mpd", &HashMap::new(), &non_supervises),
            Vivacite::HorsAtteinte
        );
    }

    #[test]
    fn un_nom_absent_des_deux_registres_est_eteint() {
        assert_eq!(vivacite("mpd", &HashMap::new(), &HashSet::new()), Vivacite::Eteint);
    }

    /// La conjonction est inatteignable en production (voir la doc de
    /// `vivacite`), mais la fonction est totale : c'est ce que dit ce test, et
    /// il fige l'ordre pour qui ajouterait un registre.
    #[test]
    fn ce_que_le_coeur_peut_arreter_prime_sur_ce_quil_constate() {
        let mut kt = HashMap::new();
        kt.insert("mpd".to_string(), tokio::sync::oneshot::channel::<()>().0);
        let non_supervises: HashSet<String> = ["mpd".to_string()].into_iter().collect();
        assert_eq!(vivacite("mpd", &kt, &non_supervises), Vivacite::Supervise);
    }

    /// Le contrôle positif : le cœur tient le déclencheur, donc il éteint pour
    /// de bon. Sans ce test, celui du refus passerait aussi avec une fonction
    /// qui refuse **toujours**.
    #[tokio::test]
    async fn un_greffon_supervise_seteint_et_lannonce_disparait() {
        let mut b = banc();
        b.kill_triggers.insert("mpd".to_string(), tokio::sync::oneshot::channel::<()>().0);

        assert!(eteindre(&mut b).await, "l'extinction d'un greffon supervisé doit réussir");
        assert!(!b.kill_triggers.contains_key("mpd"), "le déclencheur est consommé");
        assert!(!b.rassemble.announcements.contains_key("mpd"), "l'annonce est retirée");
        assert!(ligne(&b).await.disabled, "la ligne de statut dit « désactivé »");
    }

    /// Le constat lui-même. Un processus vivant que le cœur ne peut pas
    /// arrêter : la bascule doit rendre `false` **et** ne rien avoir touché.
    ///
    /// Les trois assertions d'immobilité ne sont pas décoratives. Un correctif
    /// qui journaliserait puis décâblerait quand même passerait la première et
    /// échouerait sur les suivantes — or c'est précisément la demi-mesure qui
    /// produit l'état le plus trompeur de la page de statut.
    #[tokio::test]
    async fn un_greffon_hors_datteinte_nest_pas_eteint_et_rien_nest_decable() {
        let mut b = banc();
        b.non_supervises.insert("mpd".to_string());

        assert!(
            !eteindre(&mut b).await,
            "annoncer un arrêt qu'on n'a pas obtenu est le défaut à corriger"
        );
        assert!(
            b.rassemble.announcements.contains_key("mpd"),
            "l'annonce reste : le greffon tourne toujours"
        );
        let l = ligne(&b).await;
        assert!(!l.disabled, "la page ne doit pas le montrer désactivé");
        assert!(l.connected, "il est toujours joignable, la ligne le dit");
    }

    /// Éteindre un greffon déjà éteint reste un succès : le demandeur voulait
    /// cet état, il l'obtient. Symétrique du non-événement de l'allumage, et ce
    /// qui distingue `Eteint` de `HorsAtteinte` — sans quoi un double clic sur
    /// « désactiver » remonterait une erreur.
    #[tokio::test]
    async fn eteindre_un_greffon_deja_eteint_reussit() {
        let mut b = banc();
        assert!(eteindre(&mut b).await);
        assert!(ligne(&b).await.disabled);
    }
}

#[cfg(test)]
mod relais_tests {
    //! Le relais d'afficheur, éprouvé sur le vrai chemin : un `DisplayClient`
    //! du SDK d'un côté, `serve_display` de l'autre, et entre les deux
    //! exactement la fonction que `main` appelle.
    //!
    //! Aucune marge de temps nulle part. Le sens positif est prouvé en
    //! **attendant** ce qui doit arriver (un canal, donc l'attente est exacte).
    //! Le sens négatif — « rien n'arrive » — ne peut pas se prouver par une
    //! attente : il l'est par une **trame témoin** envoyée après, sur le même
    //! socket. Les trames y arrivent dans l'ordre et `serve_display` les traite
    //! dans l'ordre, donc voir le témoin prouve que ce qui le précédait a déjà
    //! été traité — ou n'a jamais été envoyé.

    use super::*;
    use crate::cover::{fixtures, CoverCache, Pochette};
    use ritornello_plugin_sdk::{bind_display, serve_display, DisplayPlugin};
    use ritornello_proto::Cover;

    #[derive(Debug, PartialEq)]
    enum Recu {
        Etat(Box<PlayerState>),
        Catalogue(Catalogue),
        Pochette(Cover),
    }

    /// Un afficheur qui traite **tout** : c'est délibéré. Si le sens négatif
    /// était prouvé par un afficheur incapable de recevoir une pochette, il ne
    /// prouverait rien sur le filtre du cœur — seulement sur le bouchon.
    struct Bouchon {
        tx: mpsc::UnboundedSender<Recu>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for Bouchon {
        async fn show(&mut self, state: PlayerState) -> Result<()> {
            let _ = self.tx.send(Recu::Etat(Box::new(state)));
            Ok(())
        }
        async fn catalogue(&mut self, c: Catalogue) -> Result<()> {
            let _ = self.tx.send(Recu::Catalogue(c));
            Ok(())
        }
        fn wants_covers(&self) -> bool {
            true
        }
        async fn cover(&mut self, c: Cover) -> Result<()> {
            let _ = self.tx.send(Recu::Pochette(c));
            Ok(())
        }
    }

    /// En-tête JPEG minimal puis du remplissage.
    /// **Indecodable expres** : ne convient qu'aux tests de taille et de
    /// plafond, ou l'image n'est jamais decodee. Tout ce qui traverse
    /// `CoverCache::ligne` doit passer par `fixtures::jpeg_decodable`, le rendu
    /// etant actif par defaut.
    fn jpeg(remplissage: usize) -> Vec<u8> {
        let mut v = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        v.resize(6 + remplissage, 0x42);
        v
    }

    /// Un état **tel que le cœur l'émet** : `cover_href` y est toujours de la
    /// forme `/api/cover/{clé}`, et la clé désigne une entrée du cache. Un
    /// `Default::default()` avec un `cover_href` inventé prouverait une
    /// causalité dans une trame que le producteur ne peut pas produire.
    fn etat_avec_pochette(cle: &str) -> PlayerState {
        PlayerState {
            source: "files".into(),
            morceau: ritornello_proto::Morceau {
                cover_href: Some(format!("{}{cle}", cover::PREFIXE_HREF)),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Monte un afficheur servi par le SDK, câble le relais dessus, et rend de
    /// quoi piloter l'état et lire ce que l'afficheur reçoit.
    struct Banc {
        etat_tx: watch::Sender<PlayerState>,
        recus: mpsc::UnboundedReceiver<Recu>,
        /// Le dernier état poussé. Un témoin en est dérivé, pour n'en différer
        /// que par un champ sans rapport avec la pochette (voir `temoin`).
        dernier: PlayerState,
        _catalogue_tx: watch::Sender<Catalogue>,
        _dir: tempfile::TempDir,
    }

    async fn banc(
        veut_pochettes: bool,
        covers: Arc<CoverCache>,
        etat_initial: PlayerState,
    ) -> Banc {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let (tx, recus) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = serve_display(listener, Bouchon { tx }).await;
        });
        let client = DisplayClient::connect(&socket).await.unwrap();
        let (etat_tx, etat_rx) = watch::channel(etat_initial.clone());
        let (catalogue_tx, catalogue_rx) = watch::channel(Catalogue::default());
        // Le banc ne dit rien de l'avis d'injoignabilité : récepteur abandonné
        // aussitôt, donc l'envoi de fin de relais échoue et est ignoré, comme
        // il l'est en service quand la boucle du cœur est déjà partie. Même
        // idiome que les canaux du banc de `cabler_a_chaud`.
        relais_afficheur(
            "banc".into(),
            client,
            veut_pochettes,
            covers,
            etat_rx,
            catalogue_rx,
            AvisInjoignable { cablage: 0, tx: mpsc::channel(4).0 },
        );
        let mut b = Banc {
            etat_tx,
            recus,
            dernier: etat_initial,
            _catalogue_tx: catalogue_tx,
            _dir: dir,
        };
        // Attendre que le relais ait consommé la valeur initiale **avant** de
        // rendre le banc. Un `watch` ne garde que la dernière valeur : sans
        // cette attente, un `send` du test pouvait écraser l'état initial avant
        // le `borrow_and_update()` du relais, et l'état porteur de la pochette
        // n'aurait jamais existé pour lui. Ce n'est pas une marge de temps mais
        // une synchronisation exacte — le catalogue est la **seconde** trame
        // qu'envoie le relais, donc l'avoir vue prouve que l'état initial est
        // passé. La pochette initiale, elle, part juste après : elle reste donc
        // à collecter, ce que fait `temoin`.
        loop {
            match b.recus.recv().await.expect("le relais doit envoyer letat puis le catalogue") {
                Recu::Catalogue(_) => break,
                Recu::Etat(_) => {}
                autre => panic!("trame inattendue avant le catalogue : {autre:?}"),
            }
        }
        b
    }

    /// Clôt une collecte : envoie un état témoin et rend tout ce qui est arrivé
    /// avant lui.
    ///
    /// Le témoin ne diffère du dernier état que par le **volume**, donc il porte
    /// le même `cover_href`. C'est nécessaire : un témoin sans pochette
    /// réinitialiserait la garde de déduplication du relais, et la pochette
    /// repartirait à l'état suivant — ce qui masquerait exactement la propriété
    /// que ces tests veulent voir.
    async fn temoin(banc: &mut Banc) -> Vec<Recu> {
        let mut t = banc.dernier.clone();
        t.volume = t.volume.wrapping_add(1);
        banc.dernier = t.clone();
        banc.etat_tx.send(t.clone()).unwrap();
        let mut avant = Vec::new();
        loop {
            match banc.recus.recv().await.expect("le relais doit rester vivant") {
                Recu::Etat(e) if *e == t => return avant,
                autre => avant.push(autre),
            }
        }
    }

    /// Provoque un changement d'état et rend **exactement** ce qu'il a
    /// provoqué.
    ///
    /// Deux synchronisations, et les deux sont nécessaires. La première attend
    /// l'arrivée de *cet* état : un `watch` ne conserve que la dernière valeur,
    /// donc envoyer le témoin avant d'avoir vu celui-ci pourrait l'effacer sans
    /// qu'il ait jamais existé pour le relais. La seconde est le témoin, qui
    /// clôt la collecte.
    ///
    /// **Une trame de pochette peut arriver en retard, et il a fallu un échec
    /// intermittent pour l'admettre.** Le raisonnement d'origine disait « la
    /// fenêtre précédente a été close par son propre témoin, donc rien ne reste
    /// en vol » et faisait paniquer sur toute autre trame. C'est faux : `temoin`
    /// rend la main dès qu'il voit **sa** trame d'état, or le relais enchaîne
    /// ensuite sur son étape pochette pour ce témoin-là. Un témoin dont la
    /// pochette est en attente de réessai déclenche donc une lecture qui vit
    /// encore après son retour — et si le test a entre-temps remis le fichier en
    /// place, ce réessai **réussit** et sa trame se présente juste avant l'état
    /// suivant. Cadrage faux, pas anomalie.
    ///
    /// D'où l'asymétrie assumée ci-dessous : une **pochette** en avance est
    /// versée dans la fenêtre (elle est la conséquence retardée du changement
    /// précédent, et la compter ici est ce que les assertions veulent — le
    /// relais dédoublonne ensuite, donc elle ne peut pas compter deux fois),
    /// tandis qu'un **état** inattendu reste une anomalie et fait paniquer :
    /// l'ordre des états, lui, n'a aucune raison de flotter.
    async fn provoque(banc: &mut Banc, etat: PlayerState) -> Vec<Recu> {
        banc.dernier = etat.clone();
        banc.etat_tx.send(etat.clone()).unwrap();
        let mut avant = Vec::new();
        loop {
            match banc.recus.recv().await.expect("le relais doit rester vivant") {
                Recu::Etat(e) if *e == etat => break,
                pochette @ Recu::Pochette(_) => avant.push(pochette),
                autre => panic!("trame inattendue avant letat envoye : {autre:?}"),
            }
        }
        avant.extend(temoin(banc).await);
        avant
    }

    fn pochettes(recus: &[Recu]) -> Vec<&Cover> {
        recus
            .iter()
            .filter_map(|r| match r {
                Recu::Pochette(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn un_afficheur_qui_na_pas_demande_les_pochettes_nen_recoit_aucune() {
        // **La propriété qui protège la console.** Le bouchon sait recevoir une
        // pochette ; c'est le cœur qui ne doit pas la lui envoyer.
        let covers = Arc::new(CoverCache::new());
        covers
            .insere("abcd".into(), Pochette::Octets(fixtures::jpeg_decodable(48, 48), "image/jpeg"))
            .await;
        let mut b = banc(false, covers, etat_avec_pochette("abcd")).await;

        let recus = temoin(&mut b).await;
        assert!(
            pochettes(&recus).is_empty(),
            "aucune pochette ne doit atteindre un afficheur qui n'en a pas demande : {recus:?}"
        );
    }

    #[tokio::test]
    async fn un_afficheur_qui_a_demande_recoit_les_octets_et_le_href_de_letat() {
        let image = fixtures::jpeg_decodable(48, 48);
        let covers = Arc::new(CoverCache::new());
        covers.insere("abcd".into(), Pochette::Octets(image.clone(), "image/png")).await;
        let mut b = banc(true, covers, etat_avec_pochette("abcd")).await;

        let recus = temoin(&mut b).await;
        let vues = pochettes(&recus);
        assert_eq!(vues.len(), 1, "une pochette, une seule : {recus:?}");
        assert_eq!(vues[0].bytes, image);
        assert_eq!(vues[0].mime, "image/png");
        assert_eq!(
            vues[0].href,
            format!("{}abcd", cover::PREFIXE_HREF),
            "le href doit etre exactement celui de la trame d'etat, sans quoi l'afficheur \
             ne peut pas correler l'image avec ce qui joue"
        );
    }

    #[tokio::test]
    async fn la_pochette_ne_repart_pas_tant_quelle_ne_change_pas() {
        // Une trame d'état sort jusqu'à une fois par seconde en lecture. Sans
        // cette garde, chaque seconde de lecture pousserait l'image entière —
        // et referait la lecture du fichier local qui la produit.
        let covers = Arc::new(CoverCache::new());
        covers
            .insere("abcd".into(), Pochette::Octets(fixtures::jpeg_decodable(48, 48), "image/jpeg"))
            .await;
        covers
            .insere("efgh".into(), Pochette::Octets(fixtures::jpeg_decodable(64, 64), "image/jpeg"))
            .await;
        let mut b = banc(true, covers, etat_avec_pochette("abcd")).await;

        // La pochette initiale, qui part avec le premier état.
        let recus = temoin(&mut b).await;
        assert_eq!(pochettes(&recus).len(), 1, "la pochette initiale : {recus:?}");

        // Le même `cover_href`, mais un état différent (le volume) : la trame
        // d'état repart, la pochette non.
        let mut encore = etat_avec_pochette("abcd");
        encore.volume = 42;
        let recus = provoque(&mut b, encore).await;
        assert!(
            pochettes(&recus).is_empty(),
            "une pochette inchangee ne doit pas repartir avec chaque trame d'etat : {recus:?}"
        );

        // Une autre clé, en revanche, est une autre image : elle doit partir.
        let recus = provoque(&mut b, etat_avec_pochette("efgh")).await;
        let vues = pochettes(&recus);
        assert_eq!(vues.len(), 1, "un changement de pochette doit en pousser une : {recus:?}");
        assert_eq!(vues[0].href, format!("{}efgh", cover::PREFIXE_HREF));
    }

    #[tokio::test]
    async fn une_pochette_au_dela_du_plafond_nest_pas_poussee_et_le_relais_survit() {
        // La conséquence définie du plafond, vue du relais : rien n'est poussé,
        // et surtout la tâche continue de servir l'état — un refus de pochette
        // n'est pas un échec d'envoi, sinon l'afficheur perdrait *tout* pour le
        // reste du processus.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("enorme.jpg");
        std::fs::write(&chemin, jpeg(ritornello_proto::COVER_MAX_BYTES)).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insere("abcd".into(), Pochette::Fichier(chemin)).await;
        let mut b = banc(true, covers, etat_avec_pochette("abcd")).await;

        let recus = temoin(&mut b).await;
        assert!(pochettes(&recus).is_empty(), "au-dela du plafond, rien ne doit partir : {recus:?}");
        // Le témoin est arrivé, donc le relais vit : c'est l'autre moitié de la
        // propriété, et `temoin` aurait bloqué indéfiniment sinon.
    }

    #[tokio::test]
    async fn un_href_sans_pochette_en_cache_ne_casse_pas_le_relais() {
        // Le cache est borné (`ENTREES` entrées) : la clé publiée dans l'état
        // peut avoir été évincée entre-temps.
        let covers = Arc::new(CoverCache::new());
        let mut b = banc(true, covers, etat_avec_pochette("evincee")).await;
        let recus = temoin(&mut b).await;
        assert!(pochettes(&recus).is_empty(), "{recus:?}");
    }

    #[tokio::test]
    async fn un_echec_transitoire_est_reessaye_et_la_pochette_finit_par_partir() {
        // **La propriété que le défaut cassait, et rien d'autre.** `pousse`
        // marquait la tentative comme faite *avant* de la faire : un seul délai
        // dépassé sur un partage SMB endormi sacrifiait la pochette pour **toute
        // la piste**, parce que la garde de déduplication considérait ensuite
        // l'affaire classée. Or c'est exactement le cas où un second essai
        // réussit — un partage réveillé répond au deuxième accès.
        //
        // L'échec est provoqué par la disparition du fichier, ce que
        // `lit_fichier_borne` traite comme toute IO qui n'aboutit pas. La
        // séquence est celle de la production : l'entrée est insérée alors que le
        // fichier existe (`recupere` en a lu l'en-tête avant d'insérer), le
        // partage s'absente, puis il revient.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        let image = fixtures::jpeg_decodable(48, 48);
        std::fs::write(&chemin, &image).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insere("abcd".into(), Pochette::Fichier(chemin.clone())).await;
        // Le partage s'endort : le fichier n'est plus lisible.
        std::fs::remove_file(&chemin).unwrap();

        let mut b = banc(true, covers, etat_avec_pochette("abcd")).await;
        let recus = temoin(&mut b).await;
        assert!(
            pochettes(&recus).is_empty(),
            "premiere tentative : le fichier est illisible, rien ne doit partir : {recus:?}"
        );

        // Le partage revient. Le `cover_href` n'a **pas** changé — c'est tout
        // l'enjeu : avec l'ancien code, la garde le tenait pour deja traite et
        // aucune relecture n'avait plus lieu jusqu'au morceau suivant.
        std::fs::write(&chemin, &image).unwrap();
        let mut encore = etat_avec_pochette("abcd");
        encore.volume = 42;
        let recus = provoque(&mut b, encore).await;
        let vues = pochettes(&recus);
        assert_eq!(vues.len(), 1, "le second essai doit pousser la pochette : {recus:?}");
        assert_eq!(vues[0].bytes, image);
    }

    #[tokio::test]
    async fn un_echec_definitif_nest_pas_reessaye_sans_fin() {
        // L'autre moitié du compromis, et elle compte autant : une trame d'état
        // sort jusqu'à une fois par seconde en lecture, donc retenter sans borne
        // relirait un fichier absent une fois par seconde pour le reste de la
        // piste. Le budget est de `ESSAIS_POCHETTE` essais, et il s'épuise.
        //
        // Preuve sans marge de temps : le fichier est remis en place **après**
        // épuisement du budget, et la pochette ne doit alors plus partir. Si le
        // budget n'existait pas, elle partirait.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        let image = fixtures::jpeg_decodable(48, 48);
        std::fs::write(&chemin, &image).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insere("abcd".into(), Pochette::Fichier(chemin.clone())).await;
        std::fs::remove_file(&chemin).unwrap();

        // Une tentative part avec l'état initial, une par témoin ci-dessous :
        // trois essais au total, soit tout le budget.
        let mut b = banc(true, covers, etat_avec_pochette("abcd")).await;
        for _ in 0..3 {
            let recus = temoin(&mut b).await;
            assert!(pochettes(&recus).is_empty(), "rien ne doit partir tant que le fichier manque");
        }

        std::fs::write(&chemin, &image).unwrap();
        let recus = temoin(&mut b).await;
        assert!(
            pochettes(&recus).is_empty(),
            "le budget de cette pochette est epuise : plus aucune relecture ne doit avoir lieu \
             pour ce href, {recus:?}"
        );
    }

    #[tokio::test]
    async fn un_afficheur_recable_recoit_limage_actuelle_et_non_celle_davant() {
        // **Le scénario du constat le plus grave, de bout en bout.** Trois clics
        // de l'utilisateur : désactiver l'afficheur depuis la page d'admin,
        // remplacer la pochette sur le partage, le réactiver. Le second relais
        // repart avec sa garde de déduplication à zéro et redemande la pochette
        // courante — même clé, puisque la clé hache le *chemin*.
        //
        // Une ligne encodée gardée d'un appel sur l'autre servait alors l'image
        // d'avant, et rien ne pouvait l'invalider : remplacer un fichier sur un
        // partage ne passe par aucun code à nous, et aucun `insere` n'a lieu ici.
        // Deux bancs successifs sur le **même** `CoverCache` reproduisent
        // exactement le décâblage puis le recâblage.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        let avant = fixtures::jpeg_decodable(48, 48);
        let apres = fixtures::jpeg_decodable(64, 64);
        std::fs::write(&chemin, &avant).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insere("abcd".into(), Pochette::Fichier(chemin.clone())).await;

        let mut premier = banc(true, covers.clone(), etat_avec_pochette("abcd")).await;
        let recus = temoin(&mut premier).await;
        let vues = pochettes(&recus);
        assert_eq!(vues.len(), 1, "la pochette initiale : {recus:?}");
        assert_eq!(vues[0].bytes, avant);
        // L'afficheur est desactive : son relais s'en va avec son banc.
        drop(premier);

        // L'utilisateur remplace la pochette qui ne lui plaisait pas.
        std::fs::write(&chemin, &apres).unwrap();

        // Puis il reactive l'afficheur : nouveau relais, meme cache, meme cle.
        let mut second = banc(true, covers, etat_avec_pochette("abcd")).await;
        let recus = temoin(&mut second).await;
        let vues = pochettes(&recus);
        assert_eq!(vues.len(), 1, "l'afficheur recable doit recevoir la pochette courante : {recus:?}");
        assert_eq!(
            vues[0].bytes, apres,
            "et ce doit etre l'image actuelle du partage, pas celle que le cache avait encodee \
             avant le remplacement"
        );
    }
}
