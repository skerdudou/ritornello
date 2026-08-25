mod audio_output;
mod admin;
mod core;
mod metadata;
mod placeholder;
mod player;
mod plugins;
mod register;
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
use ritornello_proto::{Announcement, Catalogue, Enrichment, InputMessage, NowPlaying, PluginKind};
use ritornello_plugin_sdk::{run_input_client, run_metadata_client, DisplayClient, SourceClient, SourceUpdate};
use std::collections::HashMap;
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
fn relais_afficheur(
    nom: String,
    client: Arc<DisplayClient>,
    mut etat_rx: watch::Receiver<PlayerState>,
    mut catalogue_rx: watch::Receiver<Catalogue>,
) {
    tokio::spawn(async move {
        let etat = etat_rx.borrow_and_update().clone();
        let cat = catalogue_rx.borrow_and_update().clone();
        if let Err(e) = client.send(&etat).await {
            tracing::warn!("display plugin {nom} relay stopped: {e}");
            return;
        }
        if let Err(e) = client.send_catalogue(&cat).await {
            tracing::warn!("display plugin {nom} relay stopped: {e}");
            return;
        }
        loop {
            let envoi = tokio::select! {
                r = etat_rx.changed() => match r {
                    Ok(()) => {
                        let e = etat_rx.borrow_and_update().clone();
                        client.send(&e).await
                    }
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
                break;
            }
        }
    });
}

/// Les fils que le câblage à chaud doit tenir pour rejouer, après le
/// démarrage, ce que la boucle de câblage initiale fait avec ses variables
/// locales.
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
    status_state: Arc<RwLock<StatusState>>,
    admin_backends: admin::AdminBackends,
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
    if rassemble.morts.contains(&nom) {
        // Sa `child.wait()` a été consommée par le rendez-vous : `plugin_waits`
        // ne la reverra jamais, donc ni son prochain code de sortie ni son
        // `mark_plugin_disconnected`. Le `connected: true` qu'on va poser sera
        // vrai à l'instant où on le pose, et ne se démentira plus jamais tout
        // seul. Le dire, plutôt que de le laisser mentir en silence.
        tracing::warn!(
            "rewiring a plugin the core no longer supervises: {nom} was seen exiting, its next exit will go unnoticed"
        );
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
                match SourceClient::connect(&socket, nom.clone(), fils.source_update_tx.clone())
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
                        fils.etat_rx.clone(),
                        fils.catalogue_rx.clone(),
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
                tokio::spawn(async move {
                    if let Err(e) = run_input_client(&socket_for_task, tx).await {
                        tracing::warn!("input plugin {name} disconnected: {e}");
                    }
                });
                lignes.push(PluginStatus::genre(&nom, "input", true, annonce.admin));
            }
            PluginKind::Metadata => {
                let tx = fils.enrich_tx.clone();
                let np_rx = fils.now_playing_rx.clone();
                let socket_for_task = socket.clone();
                let name = nom.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        run_metadata_client(&socket_for_task, name.clone(), tx, np_rx).await
                    {
                        tracing::warn!("metadata plugin {name} disconnected: {e}");
                    }
                });
                lignes.push(PluginStatus::genre(&nom, "metadata", true, annonce.admin));
            }
        }
    }

    // L'ancien dorsal est retiré **avant** la tentative de connexion, et quoi
    // qu'annonce le greffon. Un dorsal survivant à une ré-annonce pointerait
    // vers un socket disparu : `/api/admin/<nom>` rendrait une erreur au bout
    // des 5 s du protocole d'admin — sériel, donc en retenant la page — là où un
    // 404 franc dit tout de suite qu'il n'y a rien à cette adresse.
    fils.admin_backends.write().await.remove(&nom);
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
        .init();

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
        core::EN,
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

    let mut plugin_waits = FuturesUnordered::new();
    let mut lances: Vec<String> = Vec::new();
    let mut plugin_statuses = Vec::new();

    for p in &manifest.plugins {
        let prefix = sockets_dir.join(&p.name);
        match plugins::spawn(
            &p.exec,
            &register_path,
            &p.name,
            &prefix,
            persisted.locale.as_deref(),
        ) {
            Ok(child) => {
                let wname = p.name.clone();
                plugin_waits.push(async move {
                    let mut child = child;
                    let status = child.wait().await;
                    (wname, status)
                });
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
        (&mut plugin_waits).map(|(nom, _statut)| nom),
        std::time::Duration::from_secs(10),
        &tardives_tx,
        &mut tardives_rx,
    )
    .await;

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

    // La page d'admin est **annoncée** par le binaire, plus observée par une
    // fenêtre d'attente : le drapeau des statuts part de la ligne
    // d'enregistrement. Mais l'annonce n'est qu'une déclaration de fichier —
    // c'est une capacité **observée** que l'IHM doit voir au final : si la
    // connexion admin échoue plus bas, le drapeau est repassé à `false` sur
    // toutes les lignes de ce nom, quel que soit leur genre.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    // Le nom voyage avec le client : c'est lui qui nomme le greffon dans le
    // journal quand son relais s'arrête.
    let mut display_clients: Vec<(String, Arc<DisplayClient>)> = Vec::new();
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
                    match SourceClient::connect(&socket, nom.clone(), source_update_tx.clone()).await
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
                        display_clients.push((nom.clone(), client));
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
                    tokio::spawn(async move {
                        if let Err(e) = run_input_client(&socket_for_task, tx).await {
                            tracing::warn!("input plugin {name} disconnected: {e}");
                        }
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
                    tokio::spawn(async move {
                        if let Err(e) =
                            run_metadata_client(&socket_for_task, name.clone(), tx, np_rx).await
                        {
                            tracing::warn!("metadata plugin {name} disconnected: {e}");
                        }
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

    // Démarrer **sans aucune source** est légitime depuis l'enregistrement à
    // chaud, et c'était la dernière échéance qui condamnait : refuser de
    // démarrer à t+10 s contredit l'idée qu'une source peut arriver à t+30 s, et
    // supprime la page de statut précisément quand on voudrait y voir le greffon
    // figé. Il n'y aura rien à lire, mais on peut déjà voir ce qui se passe.
    //
    // Reste un seul refus, qui n'est pas une lenteur mais une erreur de
    // configuration : plus **aucun processus vivant** pour s'annoncer. Voir
    // `register::un_greffon_vivant`.
    if !register::un_greffon_vivant(&lances, &rassemble) {
        anyhow::bail!(
            "no plugin process alive (plugins.toml empty, or every plugin failed to launch or exited)"
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
    {
        // Asked once, before serving: the answer gates the System tab's two
        // OS buttons, and asking per request would mean spawning `busctl`
        // twice every five seconds.
        let sonde = system::probe_capabilities().await;
        let app = status::router(AppState {
            status: status_state.clone(),
            logs: log_buffer.clone(),
            audio_current: audio_current.clone(),
            audio_tx: audio_tx.clone(),
            catalog: catalog.clone(),
            locale_current: locale_current.clone(),
            locale_tx: locale_tx.clone(),
            locales_root: locales_root.clone(),
            admin_backends: admin_backends.clone(),
            admin_assets: Arc::new(Default::default()),
            cmd_tx: cmd_tx.clone(),
            theme_current: theme_current.clone(),
            theme_tx: theme_tx.clone(),
            settings_current: settings_current.clone(),
            settings_tx: settings_tx.clone(),
            player: etat_rx.clone(),
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
        });
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("web interface on http://{http_addr}/");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("status server: {e}");
            }
        });
    }

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
    let mut core = core::Core::new(
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
    );
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
    for (nom, display_client) in display_clients {
        relais_afficheur(nom, display_client, etat_rx.clone(), catalogue_rx.clone());
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
        status_state: status_state.clone(),
        admin_backends: admin_backends.clone(),
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
                cabler_a_chaud(annonce, &fils_chaud, &mut core, &mut rassemble).await;
                status_state.write().await.active_source = core.active_source().to_string();
            }
            Some((name, update)) = source_update_rx.recv() => {
                core.handle_source_update(&name, update);
            }
            Some((plugin, enrichment)) = enrich_rx.recv() => {
                core.handle_enrichment(&plugin, enrichment);
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
            Some((name, status)) = plugin_waits.next() => {
                tracing::warn!("plugin {name} exited: {status:?}");
                crate::status::mark_plugin_disconnected(&mut *status_state.write().await, &name);
            }
            status = mpv_child.wait() => {
                anyhow::bail!("mpv exited ({status:?}), stopping for restart by systemd");
            }
        }
    }
}
