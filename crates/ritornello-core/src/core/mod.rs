//! Le cœur : la struct `Core<P>`, sa construction, et `handle_source_update`,
//! point d'entrée d'une trame Source qui écrit dans tous les domaines.
//!
//! Un domaine par module enfant, chacun portant son `impl<P: Player> Core<P>`
//! partiel. Un module enfant voit les champs privés de la struct définie par
//! son parent : c'est ce qui rend ce découpage gratuit — aucun accesseur,
//! aucun champ `pub`. Ajouter un domaine, c'est ajouter un fichier.
//!
//! - `commands` : télécommande et IHM — playback/veille, volume, dizaines, déplacement, démarrage
//! - `deadlines` : incrustations et échéances que la boucle de `main.rs` doit réveiller
//! - `player` : événements de mpv, restart à rebours croissant, reprise au réveil
//! - `metadata` : identité, ICY, tags, enrichments, pochettes et extraction
//! - `position` : progress rapportée par mpv, ancre posée par un plugin
//! - `publication` : état du player et sources_catalog poussés aux afficheurs, SPA et plugins
//! - `settings` : sortie audio, langue, thème, écriture de `state.json`
//! - `sources` : order du cycle, bascule, arrivée à chaud et mort d'un greffon, `apply`
//! - `test_support` : player et sources factices, montages partagés par les tests

use crate::metadata::{Metadata, PlayerState};
use crate::player::mpv;
use crate::player::Player;
use crate::state::{self, PersistedState, StartupPower};
use crate::types::Event;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::SourceUpdate;
use ritornello_proto::{
    SourcesCatalog, Command, Enrichment, IdentityUpdate, InputMessage, NowPlaying, Overlay, Playback,
    Preset, SourceAction, SourceCatalog, SourceReq,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, RwLock};

mod commands;
mod deadlines;
mod playback;
mod track_metadata;
mod position;
mod publish;
mod settings;
mod sources;
pub use deadlines::next_deadline;

#[cfg(test)]
mod test_support;

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait Source: Send + Sync + 'static {
    async fn request(&self, req: SourceReq) -> Result<SourceAction>;
}

/// Ce que la boucle principale doit faire d'un événement du player.
///
/// C'est le cœur qui décide quelles variantes attestent la vivacité du stream
/// (`StreamAlive`) : la boucle de `main`, qui tient l'échéance de restart,
/// suit ce verdict au lieu de dupliquer la liste des variantes — les deux
/// listes avaient déjà commencé à devoir être maintenues en parallèle.
#[derive(Debug, PartialEq, Eq)]
pub enum EventOutcome {
    /// Rien à faire côté temporisation.
    Nothing,
    /// Le stream est vivant : annuler toute restart programmée.
    StreamAlive,
    /// Programmer une restart du stream dans ce délai.
    RetryIn(Duration),
}

/// Tout ce que le cœur reçoit du montage de `main` : ses sources, son état
/// persisté, ses canaux de sortie.
///
/// Une structure nommée plutôt qu'une longue liste de paramètres positionnels :
/// à huit éléments, l'order d'un appel ne se vérifie plus à l'œil, et deux
/// `PathBuf` voisins (`state_path`, `locales_root`) s'échangeraient sans que le
/// compilateur y trouve à redire.
pub struct Wiring {
    pub sources: HashMap<String, Arc<dyn Source>>,
    pub persisted: PersistedState,
    pub state_path: PathBuf,
    pub catalog: Arc<RwLock<Catalog>>,
    pub locales_root: PathBuf,
    pub metadata: MetadataWiring,
    /// Le sources_catalog des sources vers les plugins Display, sur **son propre**
    /// canal. Pas dans `MetadataWiring` : il ne descend ni à la SPA ni aux
    /// plugins `metadata`, et surtout pas dans `state` — un sources_catalog est
    /// structurel et rarement changeant, l'élargir ferait voyager les names de
    /// 51 stations sur chacune des trames d'état par seconde de playback.
    pub sources_catalog: watch::Sender<SourcesCatalog>,
}

/// Câblage des métadonnées.
pub struct MetadataWiring {
    /// Noms des plugins `metadata`, **dans l'order de déclaration** de
    /// `plugins.toml` : cet order est la priorité d'arbitrage.
    pub plugins: Vec<String>,
    /// Ce qui plays, vers les plugins `metadata`. Un `watch` et non un appel
    /// direct : un plugin qui ne read plus ne doit pas pouvoir figer le cœur.
    pub now_playing: watch::Sender<NowPlaying>,
    /// État du player, vers la SPA (route `GET /api/player`) et vers les
    /// plugins Display : un seul canal d'état structuré pour les deux, chacun
    /// composant ce qu'il veut de la même trame.
    pub state: watch::Sender<PlayerState>,
}

pub struct Core<P: Player> {
    player: P,
    sources: HashMap<String, Arc<dyn Source>>,
    source_order: Vec<String>,
    active_source: String,
    volume: u8,
    muted: bool,
    standby: bool,
    /// Standby as `state.json` had it at launch — the only thing
    /// `StartupPower::Previous` needs, and the reason it is a snapshot and
    /// not a re-read: `startup` runs after `new`, and by then `persist`
    /// may already have rewritten the file.
    persisted_standby: bool,
    expecting_stream: bool,
    /// Quelque chose est en playback, **quelle qu'en soit la nature**.
    ///
    /// Distinct d'`expecting_stream`, qui ne dit plus que « ce qui plays est un
    /// stream live susceptible de tomber, donc à relancer ». Les deux
    /// coïncidaient tant que seuls des stream étaient concernés ; depuis qu'une
    /// Source peut déclarer un contenu fini (`Play { finite: true }`),
    /// `expecting_stream` est faux pendant la playback d'un disque ou d'une
    /// liste de fichiers. S'en serve comme garde « ça plays » ferait taire
    /// toute couche de métadonnées sur exactement ces contenus-là.
    playback: bool,
    /// La playback en cours est **suspendue**. N'a de sens que quand `playback`
    /// est vrai ; `player_state` ne le consulte pas autrement.
    ///
    /// Remis à faux **au seul endroit** où `playback` passe à vrai. C'est la
    /// doctrine que `player_state` défend déjà pour `position_s` : un point
    /// unique ne peut pas être oublié, là où cinq effacements le seraient au
    /// sixième path ajouté.
    paused: bool,
    retry_count: u32,
    audio_device: Option<String>,
    /// Overlay temporaire (volume/muet/message) : incrustation à afficher +
    /// échéance. Porté par `PlayerState::overlay`, que le plugin d'affichage
    /// dessine en priorité sur toute autre chose.
    overlay: Option<(Overlay, Instant)>,
    /// Touche numérotée correspondant à ce qui plays, déclarée par la Source active
    /// (voir `SourceMessage::preset`). Oubliée dès que plus rien ne plays —
    /// c'est `set_identity(None)` qui fait foi, comme pour l'ardoise des
    /// métadonnées.
    preset: Option<u8>,
    /// Nom lisible de la présélection en cours, déclaré par la Source active
    /// (voir `SourceMessage::preset_name`). Vit et meurt avec `preset` : c'est
    /// `set_identity(None)` qui fait foi pour les deux, et nulle part ailleurs
    /// — la mise en veille, le changement de source et l'arrêt appellent tous
    /// `set_identity(None)`, donc ce point unique les couvre déjà.
    preset_name: Option<String>,
    /// Statut permanent déclaré par la Source active, déjà traduit (voir
    /// `SourceMessage::status`). Remplacé à chaque trame non éphémère, y
    /// compris par son absence — voir le test de convention.
    source_status: Option<String>,
    /// Mot de veille résolu, mémorisé à la construction et à chaque
    /// `set_locale` — jamais au moment de poser la veille : le sources_catalog se
    /// read derrière un verrou asynchrone, et `player_state` ne l'est pas. Le
    /// résoudre à la pose de la veille exigeait deux `await` faillibles avant
    /// de l'atteindre (`Command::Power`) : une Source ou mpv unreachable au
    /// premier passage en veille publiaient `standby: true` sans aucun statut,
    /// et l'écran devenait entièrement noir. Résolu en amont, le champ est
    /// toujours frais et ce piège d'ordonnancement disparaît. Gagne sur
    /// `source_status` dans `player_state` — l'appareil dort, ce que raconte
    /// la source n'a plus cours.
    standby_status: Option<String>,
    /// How many numbered presets the active source offers (stations,
    /// tracks), as last declared. Forgotten on source change and standby —
    /// the next source re-declares it on activate/wake — but kept on stop:
    /// a stopped radio still has its stations.
    preset_count: Option<u8>,
    /// Whether the active source has anything to eject, as last declared
    /// (`SourceMessage::can_eject`). Forgotten with the same timing as
    /// `preset_count` — source change and standby — for the same reason: it
    /// describes the source that is gone. **False, not `None`**, when nobody
    /// declares: not knowing means offering nothing, so the web remote greys
    /// its Eject key rather than sending a command into the void.
    can_eject: bool,
    /// Les présélections nommées **de chaque source**, indexées par name de
    /// source, telles que chacune les a déclarées (`SourceMessage::presets`).
    ///
    /// À part de `preset_count`, et ce n'est pas une redondance : `preset_count`
    /// décrit la source **active** et s'oublie avec elle, alors qu'une table
    /// indexée par name décrit *toutes* les sources en même temps. C'est ce
    /// qu'exige un client MPD, qui demande `listplaylistinfo "radio"` pendant
    /// que le cd plays. Rien ne l'oublie donc : ni la bascule de source, ni la
    /// veille.
    presets_par_source: HashMap<String, Vec<Preset>>,
    /// Remote tens offset in flight: `Plus10` presses accumulate here until
    /// a digit key consumes them (`+10` then `4` selects 14). Cleared by the
    /// overlay's own deadline (`expire_overlay`) or by its consumption
    /// (`Select`), and just as much by `apply_command`'s abandon
    /// guard — which also clears `self.overlay` in that third case, so an
    /// abandoned offset never leaves its `+NN` behind on a display that no
    /// longer means it.
    pending_tens: u8,
    state_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    locale: Option<String>,
    locales_root: PathBuf,
    theme: Option<String>,
    mode: Option<String>,
    /// Métadonnées du track : identité de ce qui plays, titre ICY, et
    /// enrichments des plugins. Voir `metadata.rs` pour l'arbitrage.
    metadata: Metadata,
    now_playing_tx: watch::Sender<NowPlaying>,
    state_tx: watch::Sender<PlayerState>,
    /// Le sources_catalog des sources vers les afficheurs. Un canal séparé d'`state_tx`
    /// et jamais publié par `publish_state` : voir `publish_catalog`.
    sources_catalog_tx: watch::Sender<SourcesCatalog>,
    /// Behavior settings (hold-to-repeat timings, startup power state),
    /// persisted with the rest of the state.
    settings: crate::state::Settings,
    /// Hold-to-repeat pacing: instant before which a held volume command is
    /// ignored. Armed by a fresh volume step (now + initial delay), re-armed
    /// by each applied repeat (now + interval). `None` until a first press —
    /// a held event arriving out of nowhere (core restarted mid-hold) does
    /// nothing.
    volume_deadline: Option<Instant>,
    /// Où en est ce qui plays, en secondes entières, tel que le dernier
    /// rafraîchissement l'a établi. Publié tel quel par `player_state`.
    position_s: Option<u32>,
    /// Durée **mesurée par mpv**, distincte de celle qu'un plugin `metadata`
    /// announcement. Gardée à part parce qu'elle la supplante : les fondre en un
    /// seul champ ferait perdre la trace de qui a parlé, et la précédence
    /// deviendrait un order d'écriture — le kind d'invariant qui se casse en
    /// silence.
    measured_duration_s: Option<u32>,
    /// Position annoncée par un plugin `metadata`, et l'instant où elle est
    /// arrivée. Le cœur l'avance lui-même entre deux annonces — Radio France
    /// n'interroge le direct que toutes les quelques dizaines de secondes, et
    /// sans cette avance la barre resterait figée entre deux réponses.
    position_anchor: Option<(u32, Instant)>,
    /// Cache partagé avec le routeur : la tâche détachée y dépose, la route y
    /// read. **Le même `Arc`** que celui remis à l'`AppState` HTTP — voir la
    /// note à son lieu de construction dans `main.rs` — sans quoi une
    /// cover téléchargée par le cœur ne serait jamais lisible par la
    /// route.
    covers: Arc<crate::cover::CoverCache>,
    /// Résultats des récupérations détachées, consommés par la boucle de
    /// `main` (voir son bras `pochette_rx.recv()`). Le booléen dit si la
    /// récupération a abouti — nécessaire pour que `cover_arrived` libère
    /// `cover_in_flight` même sur un échec, au lieu de laisser cette clé
    /// bloquée pour le reste du processus.
    cover_tx: mpsc::Sender<(String, bool)>,
    /// Clé dont la récupération est en vol, pour ne pas la lancer deux fois.
    cover_in_flight: Option<String>,
    /// Dernier path annoncé par mpv (`Event::Path`), retenu **seulement**
    /// pour comparaison à l'arrivée d'une extraction détachée — jamais
    /// interprété, comme le veut le principe posé pour `OBSERVED`. Une
    /// extraction lancée pour un path peut revenir après que mpv soit
    /// passé à un autre : sans cette trace, son résultat s'installerait
    /// après coup sur la piste suivante.
    current_path: Option<String>,
    /// Chemin dont l'extraction embarquée est actuellement en vol, pour ne
    /// pas en relancer une deuxième pendant que la première tourne encore
    /// sur ce même fichier.
    extraction_in_flight: Option<String>,
    /// Résultat d'une extraction détachée par `handle_path`, consommé par la
    /// boucle `select!` de `main` (voir `extraction_arrived`). Symétrique de
    /// `cover_tx` ci-dessus.
    extraction_tx: mpsc::Sender<(String, Option<ritornello_proto::CoverRef>)>,
    /// Disjoncteur qui bounded l'appel `lofty`, strictement bloquant et
    /// potentiellement sur un partage réseau : voir `health.rs` et le
    /// commentaire de `handle_path`.
    health: Arc<crate::health::Health>,
}

/// Résout le mot de veille depuis un sources_catalog déjà en main.
///
/// Fonction libre plutôt que méthode : elle sert à la fois à la construction
/// (sources_catalog lu par `try_read`, avant que `self` n'existe) et à `set_locale`
/// (sources_catalog tout juste chargé, avant qu'il ne remplace celui du cœur), donc
/// aucune des deux n'a besoin de passer par le verrou asynchrone une seconde
/// fois.
fn resolve_standby_status(catalog: &Catalog) -> String {
    catalog.get("standby").to_string()
}

impl<P: Player> Core<P> {
    pub fn new(
        player: P,
        wiring: Wiring,
        covers: Arc<crate::cover::CoverCache>,
        cover_tx: mpsc::Sender<(String, bool)>,
        extraction_tx: mpsc::Sender<(String, Option<ritornello_proto::CoverRef>)>,
    ) -> Self {
        let Wiring { sources, persisted, state_path, catalog, locales_root, metadata, sources_catalog } =
            wiring;
        let mut source_order: Vec<String> = sources.keys().cloned().collect();
        source_order.sort();
        let active_source = if sources.contains_key(&persisted.active_source) {
            persisted.active_source.clone()
        } else {
            source_order.first().cloned().unwrap_or_default()
        };
        // Résolu tout de suite : le seul écrivain de ce sources_catalog est
        // `set_locale`, joignable uniquement depuis la boucle `select!` qui ne
        // démarre qu'après le retour d'ici — aucun verrou concurrent ne peut
        // donc exister à cet instant. Voir `resolve_standby_status` pour la
        // raison de ce choix (plus jamais résolu au moment de poser la veille).
        //
        // L'échec est malgré tout journalisé plutôt qu'avalé : il rendrait
        // l'écran de veille entièrement clear jusqu'au prochain changement de
        // langue — précisément le défaut que ce pré-calcul corrige. Un
        // invariant qu'on croit tenu et que personne ne vérifie est ce qui a
        // produit ce défaut la première fois.
        let standby_status = match catalog.try_read() {
            Ok(cat) => Some(resolve_standby_status(&cat)),
            Err(_) => {
                tracing::warn!(
                    "standby label unavailable at startup: the standby screen will stay blank until the next locale change"
                );
                None
            }
        };
        let coeur = Self {
            player,
            sources,
            source_order,
            active_source,
            // Reborné à la playback : `state.json` peut avoir été édité à la
            // main, et un `volume: 255` partirait tel quel à mpv au réveil.
            volume: persisted.volume.min(100),
            muted: false,
            standby: false,
            persisted_standby: persisted.standby,
            expecting_stream: false,
            playback: false,
            paused: false,
            retry_count: 0,
            audio_device: persisted.audio_device.clone(),
            overlay: None,
            preset: None,
            preset_name: None,
            source_status: None,
            standby_status,
            preset_count: None,
            can_eject: false,
            presets_par_source: HashMap::new(),
            pending_tens: 0,
            state_path,
            catalog,
            locale: persisted.locale.clone(),
            locales_root,
            theme: persisted.theme.clone(),
            mode: persisted.mode.clone(),
            metadata: Metadata::new(metadata.plugins),
            now_playing_tx: metadata.now_playing,
            state_tx: metadata.state,
            sources_catalog_tx: sources_catalog,
            settings: persisted.settings.clone(),
            volume_deadline: None,
            position_s: None,
            measured_duration_s: None,
            position_anchor: None,
            covers,
            cover_tx,
            cover_in_flight: None,
            current_path: None,
            extraction_in_flight: None,
            extraction_tx,
            health: Arc::new(crate::health::Health::new()),
        };
        // Les sources câblées au démarrage sont déjà connues : sans cette
        // publication, le canal garderait son `SourcesCatalog::default()` clear et un
        // afficheur relayé avant la première présélection croirait que
        // l'appareil n'a aucune source. `add_source` couvre la suite.
        coeur.publish_catalog();
        // Les réglages persistés atteignent le cache de pochettes ici, et pas
        // seulement au premier `set_settings` : sans cette line, un appareil
        // dont `state.json` décoche le réencodage l'appliquerait à partir de la
        // première visite de la page de configuration, et pousserait des images
        // pleine size jusque-là. Le démarrage doit obéir au fichier.
        coeur.covers.set_cover_settings(crate::cover::CoverSettings::from(&coeur.settings));
        coeur
    }

    /// Applique ce qu'une Source rapporte : son statut, et/ou l'identité de ce
    /// qu'elle plays désormais.
    ///
    /// Les deux arrivent dans la même trame et sont appliqués ensemble, sans
    /// affichage intermédiaire : aucun instant observable ne voit la line
    /// affichée décrire un track et l'identité annoncée aux plugins en décrire
    /// un autre.
    ///
    /// Deux sortes de trames arrivent par ce canal, et elles ne prennent pas le
    /// même path :
    ///
    /// - celles qui **recomposent la vue** — une réponse de Source, qui déclare
    ///   une identité ou un statut, ou un mot éphémère à incruster ;
    /// - celles qui **annoncent un fait** sans rien dire de ce qui plays — les
    ///   présélections nommées, leur nombre, le tiroir, la renumérotation de la
    ///   piste en cours, la cover. Celles-là rendent la main avant le
    ///   traitement du statut : voir le retour anticipé.
    ///
    /// **En pratique, presque toute trame de production emprunte le second
    /// path dès qu'elle ne déclare ni identité ni statut** : le prédicat qui
    /// l'ouvre est une tautologie pour le SDK (voir le corps). Tout champ doit
    /// donc être appliqué **sur les deux chemins** — ce qui n'est appliqué qu'en
    /// bas de fonction n'est jamais appliqué. La déstructuration exhaustive en
    /// tête de fonction est ce qui rend cette décision obligatoire pour tout
    /// champ ajouté plus tard.
    pub fn handle_source_update(&mut self, name: &str, update: SourceUpdate) {
        // **Une trame d'une source que le cœur ne connaît plus est jetée, et
        // entière.** Le fan-out des requêtes de sources_catalog est détaché : un
        // `ListPresets` part dans sa propre tâche, et `remove_source` peut
        // s'exécuter entre la requête et la réponse — un greffon éteint depuis
        // l'IHM, ou mort de lui-même. Sans ce garde, la réponse encore en vol
        // ré-insérait la liste dans `presets_par_source` **après** l'éviction,
        // parce que cette insertion se fait délibérément avant le garde de
        // source active (le sources_catalog décrit toutes les sources, pas celle qui
        // plays). Le sources_catalog republié annonçait alors une liste enregistrée
        // pour une source qui n'existe plus, un client MPD la mettait en cache,
        // et un `load` dessus n'était refusé qu'au dernier moment par le garde
        // de `Command::SelectSource` — donc après avoir menti à l'utilisateur.
        //
        // `sources` et non `source_order` : les deux sont retirés ensemble par
        // `remove_source`, mais `sources` est la table qui dit ce que le cœur
        // peut encore joindre. Le garde ne peut pas refuser une trame
        // légitimement précoce : au démarrage les clients sont câblés avant que
        // la boucle ne drame le canal, et le câblage à chaud est *awaité* depuis
        // la boucle principale, qui ne traite donc aucune trame pendant ce
        // temps.
        if !self.sources.contains_key(name) {
            tracing::debug!("source update for {name} dropped: no longer a wired source");
            return;
        }
        // **Déstructuration exhaustive, et c'est le garde-fou principal de cette
        // fonction.** Pas de `..` : ajouter un champ à `SourceUpdate` ne compile
        // plus tant que quelqu'un n'a pas décidé, ici, à laquelle des deux
        // moitiés il appartient — le prédicat `porte_un_fait` **et** son
        // application sur les deux chemins.
        //
        // Dérivé plutôt que demandé, et c'est une leçon payée. La fusion du
        // chantier des pochettes a ajouté `cover` au prédicat sans l'appliquer
        // sur le path du retour anticipé : le champ était gardé, donc la trame
        // passait, mais son application vivait tout en bas de la fonction, après
        // un `return` que cette trame prenait toujours. Chaque cover de Source
        // était perdue **en silence**. Rien ne l'a signalé : le prédicat teste
        // les champs un par un, et `SourceUpdate` dérive `Default`, si bien qu'un
        // dixième champ ne casse aucun littéral et aucun test. Un commentaire
        // réclamant « pensez aux deux moitiés » se read après coup ; une
        // déstructuration exhaustive, elle, ne peut pas être oubliée. Même
        // principe que l'announcement d'un greffon, qui ne peut pas mentir sur ses
        // genres parce qu'ils sont déduits et non déclarés.
        let SourceUpdate {
            identity,
            transient,
            preset,
            preset_count,
            preset_name,
            status,
            can_eject,
            presets,
            cover,
        } = update;
        // Lu **avant** le garde ci-dessous, et c'est voulu : le sources_catalog décrit
        // toutes les sources, pas celle qui plays. Un client MPD interroge
        // `listplaylistinfo "radio"` pendant que le cd plays, et la veille ne
        // change rien à ce qu'une source contains. Le garde, lui, protège ce qui
        // décrit **ce qui plays** — identité, statut, message éphémère — et reste
        // en place pour tout le reste.
        let porte_des_presets = presets.is_some();
        if let Some(presets) = presets {
            self.presets_par_source.insert(name.to_string(), presets);
            self.publish_catalog();
        }
        if self.standby || name != self.active_source {
            return;
        }
        // `preset_count` et `can_eject` décrivent la **source active** — combien
        // de présélections elle offre, si elle a quelque chose à éjecter — et
        // leur doc de champ les nomme déjà comme une paire, oubliée ensemble à la
        // bascule de source et à la veille. Appliqués ici, **avant** le retour
        // anticipé, pour que celui-ci ne puisse pas les avaler ; l'order
        // vis-à-vis de l'identité est sans effet, `set_identity` n'y touche pas.
        if let Some(c) = preset_count {
            self.preset_count = Some(c);
        }
        if let Some(e) = can_eject {
            self.can_eject = e;
        }
        // **Les deux chemins, et lequel des deux porte réellement la sûreté.**
        //
        // Le traitement du statut, juste en dessous, *remplace* le statut
        // mémorisé par ce que porte la trame, absence comprise : une trame
        // permanente muette **efface** ce que la source avait déclaré. Le retour
        // anticipé existe pour que les trames qui ne font qu'annoncer un fait ne
        // l'atteignent pas.
        //
        // **`porte_un_fait` est en pratique une tautologie, et il faut le
        // savoir.** `serve_source` estampille
        // `can_eject: Some(plugin.can_eject())` sur **chacune** des deux trames
        // qu'il écrit — la réponse corrélée et la notification spontanée — et
        // `SourceClient` le recopie tel quel (voir la doc de
        // `SourceMessage::can_eject` : « The SDK stamps it on **every** frame »).
        // Toute trame venue du SDK arme donc `can_eject.is_some()`, donc arme
        // `porte_un_fait`. Les autres clauses ne changent rien pour une trame de
        // production : elles sont une **assurance** au cas où l'estampille
        // deviendrait conditionnelle, pas un garde vivant.
        //
        // La conséquence est celle qui compte : ce n'est **pas** le prédicat qui
        // protège quoi que ce soit, c'est le fait d'appliquer chaque champ **sur
        // les deux chemins**. Une trame qui n'announcement qu'un fait prend le retour
        // anticipé de toute façon ; ce qui n'est appliqué qu'en bas de fonction
        // n'est donc jamais appliqué du tout. C'est exactement le défaut que la
        // fusion du chantier des pochettes a produit — `cover` gardé mais
        // appliqué seulement en bas, donc chaque cover perdue en silence — et
        // c'est la déstructuration en tête de fonction, non ce commentaire, qui
        // empêche sa récurrence.
        //
        // Le cas historique du statut effacé, lui, ne peut plus venir du SDK :
        // `preset_count` seul (la page d'admin de `plugin-files` enregistrant une
        // liste) blanchissait « PAS DE DISQUE » sur la console et la SPA jusqu'à
        // la commande suivante, et c'est le retour anticipé qui l'a réparé.
        //
        // `recompose_la_vue` reprend l'invariant du SDK mot pour mot : seule une
        // identité ou un statut déclarés attestent une recomposition de vue, et
        // `transient` s'y joint parce qu'un mot éphémère est un propos sur ce qui
        // plays (il doit garder son incrustation et désarmer un `+NN` en vol).
        // `preset`, `preset_name`, `preset_count`, `can_eject`, `presets` et
        // `cover` n'attestent rien : tous ont la convention « absent = garder »,
        // donc aucun ne peut prouver que la trame décrit la vue entière.
        let recompose_la_vue = transient || identity.is_some() || status.is_some();
        let porte_un_fait = porte_des_presets
            || preset_count.is_some()
            || can_eject.is_some()
            || preset.is_some()
            || preset_name.is_some()
            || cover.is_some();
        if porte_un_fait && !recompose_la_vue {
            // Un **seul** appel, et c'est le point : les champs « absent =
            // garder » qui doivent être appliqués après l'identité vivent tous
            // dans `apply_declared_facts`, appelée ici et une seule autre
            // fois en bas de fonction. Un champ ajouté là-dedans atterrit donc
            // sur les deux chemins par construction, au lieu de dépendre de
            // quelqu'un qui se souvienne de le recopier — c'est exactement
            // l'oubli qui a fait perdre chaque cover de Source en silence.
            self.apply_declared_facts(preset, preset_name, cover, name);
            // Publier quand même : compte, tiroir et sélection font partie de
            // l'état diffusé, et le canal déduplique si rien n'a bougé.
            self.publish_state();
            return;
        }
        // `status` est réaffirmé par chaque trame permanente : absent vaut
        // effacé — convention **inverse** de celle de `preset`, et la seule
        // qui permette d'effacer un statut (« PAS DE DISQUE » doit pouvoir
        // disparaître à l'insertion d'un disque). Une trame éphémère, elle, ne
        // touche pas au statut mémorisé : son mot va dans l'incrustation
        // ci-dessous, pas ici.
        if !transient {
            self.source_status = status.clone();
        }
        if transient {
            // Message éphémère (« présélection clear ») : il emprunte
            // l'emplacement et l'échéance de l'incrustation volume/muet, donc
            // `self.source_status` — le statut permanent — est conservé et
            // reparaît d'elle-même. Sans cela, le message restait à l'écran
            // indéfiniment alors que la playback continuait sur la station
            // précédente : l'affichage décrivait durablement un état qui
            // n'existait plus. `overlay_ms`, pas `tens_window_ms` : ce message
            // n'a rien à voir avec le décalage `+NN` de la télécommande, seule
            // l'incrustation volume/muet partage son échéance avec lui.
            //
            // Un décalage `+NN` en cours perd donc son emplacement d'affichage
            // ici : le désarmer avec lui est ce qui évite qu'il survive
            // derrière un écran qui ne le montre plus (même raison que le
            // garde d'abandon d'`apply_command`) — que la trame porte ou
            // non un mot à afficher.
            self.pending_tens = 0;
            if let Some(mot) = status {
                let deadline = Instant::now() + Duration::from_millis(self.settings.overlay_ms.into());
                self.overlay = Some((
                    Overlay::Message { text: mot, remaining_ms: self.settings.overlay_ms },
                    deadline,
                ));
            }
        }
        if let Some(identity) = identity {
            let valeur = match identity {
                IdentityUpdate::Playing(v) => Some(v),
                IdentityUpdate::Nothing => None,
            };
            self.set_identity(valeur);
        }
        // Le second — et dernier — appelant de `apply_declared_facts`,
        // ici **après** l'identité : `set_identity(None)` efface la sélection, et
        // `set_identity` tout court remet à zéro tout ce que `Metadata`
        // retenait, cover de la Source comprise. Une trame qui déclare
        // explicitement l'un ou l'autre doit gagner sur ce reset, donc elle est
        // appliquée derrière lui. C'est cet order-là qui interdit de remonter cet
        // appel avec `preset_count` ; le path du retour anticipé, lui, ne peut
        // pas porter d'identité par construction, donc l'y appeler est sûr.
        self.apply_declared_facts(preset, preset_name, cover, name);
        // `preset_count` et `can_eject` sont appliqués **en tête** de cette
        // fonction, avant le retour anticipé, pour la même raison.
        //
        // Toujours publier : la sélection courante fait partie de l'état
        // diffusé, et cet appel couvre la trame qui ne change ni identité ni
        // métadonnées (les autres chemins publient déjà, et le canal
        // déduplique). `player_state` porte toujours l'incrustation active
        // aux côtés du reste : une trame source arrivant pendant une
        // incrustation met donc à jour source_status/preset/preset_name sans
        // rien changer de ce que l'afficheur montre tant qu'elle dure.
        self.publish_state();
    }

    /// Tout ce qu'une trame de Source déclare et qui doit être appliqué
    /// **après** l'identité, en un seul endroit.
    ///
    /// **C'est la place, et non un commentaire, qui rend la règle tenable.**
    /// `handle_source_update` a deux sorties — le retour anticipé des trames qui
    /// n'annoncent qu'un fait, et le bas de fonction pour celles qui recomposent
    /// la vue — et un champ appliqué à un seul des deux endroits est perdu en
    /// silence sur l'autre. C'est arrivé deux fois : `presets`, puis `cover`, ce
    /// dernier gardé par le prédicat mais appliqué seulement en bas, donc jamais,
    /// puisque une cover de Source arrive toujours seule et prend toujours le
    /// retour anticipé. La déstructuration exhaustive en tête de fonction force
    /// la *question* (« à laquelle des deux moitiés ce champ appartient-il ? »)
    /// mais pas la *réponse* : deux appels côte à côte pouvaient toujours
    /// diverger. Avec un seul corps appelé aux deux sorties, la réponse est
    /// structurelle pour tout champ ajouté ici.
    ///
    /// La limite, dite franchement : rien n'empêche d'écrire un jour un nouveau
    /// champ *à côté* de cet appel plutôt que dedans. Ce qui est acquis, c'est
    /// qu'aucun champ déjà passé par ici ne peut manquer sur un path.
    fn apply_declared_facts(
        &mut self,
        preset: Option<u8>,
        preset_name: Option<String>,
        cover: Option<ritornello_proto::CoverRef>,
        name: &str,
    ) {
        self.apply_selection(preset, preset_name);
        self.apply_source_cover(cover, name);
    }

}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[tokio::test]
    async fn la_veille_ignore_les_mises_a_jour_de_la_source_et_le_reveil_les_reprend() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(state_rx.borrow_and_update().standby);
        let mut update = bare_update();
        update.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update.clone());
        assert_eq!(state_rx.borrow().preset_name, None, "en veille, la trame source est ignoree");
        core.handle_command(Command::Power).await.unwrap();
        core.handle_source_update("radio", update);
        assert_eq!(
            state_rx.borrow_and_update().preset_name.as_deref(),
            Some("FIP"),
            "le reveil laisse la source reprendre la main"
        );
    }

    #[tokio::test]
    async fn mise_a_jour_dune_source_inactive_est_ignoree() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update_cd = bare_update();
        update_cd.preset_name = Some("CD".into());
        core.handle_source_update("cd", update_cd);
        assert_eq!(state_rx.borrow().preset_name, None, "la mise a jour de \"cd\" (inactive) n'a pas ete appliquee");
        let mut update_radio = bare_update();
        update_radio.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update_radio);
        assert_eq!(state_rx.borrow_and_update().preset_name.as_deref(), Some("FIP"));
    }

    #[tokio::test]
    async fn le_statut_de_la_source_ne_survit_pas_a_la_mise_en_veille() {
        // Second scénario d'I2 : sans effacement explicite, `source_status`
        // restait en mémoire pendant la veille (masqué par la priorité du mot
        // de veille dans `player_state`) et reparaissait au réveil tant que la
        // Source n'avait pas reparlé — un mensonge prêt à resurgir.
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update = bare_update();
        update.status = Some("pas de disque".into());
        core.handle_source_update("radio", update);
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("pas de disque"));
        core.handle_command(Command::Power).await.unwrap(); // veille
        core.handle_command(Command::Power).await.unwrap(); // reveil, source muette
        assert_eq!(
            state_rx.borrow_and_update().status,
            None,
            "le statut de l'ancienne trame ne doit pas reapparaitre au reveil avant que la Source n'ait reparle"
        );
    }

    #[tokio::test]
    async fn le_compte_de_preselections_est_memorise_et_publie() {
        // Une trame qui déclare un compte doit se retrouver dans PlayerState ;
        // une trame muette sur le sujet ne doit pas l'effacer.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_count(Some(23)));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_source_update("radio", update_with_count(None));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        // Some(0) écrase : le cd sans disque dit « rien à numéroter ».
        core.handle_source_update("radio", update_with_count(Some(0)));
        assert_eq!(state_rx.borrow().preset_count, Some(0));
    }

    #[tokio::test]
    async fn la_capacite_dejection_est_memorisee_et_publiee() {
        // Fausse par défaut : ne pas savoir, c'est n'offrir rien — la
        // télécommande web grise sa touche Eject tant que personne ne l'a
        // réclamée. Une trame muette sur le sujet ne l'efface pas.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        assert!(!state_rx.borrow().can_eject, "rien de declare : rien d'offert");
        core.handle_source_update("radio", update_with_eject(Some(true)));
        assert!(state_rx.borrow().can_eject);
        core.handle_source_update("radio", update_with_eject(None));
        assert!(state_rx.borrow().can_eject, "une trame muette ne retire pas la capacite");
        core.handle_source_update("radio", update_with_eject(Some(false)));
        assert!(!state_rx.borrow().can_eject);
    }

    #[tokio::test]
    async fn lejection_survit_a_larret_mais_ni_au_changement_de_source_ni_a_la_veille() {
        // Même calendrier d'oubli que `preset_count`, et pour la même raison :
        // la capacité décrit la Source, pas ce qui plays. Un arrêt ne change
        // pas le fait que le player a un tiroir ; changer de source, si.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_eject(Some(true)));
        core.handle_command(Command::Stop).await.unwrap();
        assert!(state_rx.borrow().can_eject, "un tiroir ne disparait pas a l'arret");
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(!state_rx.borrow().can_eject, "la capacite decrit la source qui s'en va");
    }

    #[tokio::test]
    async fn la_veille_retire_la_capacite_dejection() {
        // La veille ne laisse passer aucune commande (`handle_command`) : offrir
        // Eject y serait un mensonge de plus. Un cœur neuf par test — après
        // `SourceCycle`, plus rien ne garantit que « radio » soit encore la
        // source active, donc plus rien ne garantit qu'une trame la concernant
        // franchisse le garde-fou de `handle_source_update`.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_eject(Some(true)));
        assert!(state_rx.borrow().can_eject);
        core.handle_command(Command::Power).await.unwrap();
        assert!(!state_rx.borrow().can_eject);
    }

    #[tokio::test]
    async fn le_compte_survit_a_larret_mais_pas_au_changement_de_source() {
        // Stop efface preset (plus rien ne plays) mais pas le compte : une radio
        // arrêtée a toujours ses stations.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_count(Some(23)));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(state_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn une_mise_a_jour_de_seul_compte_laisse_morceau_et_identite_intacts() {
        // Garantie de sûreté dont dépend l'announcement spontanée de `preset_count`
        // par la radio après un enregistrement réussi côté admin (voir
        // `RadioSource::poll_notification`) : une trame qui ne porte que le
        // compte doit laisser le track en cours et l'identité intacts, et
        // tout de même publier l'état. Rien ne le vérifiait avant ce test.
        let (mut core, mut np_rx, state_rx, _d) = setup_metadata(vec![]);
        let id = serde_json::json!({"kind": "stream", "url": "http://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        // Repère pris après l'installation de l'identité : seuls les
        // changements ultérieurs doivent être détectés.
        np_rx.borrow_and_update();
        let morceau_avant = state_rx.borrow().track.clone();

        core.handle_source_update("radio", update_with_count(Some(5)));

        assert_eq!(state_rx.borrow().preset_count, Some(5), "le compte doit etre publie");
        assert_eq!(state_rx.borrow().track, morceau_avant, "le track ne doit pas bouger");
        assert!(!np_rx.has_changed().unwrap(), "l'identity ne doit pas bouger");
        assert_eq!(np_rx.borrow().identity, Some(id));
    }

    #[tokio::test]
    async fn une_mise_a_jour_de_seul_nom_laisse_morceau_et_identite_intacts() {
        // Même garantie que pour `preset_count` ci-dessus, cette fois pour
        // `preset_name` : une trame qui ne porte que le name doit se fondre
        // dans l'état publié sans rien déranger d'autre.
        let (mut core, mut np_rx, state_rx, _d) = setup_metadata(vec![]);
        let id = serde_json::json!({"kind": "stream", "url": "http://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        np_rx.borrow_and_update();
        let morceau_avant = state_rx.borrow().track.clone();

        core.handle_source_update("radio", update_with_name(Some("FIP")));

        assert_eq!(state_rx.borrow().preset_name.as_deref(), Some("FIP"), "le name doit etre publie");
        assert_eq!(state_rx.borrow().track, morceau_avant, "le track ne doit pas bouger");
        assert!(!np_rx.has_changed().unwrap(), "l'identity ne doit pas bouger");
        assert_eq!(np_rx.borrow().identity, Some(id));
    }

    #[tokio::test]
    async fn le_compte_est_oublie_en_veille() {
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("radio", update_with_count(Some(23)));
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::Power).await.unwrap(); // entre en veille
        assert_eq!(state_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn un_message_ephemere_seffece_et_laisse_reparaitre_letat_precedent() {
        // Cas reel : selectionner une preselection clear. Rien n'est lance, la
        // station precedente plays toujours — le message doit donc passer, puis
        // ceder la place, sans que le statut permanent ni les metadata bougent.
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", plays(id.clone()));
        let mut permanent = bare_update();
        permanent.status = Some("FIP".into());
        core.handle_source_update("radio", permanent);
        core.handle_enrichment("ouifm", enrichment(id, "Miles Davis", "So What"));
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("FIP"));

        let mut ephemere = bare_update();
        ephemere.transient = true;
        // Le mot affiché vient de `status`, jamais d'une vue composée (voir
        // Task 3) : c'est ainsi que le plugin radio le déclare réellement sur
        // la branche « présélection clear ».
        ephemere.status = Some("empty preset".into());
        core.handle_source_update("radio", ephemere);
        let pendant = state_rx.borrow_and_update().clone();
        assert!(matches!(pendant.overlay, Some(Overlay::Message { .. })), "le message doit s'afficher");
        assert_eq!(pendant.status.as_deref(), Some("FIP"), "le statut permanent n'a pas bouge");
        assert!(core.overlay_deadline().is_some(), "et porter une deadline");

        core.expire_overlay();
        let apres = state_rx.borrow_and_update().clone();
        assert!(apres.overlay.is_none());
        assert_eq!(apres.status.as_deref(), Some("FIP"), "la station qui plays doit reparaitre");
        assert_eq!(apres.track.title.as_deref(), Some("So What"), "les metadata aussi");
    }

    #[tokio::test]
    async fn un_statut_de_source_est_publie_puis_remplace() {
        // Convention **différente** de celle de `preset` : dans une trame,
        // `status` absent signifie « aucun statut », pas « garder le précédent ».
        // C'est ce qui reproduit le comportement actuel — une source recompose sa
        // vue entière à chaque trame — et la seule convention qui permette
        // d'effacer un statut : sinon « PAS DE DISQUE » resterait affiché après
        // l'insertion d'un disque, sans aucune façon de l'annuler.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut update = bare_update();
        update.status = Some("PAS DE DISQUE".into());
        core.handle_source_update("radio", update);
        assert_eq!(core.player_state().status.as_deref(), Some("PAS DE DISQUE"));

        core.handle_source_update("radio", bare_update());
        assert_eq!(core.player_state().status, None, "absent vaut effacé, pas conservé");
    }

    #[tokio::test]
    async fn un_statut_ephemere_ne_touche_pas_au_statut_memorise() {
        // Le cas « présélection clear » : un mot passager, alors que la station
        // précédente continue de jouer. Il alimente l'incrustation, et le statut
        // permanent doit reparaître à l'échéance.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("FIP".into());
        core.handle_source_update("radio", permanent);

        let mut ephemere = bare_update();
        ephemere.status = Some("PRESELECTION VIDE".into());
        ephemere.transient = true;
        core.handle_source_update("radio", ephemere);
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("FIP"),
            "le statut permanent survit à un message éphémère"
        );
        assert!(matches!(core.player_state().overlay, Some(Overlay::Message { .. })));

        core.expire_overlay();
        assert_eq!(core.player_state().status.as_deref(), Some("FIP"));
        assert!(core.player_state().overlay.is_none());
    }

    #[tokio::test]
    async fn un_compte_seul_neffece_pas_le_statut_de_la_source() {
        // Le defaut etait **en service** : `plugin-files` announcement un compte sans
        // statut quand sa page d'admin enregistre une liste, alors qu'il declare
        // un statut permanent partout ailleurs. Le statut disparaissait donc de
        // la console et de la SPA jusqu'a la commande suivante.
        //
        // `preset_count` est dans le predicat de trame interessante du SDK depuis
        // toujours : cette trame **arrive** au coeur, et le traitement du statut
        // l'aurait effacee faute d'en porter un.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("6 FICHIERS".into());
        core.handle_source_update("radio", permanent);
        assert_eq!(core.player_state().status.as_deref(), Some("6 FICHIERS"));

        let mut compte = bare_update();
        compte.preset_count = Some(6);
        core.handle_source_update("radio", compte);
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("6 FICHIERS"),
            "une trame qui ne declare ni identity ni statut n'a rien a dire du statut"
        );
        assert_eq!(
            core.player_state().preset_count,
            Some(6),
            "et le compte doit quand meme etre pris : le retour anticipe est apres lui"
        );
    }

    #[tokio::test]
    async fn un_avis_de_renumerotation_neffece_pas_le_statut() {
        // La trame exacte de `plugin-files` apres un enregistrement depuis sa
        // page d'admin : le compte, **et** le numero et le name de la piste
        // courante, sans identity (la piste ne doit pas etre redeclaree) ni
        // statut. Trois champs de fusion, aucune recomposition de vue.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("6 FICHIERS".into());
        core.handle_source_update("radio", permanent);

        let mut notice = bare_update();
        notice.preset_count = Some(9);
        notice.preset = Some(3);
        notice.preset_name = Some("Kind of Blue".into());
        core.handle_source_update("radio", notice);
        let state = core.player_state();
        assert_eq!(state.status.as_deref(), Some("6 FICHIERS"), "le statut permanent survit");
        assert_eq!(state.preset_count, Some(9));
        assert_eq!(state.preset, Some(3));
        assert_eq!(state.preset_name.as_deref(), Some("Kind of Blue"));
    }

    #[tokio::test]
    async fn des_preselections_seules_neffacent_pas_le_statut() {
        // Le second producteur du meme piege : la reponse a `ListPresets` ne
        // porte ni identity ni statut. Sans le retour anticipe, demander son
        // sources_catalog a une source blanchirait son statut.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = bare_update();
        permanent.status = Some("PAS DE DISQUE".into());
        core.handle_source_update("radio", permanent);

        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("PAS DE DISQUE"),
            "demander le sources_catalog ne doit pas effacer l'ecran"
        );
        assert_eq!(names(&core.sources_catalog()), vec!["cd".to_string(), "radio".into()]);
    }

    #[tokio::test]
    async fn les_preselections_dune_source_inactive_sont_gardees() {
        // La raison d'etre du contournement du garde : `listplaylistinfo "radio"`
        // s'interroge pendant que le cd plays.
        let (mut core, _pc, _sc, _rx, _d) =
            setup_persisted(PersistedState { active_source: "cd".into(), ..Default::default() });
        assert_eq!(core.active_source(), "cd");
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP"), preset_of(5, "OUI FM")]));
        let cat = core.sources_catalog();
        let radio = cat.sources.iter().find(|s| s.name == "radio").expect("radio est declaree");
        assert_eq!(radio.presets, vec![preset_of(1, "FIP"), preset_of(5, "OUI FM")]);
        let cd = cat.sources.iter().find(|s| s.name == "cd").expect("cd est declaree");
        assert!(cd.presets.is_empty(), "le cd n'enumere rien, il figure quand meme");
    }

    #[tokio::test]
    async fn les_preselections_arrivent_meme_en_veille() {
        // Le garde arrete l'identity et le statut, pas un fait sur une source :
        // ce qu'une source contains ne depend pas de l'appareil etant allume.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::Power).await.unwrap();
        assert!(core.player_state().standby, "l'appareil dort");
        core.handle_source_update("radio", with_presets(vec![preset_of(3, "FIP")]));
        let cat = core.sources_catalog();
        let radio = cat.sources.iter().find(|s| s.name == "radio").unwrap();
        assert_eq!(radio.presets, vec![preset_of(3, "FIP")]);
    }

    // -- État partiel (`known`) et cover : tâche 5 -----------------------

    // -- CoverPayload embarquée, lue par le cœur : tâche 6 ----------------------

    /// C'est cette fonction, et non plus une relecture du code de `main`, qui
    /// prouve le partage exigé par la tâche 5 : le `Core` et l'`AppState` HTTP
    /// doivent recevoir **le même** `Arc<CoverCache>`. Un second
    /// `Arc::new(CoverCache::new())` glissé pour l'un des deux compilerait et
    /// laisserait passer tous les autres tests — y compris le test de route
    /// HTTP ci-dessus, qui construit son propre `AppState` à la main — mais
    /// romprait `Arc::ptr_eq` ici.
    #[test]
    fn le_coeur_et_lappstate_partagent_reellement_le_meme_arc() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let wiring = Wiring {
            sources: HashMap::new(),
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
            metadata: silent_wiring(vec![]),
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
        };
        let (cover_tx, _pochette_rx) = mpsc::channel::<(String, bool)>(4);
        let (app_state, core) = crate::assemble_covers_and_core(
            FakePlayer::default(),
            wiring,
            cover_tx,
            mpsc::channel(4).0,
            crate::status::tests_support::app_state(),
        );
        assert!(
            Arc::ptr_eq(core.app_covers(), &app_state.covers),
            "le coeur et l'AppState HTTP doivent partager le meme Arc<CoverCache>"
        );
    }

}
