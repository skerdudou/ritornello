use crate::metadata::{Metadonnees, PlayerState};
use crate::player::mpv;
use crate::player::Player;
use crate::state::{self, PersistedState, StartupPower};
use crate::types::Event;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::SourceUpdate;
use ritornello_proto::{
    Catalogue, Command, Enrichment, IdentityUpdate, InputMessage, NowPlaying, Overlay, Playback,
    Preset, SourceAction, SourceCatalogue, SourceReq,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, RwLock};

mod echeances;
mod lecteur;
mod commandes;
mod reglages;
mod sources;
mod metadonnees;
mod position;
pub use echeances::prochaine_echeance;

#[cfg(test)]
mod test_support;

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait Source: Send + Sync + 'static {
    async fn request(&self, req: SourceReq) -> Result<SourceAction>;
}

/// Ce que la boucle principale doit faire d'un événement du lecteur.
///
/// C'est le cœur qui décide quelles variantes attestent la vivacité du flux
/// (`StreamAlive`) : la boucle de `main`, qui tient l'échéance de relance,
/// suit ce verdict au lieu de dupliquer la liste des variantes — les deux
/// listes avaient déjà commencé à devoir être maintenues en parallèle.
#[derive(Debug, PartialEq, Eq)]
pub enum EventOutcome {
    /// Rien à faire côté temporisation.
    Nothing,
    /// Le flux est vivant : annuler toute relance programmée.
    StreamAlive,
    /// Programmer une relance du flux dans ce délai.
    RetryIn(Duration),
}

/// Tout ce que le cœur reçoit du montage de `main` : ses sources, son état
/// persisté, ses canaux de sortie.
///
/// Une structure nommée plutôt qu'une longue liste de paramètres positionnels :
/// à huit éléments, l'ordre d'un appel ne se vérifie plus à l'œil, et deux
/// `PathBuf` voisins (`state_path`, `locales_root`) s'échangeraient sans que le
/// compilateur y trouve à redire.
pub struct Cablage {
    pub sources: HashMap<String, Arc<dyn Source>>,
    pub persisted: PersistedState,
    pub state_path: PathBuf,
    pub catalog: Arc<RwLock<Catalog>>,
    pub locales_root: PathBuf,
    pub metadata: MetadataCablage,
    /// Le catalogue des sources vers les plugins Display, sur **son propre**
    /// canal. Pas dans `MetadataCablage` : il ne descend ni à la SPA ni aux
    /// plugins `metadata`, et surtout pas dans `etat` — un catalogue est
    /// structurel et rarement changeant, l'élargir ferait voyager les noms de
    /// 51 stations sur chacune des trames d'état par seconde de lecture.
    pub catalogue: watch::Sender<Catalogue>,
}

/// Câblage des métadonnées.
pub struct MetadataCablage {
    /// Noms des plugins `metadata`, **dans l'ordre de déclaration** de
    /// `plugins.toml` : cet ordre est la priorité d'arbitrage.
    pub plugins: Vec<String>,
    /// Ce qui joue, vers les plugins `metadata`. Un `watch` et non un appel
    /// direct : un plugin qui ne lit plus ne doit pas pouvoir figer le cœur.
    pub now_playing: watch::Sender<NowPlaying>,
    /// État du lecteur, vers la SPA (route `GET /api/player`) et vers les
    /// plugins Display : un seul canal d'état structuré pour les deux, chacun
    /// composant ce qu'il veut de la même trame.
    pub etat: watch::Sender<PlayerState>,
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
    /// not a re-read: `demarrage` runs after `new`, and by then `persist`
    /// may already have rewritten the file.
    veille_persistee: bool,
    expecting_stream: bool,
    /// Quelque chose est en lecture, **quelle qu'en soit la nature**.
    ///
    /// Distinct d'`expecting_stream`, qui ne dit plus que « ce qui joue est un
    /// flux live susceptible de tomber, donc à relancer ». Les deux
    /// coïncidaient tant que seuls des flux étaient concernés ; depuis qu'une
    /// Source peut déclarer un contenu fini (`Play { finite: true }`),
    /// `expecting_stream` est faux pendant la lecture d'un disque ou d'une
    /// liste de fichiers. S'en servir comme garde « ça joue » ferait taire
    /// toute couche de métadonnées sur exactement ces contenus-là.
    lecture: bool,
    /// La lecture en cours est **suspendue**. N'a de sens que quand `lecture`
    /// est vrai ; `etat_lecteur` ne le consulte pas autrement.
    ///
    /// Remis à faux **au seul endroit** où `lecture` passe à vrai. C'est la
    /// doctrine que `etat_lecteur` défend déjà pour `position_s` : un point
    /// unique ne peut pas être oublié, là où cinq effacements le seraient au
    /// sixième chemin ajouté.
    paused: bool,
    retry_count: u32,
    audio_device: Option<String>,
    /// Overlay temporaire (volume/muet/message) : incrustation à afficher +
    /// échéance. Porté par `PlayerState::overlay`, que le plugin d'affichage
    /// dessine en priorité sur toute autre chose.
    overlay: Option<(Overlay, Instant)>,
    /// Touche numérotée correspondant à ce qui joue, déclarée par la Source active
    /// (voir `SourceMessage::preset`). Oubliée dès que plus rien ne joue —
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
    /// `set_locale` — jamais au moment de poser la veille : le catalogue se
    /// lit derrière un verrou asynchrone, et `etat_lecteur` ne l'est pas. Le
    /// résoudre à la pose de la veille exigeait deux `await` faillibles avant
    /// de l'atteindre (`Command::Power`) : une Source ou mpv injoignables au
    /// premier passage en veille publiaient `standby: true` sans aucun statut,
    /// et l'écran devenait entièrement noir. Résolu en amont, le champ est
    /// toujours frais et ce piège d'ordonnancement disparaît. Gagne sur
    /// `source_status` dans `etat_lecteur` — l'appareil dort, ce que raconte
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
    /// Les présélections nommées **de chaque source**, indexées par nom de
    /// source, telles que chacune les a déclarées (`SourceMessage::presets`).
    ///
    /// À part de `preset_count`, et ce n'est pas une redondance : `preset_count`
    /// décrit la source **active** et s'oublie avec elle, alors qu'une table
    /// indexée par nom décrit *toutes* les sources en même temps. C'est ce
    /// qu'exige un client MPD, qui demande `listplaylistinfo "radio"` pendant
    /// que le cd joue. Rien ne l'oublie donc : ni la bascule de source, ni la
    /// veille.
    presets_par_source: HashMap<String, Vec<Preset>>,
    /// Remote tens offset in flight: `Plus10` presses accumulate here until
    /// a digit key consumes them (`+10` then `4` selects 14). Cleared by the
    /// overlay's own deadline (`expire_overlay`) or by its consumption
    /// (`Select`), and just as much by `appliquer_commande`'s abandon
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
    /// Métadonnées du morceau : identité de ce qui joue, titre ICY, et
    /// enrichissements des plugins. Voir `metadata.rs` pour l'arbitrage.
    metadonnees: Metadonnees,
    now_playing_tx: watch::Sender<NowPlaying>,
    etat_tx: watch::Sender<PlayerState>,
    /// Le catalogue des sources vers les afficheurs. Un canal séparé d'`etat_tx`
    /// et jamais publié par `publie_etat` : voir `publie_catalogue`.
    catalogue_tx: watch::Sender<Catalogue>,
    /// Behavior settings (hold-to-repeat timings, startup power state),
    /// persisted with the rest of the state.
    settings: crate::state::Settings,
    /// Hold-to-repeat pacing: instant before which a held volume command is
    /// ignored. Armed by a fresh volume step (now + initial delay), re-armed
    /// by each applied repeat (now + interval). `None` until a first press —
    /// a held event arriving out of nowhere (core restarted mid-hold) does
    /// nothing.
    volume_deadline: Option<Instant>,
    /// Où en est ce qui joue, en secondes entières, tel que le dernier
    /// rafraîchissement l'a établi. Publié tel quel par `etat_lecteur`.
    position_s: Option<u32>,
    /// Durée **mesurée par mpv**, distincte de celle qu'un plugin `metadata`
    /// annonce. Gardée à part parce qu'elle la supplante : les fondre en un
    /// seul champ ferait perdre la trace de qui a parlé, et la précédence
    /// deviendrait un ordre d'écriture — le genre d'invariant qui se casse en
    /// silence.
    duree_mesuree_s: Option<u32>,
    /// Position annoncée par un plugin `metadata`, et l'instant où elle est
    /// arrivée. Le cœur l'avance lui-même entre deux annonces — Radio France
    /// n'interroge le direct que toutes les quelques dizaines de secondes, et
    /// sans cette avance la barre resterait figée entre deux réponses.
    ancre_position: Option<(u32, Instant)>,
    /// Cache partagé avec le routeur : la tâche détachée y dépose, la route y
    /// lit. **Le même `Arc`** que celui remis à l'`AppState` HTTP — voir la
    /// note à son lieu de construction dans `main.rs` — sans quoi une
    /// pochette téléchargée par le cœur ne serait jamais lisible par la
    /// route.
    covers: Arc<crate::cover::CoverCache>,
    /// Résultats des récupérations détachées, consommés par la boucle de
    /// `main` (voir son bras `pochette_rx.recv()`). Le booléen dit si la
    /// récupération a abouti — nécessaire pour que `pochette_arrivee` libère
    /// `pochette_en_vol` même sur un échec, au lieu de laisser cette clé
    /// bloquée pour le reste du processus.
    pochette_tx: mpsc::Sender<(String, bool)>,
    /// Clé dont la récupération est en vol, pour ne pas la lancer deux fois.
    pochette_en_vol: Option<String>,
    /// Dernier chemin annoncé par mpv (`Event::Path`), retenu **seulement**
    /// pour comparaison à l'arrivée d'une extraction détachée — jamais
    /// interprété, comme le veut le principe posé pour `OBSERVEES`. Une
    /// extraction lancée pour un chemin peut revenir après que mpv soit
    /// passé à un autre : sans cette trace, son résultat s'installerait
    /// après coup sur la piste suivante.
    chemin_courant: Option<String>,
    /// Chemin dont l'extraction embarquée est actuellement en vol, pour ne
    /// pas en relancer une deuxième pendant que la première tourne encore
    /// sur ce même fichier.
    extraction_en_vol: Option<String>,
    /// Résultat d'une extraction détachée par `handle_path`, consommé par la
    /// boucle `select!` de `main` (voir `extraction_arrivee`). Symétrique de
    /// `pochette_tx` ci-dessus.
    extraction_tx: mpsc::Sender<(String, Option<ritornello_proto::CoverRef>)>,
    /// Disjoncteur qui borne l'appel `lofty`, strictement bloquant et
    /// potentiellement sur un partage réseau : voir `sante.rs` et le
    /// commentaire de `handle_path`.
    sante: Arc<crate::sante::Sante>,
}

/// Résout le mot de veille depuis un catalogue déjà en main.
///
/// Fonction libre plutôt que méthode : elle sert à la fois à la construction
/// (catalogue lu par `try_read`, avant que `self` n'existe) et à `set_locale`
/// (catalogue tout juste chargé, avant qu'il ne remplace celui du cœur), donc
/// aucune des deux n'a besoin de passer par le verrou asynchrone une seconde
/// fois.
fn resout_standby_status(catalog: &Catalog) -> String {
    catalog.get("standby").to_string()
}

impl<P: Player> Core<P> {
    pub fn new(
        player: P,
        cablage: Cablage,
        covers: Arc<crate::cover::CoverCache>,
        pochette_tx: mpsc::Sender<(String, bool)>,
        extraction_tx: mpsc::Sender<(String, Option<ritornello_proto::CoverRef>)>,
    ) -> Self {
        let Cablage { sources, persisted, state_path, catalog, locales_root, metadata, catalogue } =
            cablage;
        let mut source_order: Vec<String> = sources.keys().cloned().collect();
        source_order.sort();
        let active_source = if sources.contains_key(&persisted.active_source) {
            persisted.active_source.clone()
        } else {
            source_order.first().cloned().unwrap_or_default()
        };
        // Résolu tout de suite : le seul écrivain de ce catalogue est
        // `set_locale`, joignable uniquement depuis la boucle `select!` qui ne
        // démarre qu'après le retour d'ici — aucun verrou concurrent ne peut
        // donc exister à cet instant. Voir `resout_standby_status` pour la
        // raison de ce choix (plus jamais résolu au moment de poser la veille).
        //
        // L'échec est malgré tout journalisé plutôt qu'avalé : il rendrait
        // l'écran de veille entièrement vide jusqu'au prochain changement de
        // langue — précisément le défaut que ce pré-calcul corrige. Un
        // invariant qu'on croit tenu et que personne ne vérifie est ce qui a
        // produit ce défaut la première fois.
        let standby_status = match catalog.try_read() {
            Ok(cat) => Some(resout_standby_status(&cat)),
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
            // Reborné à la lecture : `state.json` peut avoir été édité à la
            // main, et un `volume: 255` partirait tel quel à mpv au réveil.
            volume: persisted.volume.min(100),
            muted: false,
            standby: false,
            veille_persistee: persisted.standby,
            expecting_stream: false,
            lecture: false,
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
            metadonnees: Metadonnees::new(metadata.plugins),
            now_playing_tx: metadata.now_playing,
            etat_tx: metadata.etat,
            catalogue_tx: catalogue,
            settings: persisted.settings.clone(),
            volume_deadline: None,
            position_s: None,
            duree_mesuree_s: None,
            ancre_position: None,
            covers,
            pochette_tx,
            pochette_en_vol: None,
            chemin_courant: None,
            extraction_en_vol: None,
            extraction_tx,
            sante: Arc::new(crate::sante::Sante::new()),
        };
        // Les sources câblées au démarrage sont déjà connues : sans cette
        // publication, le canal garderait son `Catalogue::default()` vide et un
        // afficheur relayé avant la première présélection croirait que
        // l'appareil n'a aucune source. `add_source` couvre la suite.
        coeur.publie_catalogue();
        // Les réglages persistés atteignent le cache de pochettes ici, et pas
        // seulement au premier `set_settings` : sans cette ligne, un appareil
        // dont `state.json` décoche le réencodage l'appliquerait à partir de la
        // première visite de la page de configuration, et pousserait des images
        // pleine taille jusque-là. Le démarrage doit obéir au fichier.
        coeur.covers.set_reglages(crate::cover::Reglages::from(&coeur.settings));
        coeur
    }

    /// Applique ce qu'une Source rapporte : son statut, et/ou l'identité de ce
    /// qu'elle joue désormais.
    ///
    /// Les deux arrivent dans la même trame et sont appliqués ensemble, sans
    /// affichage intermédiaire : aucun instant observable ne voit la ligne
    /// affichée décrire un morceau et l'identité annoncée aux plugins en décrire
    /// un autre.
    ///
    /// Deux sortes de trames arrivent par ce canal, et elles ne prennent pas le
    /// même chemin :
    ///
    /// - celles qui **recomposent la vue** — une réponse de Source, qui déclare
    ///   une identité ou un statut, ou un mot éphémère à incruster ;
    /// - celles qui **annoncent un fait** sans rien dire de ce qui joue — les
    ///   présélections nommées, leur nombre, le tiroir, la renumérotation de la
    ///   piste en cours, la pochette. Celles-là rendent la main avant le
    ///   traitement du statut : voir le retour anticipé.
    ///
    /// **En pratique, presque toute trame de production emprunte le second
    /// chemin dès qu'elle ne déclare ni identité ni statut** : le prédicat qui
    /// l'ouvre est une tautologie pour le SDK (voir le corps). Tout champ doit
    /// donc être appliqué **sur les deux chemins** — ce qui n'est appliqué qu'en
    /// bas de fonction n'est jamais appliqué. La déstructuration exhaustive en
    /// tête de fonction est ce qui rend cette décision obligatoire pour tout
    /// champ ajouté plus tard.
    pub fn handle_source_update(&mut self, name: &str, update: SourceUpdate) {
        // **Une trame d'une source que le cœur ne connaît plus est jetée, et
        // entière.** Le fan-out des requêtes de catalogue est détaché : un
        // `ListPresets` part dans sa propre tâche, et `remove_source` peut
        // s'exécuter entre la requête et la réponse — un greffon éteint depuis
        // l'IHM, ou mort de lui-même. Sans ce garde, la réponse encore en vol
        // ré-insérait la liste dans `presets_par_source` **après** l'éviction,
        // parce que cette insertion se fait délibérément avant le garde de
        // source active (le catalogue décrit toutes les sources, pas celle qui
        // joue). Le catalogue republié annonçait alors une liste enregistrée
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
        // sur le chemin du retour anticipé : le champ était gardé, donc la trame
        // passait, mais son application vivait tout en bas de la fonction, après
        // un `return` que cette trame prenait toujours. Chaque pochette de Source
        // était perdue **en silence**. Rien ne l'a signalé : le prédicat teste
        // les champs un par un, et `SourceUpdate` dérive `Default`, si bien qu'un
        // dixième champ ne casse aucun littéral et aucun test. Un commentaire
        // réclamant « pensez aux deux moitiés » se lit après coup ; une
        // déstructuration exhaustive, elle, ne peut pas être oubliée. Même
        // principe que l'annonce d'un greffon, qui ne peut pas mentir sur ses
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
        // Lu **avant** le garde ci-dessous, et c'est voulu : le catalogue décrit
        // toutes les sources, pas celle qui joue. Un client MPD interroge
        // `listplaylistinfo "radio"` pendant que le cd joue, et la veille ne
        // change rien à ce qu'une source contient. Le garde, lui, protège ce qui
        // décrit **ce qui joue** — identité, statut, message éphémère — et reste
        // en place pour tout le reste.
        let porte_des_presets = presets.is_some();
        if let Some(presets) = presets {
            self.presets_par_source.insert(name.to_string(), presets);
            self.publie_catalogue();
        }
        if self.standby || name != self.active_source {
            return;
        }
        // `preset_count` et `can_eject` décrivent la **source active** — combien
        // de présélections elle offre, si elle a quelque chose à éjecter — et
        // leur doc de champ les nomme déjà comme une paire, oubliée ensemble à la
        // bascule de source et à la veille. Appliqués ici, **avant** le retour
        // anticipé, pour que celui-ci ne puisse pas les avaler ; l'ordre
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
        // les deux chemins**. Une trame qui n'annonce qu'un fait prend le retour
        // anticipé de toute façon ; ce qui n'est appliqué qu'en bas de fonction
        // n'est donc jamais appliqué du tout. C'est exactement le défaut que la
        // fusion du chantier des pochettes a produit — `cover` gardé mais
        // appliqué seulement en bas, donc chaque pochette perdue en silence — et
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
        // joue (il doit garder son incrustation et désarmer un `+NN` en vol).
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
            // dans `applique_les_faits_declares`, appelée ici et une seule autre
            // fois en bas de fonction. Un champ ajouté là-dedans atterrit donc
            // sur les deux chemins par construction, au lieu de dépendre de
            // quelqu'un qui se souvienne de le recopier — c'est exactement
            // l'oubli qui a fait perdre chaque pochette de Source en silence.
            self.applique_les_faits_declares(preset, preset_name, cover, name);
            // Publier quand même : compte, tiroir et sélection font partie de
            // l'état diffusé, et le canal déduplique si rien n'a bougé.
            self.publie_etat();
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
            // Message éphémère (« présélection vide ») : il emprunte
            // l'emplacement et l'échéance de l'incrustation volume/muet, donc
            // `self.source_status` — le statut permanent — est conservé et
            // reparaît d'elle-même. Sans cela, le message restait à l'écran
            // indéfiniment alors que la lecture continuait sur la station
            // précédente : l'affichage décrivait durablement un état qui
            // n'existait plus. `overlay_ms`, pas `tens_window_ms` : ce message
            // n'a rien à voir avec le décalage `+NN` de la télécommande, seule
            // l'incrustation volume/muet partage son échéance avec lui.
            //
            // Un décalage `+NN` en cours perd donc son emplacement d'affichage
            // ici : le désarmer avec lui est ce qui évite qu'il survive
            // derrière un écran qui ne le montre plus (même raison que le
            // garde d'abandon d'`appliquer_commande`) — que la trame porte ou
            // non un mot à afficher.
            self.pending_tens = 0;
            if let Some(mot) = status {
                let echeance = Instant::now() + Duration::from_millis(self.settings.overlay_ms.into());
                self.overlay = Some((
                    Overlay::Message { text: mot, remaining_ms: self.settings.overlay_ms },
                    echeance,
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
        // Le second — et dernier — appelant de `applique_les_faits_declares`,
        // ici **après** l'identité : `set_identity(None)` efface la sélection, et
        // `set_identity` tout court remet à zéro tout ce que `Metadonnees`
        // retenait, pochette de la Source comprise. Une trame qui déclare
        // explicitement l'un ou l'autre doit gagner sur ce reset, donc elle est
        // appliquée derrière lui. C'est cet ordre-là qui interdit de remonter cet
        // appel avec `preset_count` ; le chemin du retour anticipé, lui, ne peut
        // pas porter d'identité par construction, donc l'y appeler est sûr.
        self.applique_les_faits_declares(preset, preset_name, cover, name);
        // `preset_count` et `can_eject` sont appliqués **en tête** de cette
        // fonction, avant le retour anticipé, pour la même raison.
        //
        // Toujours publier : la sélection courante fait partie de l'état
        // diffusé, et cet appel couvre la trame qui ne change ni identité ni
        // métadonnées (les autres chemins publient déjà, et le canal
        // déduplique). `etat_lecteur` porte toujours l'incrustation active
        // aux côtés du reste : une trame source arrivant pendant une
        // incrustation met donc à jour source_status/preset/preset_name sans
        // rien changer de ce que l'afficheur montre tant qu'elle dure.
        self.publie_etat();
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
    /// puisque une pochette de Source arrive toujours seule et prend toujours le
    /// retour anticipé. La déstructuration exhaustive en tête de fonction force
    /// la *question* (« à laquelle des deux moitiés ce champ appartient-il ? »)
    /// mais pas la *réponse* : deux appels côte à côte pouvaient toujours
    /// diverger. Avec un seul corps appelé aux deux sorties, la réponse est
    /// structurelle pour tout champ ajouté ici.
    ///
    /// La limite, dite franchement : rien n'empêche d'écrire un jour un nouveau
    /// champ *à côté* de cet appel plutôt que dedans. Ce qui est acquis, c'est
    /// qu'aucun champ déjà passé par ici ne peut manquer sur un chemin.
    fn applique_les_faits_declares(
        &mut self,
        preset: Option<u8>,
        preset_name: Option<String>,
        cover: Option<ritornello_proto::CoverRef>,
        name: &str,
    ) {
        self.applique_selection(preset, preset_name);
        self.applique_pochette_de_source(cover, name);
    }

    /// Diffuse l'état structuré du lecteur : à la SPA, et aux plugins Display
    /// (qui composent eux-mêmes leur mise en page depuis cette même trame).
    pub(crate) fn publie_etat(&self) {
        let etat = self.etat_lecteur();
        // Publié généreusement (à la fin de chaque commande, en plus des
        // chemins de métadonnées), donc dédupliqué : sans cette garde, chaque
        // navigateur connecté et chaque afficheur recevrait une trame
        // identique à la précédente.
        self.etat_tx.send_if_modified(|courant| {
            if *courant == etat {
                false
            } else {
                *courant = etat;
                true
            }
        });
        // `known` republié au même point de passage que l'état structuré :
        // c'est ici que tout chemin qui vient d'ajouter ou de corriger une
        // information de métadonnées (ICY, tags, enrichissement, pochette)
        // finit par converger, et c'est ce qui permet à un plugin `metadata`
        // câblé à chaud — ou simplement lent à répondre — de voir ce qui est
        // déjà connu sans attendre un hypothétique prochain changement
        // d'identité, qui peut ne jamais survenir tant que le même morceau
        // joue. `set_identity` construit lui-même son `NowPlaying` (source et
        // identité en changent aussi) ; ce `send_if_modified` ne fait alors
        // que constater l'égalité et ne republie rien en trop.
        let known = self.metadonnees.known();
        self.now_playing_tx.send_if_modified(|np| {
            if np.known == known {
                false
            } else {
                np.known = known;
                true
            }
        });
    }

    /// Ce qui est structurel : les sources déclarées, **dans l'ordre de
    /// bascule** de `SourceCycle`, et les présélections nommées de chacune
    /// quand elle sait les énumérer.
    ///
    /// L'ordre vient de `source_order` et non des clés de la table : c'est
    /// l'ordre que les clients verront dans `listplaylists`, et il doit être
    /// celui de la touche `SourceCycle` — sinon la liste et la touche
    /// divergent. Une source qui n'énumère pas figure quand même, avec une
    /// liste vide : elle existe, et le consommateur retombe sur `preset_count`.
    pub fn catalogue(&self) -> Catalogue {
        Catalogue {
            sources: self
                .source_order
                .iter()
                .map(|nom| SourceCatalogue {
                    name: nom.clone(),
                    presets: self.presets_par_source.get(nom).cloned().unwrap_or_default(),
                })
                .collect(),
        }
    }

    /// Diffuse le catalogue vers les afficheurs. Jumeau de `publie_etat`, sur
    /// **son propre** canal.
    ///
    /// Appelé là où le catalogue peut changer, et seulement là : à la
    /// construction du cœur (les sources du démarrage), à l'arrivée de
    /// présélections, à `add_source` (une source câblée à chaud apparaît dans la
    /// liste) et à `remove_source` (un greffon éteint en disparaît, sans quoi un
    /// client MPD garderait une liste enregistrée sur laquelle agir). Jamais
    /// depuis `publie_etat`, et `publie_etat` jamais depuis
    /// ici : les deux canaux sont séparés précisément pour ne pas se déclencher
    /// l'un l'autre — sinon les noms de 51 stations repartiraient sur chaque
    /// trame par seconde de lecture, et la déduplication par égalité ne
    /// rattraperait rien puisque les deux valeurs changeraient ensemble.
    ///
    /// Même déduplication que l'état, pour la même raison : une source qui
    /// réannonce la même liste — la radio le fait à chaque enregistrement de sa
    /// page d'admin — ne doit pas réveiller les afficheurs.
    pub(crate) fn publie_catalogue(&self) {
        let catalogue = self.catalogue();
        self.catalogue_tx.send_if_modified(|courant| {
            if *courant == catalogue {
                false
            } else {
                *courant = catalogue;
                true
            }
        });
    }

    /// État complet du lecteur : ce qui est volatil, donc ce que la SPA reçoit
    /// en flux poussé.
    pub fn etat_lecteur(&self) -> PlayerState {
        PlayerState {
            source: self.active_source.clone(),
            volume: self.volume,
            muted: self.muted,
            standby: self.standby,
            preset: self.preset,
            preset_name: self.preset_name.clone(),
            preset_count: self.preset_count,
            // La veille gagne sur le statut de la source : l'appareil dort, ce
            // que raconte la source n'a plus cours.
            status: if self.standby { self.standby_status.clone() } else { self.source_status.clone() },
            overlay: self.overlay.as_ref().map(|(o, echeance)| {
                let restant = echeance.saturating_duration_since(Instant::now()).as_millis();
                // Le `remaining_ms` mémorisé n'est jamais lu : il est réécrit
                // ici à chaque publication. L'égalité d'`Overlay` l'ignore,
                // donc ce rafraîchissement ne défait pas la déduplication des
                // trames.
                o.clone().avec_restant(u32::try_from(restant).unwrap_or(u32::MAX))
            }),
            // Gardée **ici**, à la publication, et non effacée dans chacun des
            // cinq chemins qui posent `lecture = false` (arrêt, veille,
            // changement de source, fin de contenu, `SourceAction::Stop`).
            // Un point unique ne peut pas être oublié ; cinq appels
            // sprinkled le seraient au sixième chemin ajouté, et la barre
            // resterait figée sur la dernière valeur connue sans que rien ne
            // le signale.
            position_s: if self.lecture && !self.standby { self.position_s } else { None },
            // Même raison qu'au-dessus : calculé à la publication plutôt
            // qu'entretenu dans les cinq chemins qui posent `lecture = false`.
            playback: if !self.lecture || self.standby {
                Playback::Stopped
            } else if self.paused {
                Playback::Paused
            } else {
                Playback::Playing
            },
            // `lecture` et non `expecting_stream` : la première dit « quelque
            // chose joue », la seconde « c'est un flux relançable ». Un
            // contenu déplaçable est exactement ce qui joue sans être un flux.
            seekable: self.lecture && !self.standby && !self.expecting_stream,
            // Rien à voir avec ce qui joue : un tiroir vide s'ouvre quand
            // même, et c'est la Source qui a le tiroir. La veille est le seul
            // état qui l'annule, parce qu'elle n'y laisse passer aucune
            // commande.
            can_eject: self.can_eject && !self.standby,
            morceau: {
                let mut m = self.metadonnees.etat();
                // Précédence : la durée mesurée par mpv l'emporte sur celle
                // qu'un plugin annonce. `origin` continue de désigner qui a
                // fourni le **morceau** (artiste, titre, album) et non qui a
                // fourni la durée — imprécision assumée plutôt qu'un second
                // champ d'origine pour une seule valeur numérique.
                if self.lecture && !self.standby && self.duree_mesuree_s.is_some() {
                    m.duration_s = self.duree_mesuree_s;
                }
                m
            },
        }
    }

}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[tokio::test]
    async fn la_veille_ignore_les_mises_a_jour_de_la_source_et_le_reveil_les_reprend() {
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(etat_rx.borrow_and_update().standby);
        let mut update = update_nu();
        update.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update.clone());
        assert_eq!(etat_rx.borrow().preset_name, None, "en veille, la trame source est ignoree");
        core.handle_command(Command::Power).await.unwrap();
        core.handle_source_update("radio", update);
        assert_eq!(
            etat_rx.borrow_and_update().preset_name.as_deref(),
            Some("FIP"),
            "le reveil laisse la source reprendre la main"
        );
    }

    #[tokio::test]
    async fn mise_a_jour_dune_source_inactive_est_ignoree() {
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update_cd = update_nu();
        update_cd.preset_name = Some("CD".into());
        core.handle_source_update("cd", update_cd);
        assert_eq!(etat_rx.borrow().preset_name, None, "la mise a jour de \"cd\" (inactive) n'a pas ete appliquee");
        let mut update_radio = update_nu();
        update_radio.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update_radio);
        assert_eq!(etat_rx.borrow_and_update().preset_name.as_deref(), Some("FIP"));
    }

    #[tokio::test]
    async fn le_statut_de_la_source_ne_survit_pas_a_la_mise_en_veille() {
        // Second scénario d'I2 : sans effacement explicite, `source_status`
        // restait en mémoire pendant la veille (masqué par la priorité du mot
        // de veille dans `etat_lecteur`) et reparaissait au réveil tant que la
        // Source n'avait pas reparlé — un mensonge prêt à resurgir.
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update = update_nu();
        update.status = Some("pas de disque".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("pas de disque"));
        core.handle_command(Command::Power).await.unwrap(); // veille
        core.handle_command(Command::Power).await.unwrap(); // reveil, source muette
        assert_eq!(
            etat_rx.borrow_and_update().status,
            None,
            "le statut de l'ancienne trame ne doit pas reapparaitre au reveil avant que la Source n'ait reparle"
        );
    }

    #[tokio::test]
    async fn le_compte_de_preselections_est_memorise_et_publie() {
        // Une trame qui déclare un compte doit se retrouver dans PlayerState ;
        // une trame muette sur le sujet ne doit pas l'effacer.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("radio", update_avec_compte(Some(23)));
        assert_eq!(etat_rx.borrow().preset_count, Some(23));
        core.handle_source_update("radio", update_avec_compte(None));
        assert_eq!(etat_rx.borrow().preset_count, Some(23));
        // Some(0) écrase : le cd sans disque dit « rien à numéroter ».
        core.handle_source_update("radio", update_avec_compte(Some(0)));
        assert_eq!(etat_rx.borrow().preset_count, Some(0));
    }

    #[tokio::test]
    async fn la_capacite_dejection_est_memorisee_et_publiee() {
        // Fausse par défaut : ne pas savoir, c'est n'offrir rien — la
        // télécommande web grise sa touche Eject tant que personne ne l'a
        // réclamée. Une trame muette sur le sujet ne l'efface pas.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        assert!(!etat_rx.borrow().can_eject, "rien de declare : rien d'offert");
        core.handle_source_update("radio", update_avec_ejection(Some(true)));
        assert!(etat_rx.borrow().can_eject);
        core.handle_source_update("radio", update_avec_ejection(None));
        assert!(etat_rx.borrow().can_eject, "une trame muette ne retire pas la capacite");
        core.handle_source_update("radio", update_avec_ejection(Some(false)));
        assert!(!etat_rx.borrow().can_eject);
    }

    #[tokio::test]
    async fn lejection_survit_a_larret_mais_ni_au_changement_de_source_ni_a_la_veille() {
        // Même calendrier d'oubli que `preset_count`, et pour la même raison :
        // la capacité décrit la Source, pas ce qui joue. Un arrêt ne change
        // pas le fait que le lecteur a un tiroir ; changer de source, si.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("radio", update_avec_ejection(Some(true)));
        core.handle_command(Command::Stop).await.unwrap();
        assert!(etat_rx.borrow().can_eject, "un tiroir ne disparait pas a l'arret");
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(!etat_rx.borrow().can_eject, "la capacite decrit la source qui s'en va");
    }

    #[tokio::test]
    async fn la_veille_retire_la_capacite_dejection() {
        // La veille ne laisse passer aucune commande (`handle_command`) : offrir
        // Eject y serait un mensonge de plus. Un cœur neuf par test — après
        // `SourceCycle`, plus rien ne garantit que « radio » soit encore la
        // source active, donc plus rien ne garantit qu'une trame la concernant
        // franchisse le garde-fou de `handle_source_update`.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("radio", update_avec_ejection(Some(true)));
        assert!(etat_rx.borrow().can_eject);
        core.handle_command(Command::Power).await.unwrap();
        assert!(!etat_rx.borrow().can_eject);
    }

    #[tokio::test]
    async fn le_compte_survit_a_larret_mais_pas_au_changement_de_source() {
        // Stop efface preset (plus rien ne joue) mais pas le compte : une radio
        // arrêtée a toujours ses stations.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("radio", update_avec_compte(Some(23)));
        assert_eq!(etat_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(etat_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(etat_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn une_mise_a_jour_de_seul_compte_laisse_morceau_et_identite_intacts() {
        // Garantie de sûreté dont dépend l'annonce spontanée de `preset_count`
        // par la radio après un enregistrement réussi côté admin (voir
        // `RadioSource::poll_notification`) : une trame qui ne porte que le
        // compte doit laisser le morceau en cours et l'identité intacts, et
        // tout de même publier l'état. Rien ne le vérifiait avant ce test.
        let (mut core, mut np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let id = serde_json::json!({"kind": "stream", "url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        // Repère pris après l'installation de l'identité : seuls les
        // changements ultérieurs doivent être détectés.
        np_rx.borrow_and_update();
        let morceau_avant = etat_rx.borrow().morceau.clone();

        core.handle_source_update("radio", update_avec_compte(Some(5)));

        assert_eq!(etat_rx.borrow().preset_count, Some(5), "le compte doit etre publie");
        assert_eq!(etat_rx.borrow().morceau, morceau_avant, "le morceau ne doit pas bouger");
        assert!(!np_rx.has_changed().unwrap(), "l'identite ne doit pas bouger");
        assert_eq!(np_rx.borrow().identity, Some(id));
    }

    #[tokio::test]
    async fn une_mise_a_jour_de_seul_nom_laisse_morceau_et_identite_intacts() {
        // Même garantie que pour `preset_count` ci-dessus, cette fois pour
        // `preset_name` : une trame qui ne porte que le nom doit se fondre
        // dans l'état publié sans rien déranger d'autre.
        let (mut core, mut np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let id = serde_json::json!({"kind": "stream", "url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        np_rx.borrow_and_update();
        let morceau_avant = etat_rx.borrow().morceau.clone();

        core.handle_source_update("radio", update_avec_nom(Some("FIP")));

        assert_eq!(etat_rx.borrow().preset_name.as_deref(), Some("FIP"), "le nom doit etre publie");
        assert_eq!(etat_rx.borrow().morceau, morceau_avant, "le morceau ne doit pas bouger");
        assert!(!np_rx.has_changed().unwrap(), "l'identite ne doit pas bouger");
        assert_eq!(np_rx.borrow().identity, Some(id));
    }

    #[tokio::test]
    async fn le_compte_est_oublie_en_veille() {
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("radio", update_avec_compte(Some(23)));
        assert_eq!(etat_rx.borrow().preset_count, Some(23));
        core.handle_command(Command::Power).await.unwrap(); // entre en veille
        assert_eq!(etat_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn letat_du_lecteur_diffuse_volume_muet_veille_et_source() {
        // Le volume n'est expose par aucune route : sa place est ce canal
        // pousse, avec le reste de ce qui est volatil. Une branche de
        // `handle_command` qui oublierait de publier laisserait l'IHM afficher
        // un etat perime sans que rien ne le signale — d'ou la publication a la
        // sortie de **toute** commande, et d'ou ce test qui les parcourt.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        let initial = etat_rx.borrow().clone();
        assert_eq!(initial.volume, 60, "le volume persiste doit etre connu des le demarrage");
        assert_eq!(initial.source, "radio");
        assert!(!initial.muted);
        assert!(!initial.standby);

        core.handle_command(Command::VolumeUp).await.unwrap();
        assert_eq!(etat_rx.borrow().volume, 65);
        core.handle_command(Command::VolumeDown).await.unwrap();
        assert_eq!(etat_rx.borrow().volume, 60);

        core.handle_command(Command::Mute).await.unwrap();
        assert!(etat_rx.borrow().muted);
        core.handle_command(Command::Mute).await.unwrap();
        assert!(!etat_rx.borrow().muted);

        core.handle_command(Command::Power).await.unwrap();
        assert!(etat_rx.borrow().standby, "la veille doit se voir dans l'IHM");
        core.handle_command(Command::Power).await.unwrap();
        assert!(!etat_rx.borrow().standby);
    }

    #[tokio::test]
    async fn le_morceau_est_aplati_dans_le_json_de_letat() {
        // L'IHM recoit un objet plat : un seul encart, pas deux niveaux a
        // distinguer.
        let (mut core, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        let json = serde_json::to_value(core.etat_lecteur()).unwrap();
        assert_eq!(json["source"], "radio");
        assert_eq!(json["volume"], 60);
        assert_eq!(json["artist"], "Miles Davis", "aplati, pas sous `morceau`");
        assert_eq!(json["title"], "So What");
        assert_eq!(json["origin"], "ouifm");
    }

    #[tokio::test]
    async fn un_message_ephemere_seffece_et_laisse_reparaitre_letat_precedent() {
        // Cas reel : selectionner une preselection vide. Rien n'est lance, la
        // station precedente joue toujours — le message doit donc passer, puis
        // ceder la place, sans que le statut permanent ni les metadonnees bougent.
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        let mut permanent = update_nu();
        permanent.status = Some("FIP".into());
        core.handle_source_update("radio", permanent);
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("FIP"));

        let mut ephemere = update_nu();
        ephemere.transient = true;
        // Le mot affiché vient de `status`, jamais d'une vue composée (voir
        // Task 3) : c'est ainsi que le plugin radio le déclare réellement sur
        // la branche « présélection vide ».
        ephemere.status = Some("empty preset".into());
        core.handle_source_update("radio", ephemere);
        let pendant = etat_rx.borrow_and_update().clone();
        assert!(matches!(pendant.overlay, Some(Overlay::Message { .. })), "le message doit s'afficher");
        assert_eq!(pendant.status.as_deref(), Some("FIP"), "le statut permanent n'a pas bouge");
        assert!(core.overlay_deadline().is_some(), "et porter une echeance");

        core.expire_overlay();
        let apres = etat_rx.borrow_and_update().clone();
        assert!(apres.overlay.is_none());
        assert_eq!(apres.status.as_deref(), Some("FIP"), "la station qui joue doit reparaitre");
        assert_eq!(apres.morceau.title.as_deref(), Some("So What"), "les metadonnees aussi");
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
        let mut update = update_nu();
        update.status = Some("PAS DE DISQUE".into());
        core.handle_source_update("radio", update);
        assert_eq!(core.etat_lecteur().status.as_deref(), Some("PAS DE DISQUE"));

        core.handle_source_update("radio", update_nu());
        assert_eq!(core.etat_lecteur().status, None, "absent vaut effacé, pas conservé");
    }

    #[tokio::test]
    async fn un_statut_ephemere_ne_touche_pas_au_statut_memorise() {
        // Le cas « présélection vide » : un mot passager, alors que la station
        // précédente continue de jouer. Il alimente l'incrustation, et le statut
        // permanent doit reparaître à l'échéance.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = update_nu();
        permanent.status = Some("FIP".into());
        core.handle_source_update("radio", permanent);

        let mut ephemere = update_nu();
        ephemere.status = Some("PRESELECTION VIDE".into());
        ephemere.transient = true;
        core.handle_source_update("radio", ephemere);
        assert_eq!(
            core.etat_lecteur().status.as_deref(),
            Some("FIP"),
            "le statut permanent survit à un message éphémère"
        );
        assert!(matches!(core.etat_lecteur().overlay, Some(Overlay::Message { .. })));

        core.expire_overlay();
        assert_eq!(core.etat_lecteur().status.as_deref(), Some("FIP"));
        assert!(core.etat_lecteur().overlay.is_none());
    }

    #[tokio::test]
    async fn un_compte_seul_neffece_pas_le_statut_de_la_source() {
        // Le defaut etait **en service** : `plugin-files` annonce un compte sans
        // statut quand sa page d'admin enregistre une liste, alors qu'il declare
        // un statut permanent partout ailleurs. Le statut disparaissait donc de
        // la console et de la SPA jusqu'a la commande suivante.
        //
        // `preset_count` est dans le predicat de trame interessante du SDK depuis
        // toujours : cette trame **arrive** au coeur, et le traitement du statut
        // l'aurait effacee faute d'en porter un.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = update_nu();
        permanent.status = Some("6 FICHIERS".into());
        core.handle_source_update("radio", permanent);
        assert_eq!(core.etat_lecteur().status.as_deref(), Some("6 FICHIERS"));

        let mut compte = update_nu();
        compte.preset_count = Some(6);
        core.handle_source_update("radio", compte);
        assert_eq!(
            core.etat_lecteur().status.as_deref(),
            Some("6 FICHIERS"),
            "une trame qui ne declare ni identite ni statut n'a rien a dire du statut"
        );
        assert_eq!(
            core.etat_lecteur().preset_count,
            Some(6),
            "et le compte doit quand meme etre pris : le retour anticipe est apres lui"
        );
    }

    #[tokio::test]
    async fn un_avis_de_renumerotation_neffece_pas_le_statut() {
        // La trame exacte de `plugin-files` apres un enregistrement depuis sa
        // page d'admin : le compte, **et** le numero et le nom de la piste
        // courante, sans identite (la piste ne doit pas etre redeclaree) ni
        // statut. Trois champs de fusion, aucune recomposition de vue.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = update_nu();
        permanent.status = Some("6 FICHIERS".into());
        core.handle_source_update("radio", permanent);

        let mut avis = update_nu();
        avis.preset_count = Some(9);
        avis.preset = Some(3);
        avis.preset_name = Some("Kind of Blue".into());
        core.handle_source_update("radio", avis);
        let etat = core.etat_lecteur();
        assert_eq!(etat.status.as_deref(), Some("6 FICHIERS"), "le statut permanent survit");
        assert_eq!(etat.preset_count, Some(9));
        assert_eq!(etat.preset, Some(3));
        assert_eq!(etat.preset_name.as_deref(), Some("Kind of Blue"));
    }

    #[tokio::test]
    async fn des_preselections_seules_neffacent_pas_le_statut() {
        // Le second producteur du meme piege : la reponse a `ListPresets` ne
        // porte ni identite ni statut. Sans le retour anticipe, demander son
        // catalogue a une source blanchirait son statut.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut permanent = update_nu();
        permanent.status = Some("PAS DE DISQUE".into());
        core.handle_source_update("radio", permanent);

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert_eq!(
            core.etat_lecteur().status.as_deref(),
            Some("PAS DE DISQUE"),
            "demander le catalogue ne doit pas effacer l'ecran"
        );
        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string(), "radio".into()]);
    }

    #[tokio::test]
    async fn les_preselections_dune_source_inactive_sont_gardees() {
        // La raison d'etre du contournement du garde : `listplaylistinfo "radio"`
        // s'interroge pendant que le cd joue.
        let (mut core, _pc, _sc, _rx, _d) =
            setup_persiste(PersistedState { active_source: "cd".into(), ..Default::default() });
        assert_eq!(core.active_source(), "cd");
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(5, "OUI FM")]));
        let cat = core.catalogue();
        let radio = cat.sources.iter().find(|s| s.name == "radio").expect("radio est declaree");
        assert_eq!(radio.presets, vec![pres(1, "FIP"), pres(5, "OUI FM")]);
        let cd = cat.sources.iter().find(|s| s.name == "cd").expect("cd est declaree");
        assert!(cd.presets.is_empty(), "le cd n'enumere rien, il figure quand meme");
    }

    #[tokio::test]
    async fn les_preselections_arrivent_meme_en_veille() {
        // Le garde arrete l'identite et le statut, pas un fait sur une source :
        // ce qu'une source contient ne depend pas de l'appareil etant allume.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::Power).await.unwrap();
        assert!(core.etat_lecteur().standby, "l'appareil dort");
        core.handle_source_update("radio", avec_presets(vec![pres(3, "FIP")]));
        let cat = core.catalogue();
        let radio = cat.sources.iter().find(|s| s.name == "radio").unwrap();
        assert_eq!(radio.presets, vec![pres(3, "FIP")]);
    }

    #[tokio::test]
    async fn le_catalogue_suit_lordre_de_bascule_des_sources() {
        // C'est l'ordre que les clients verront dans `listplaylists`, et il doit
        // etre celui de `SourceCycle` : sinon la liste et la touche divergent.
        //
        // Compare a l'ordre **observe** en pressant la touche, et non a
        // `source_order` : comparer le catalogue au champ dont il est construit
        // ne prouverait rien.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.add_source("files".into(), Arc::new(FakeSource { name: "files", calls: source_calls }));
        let attendu = noms(&core.catalogue());
        assert_eq!(attendu.len(), 3);

        core.handle_command(Command::SelectSource(attendu[0].clone())).await.unwrap();
        let mut tour = vec![core.active_source().to_string()];
        for _ in 1..attendu.len() {
            core.handle_command(Command::SourceCycle).await.unwrap();
            tour.push(core.active_source().to_string());
        }
        assert_eq!(attendu, tour, "le catalogue doit enumerer dans le sens de la touche");
    }

    #[tokio::test]
    async fn le_catalogue_porte_les_sources_du_demarrage_sans_attendre_une_preselection() {
        // Les sources cablees au rendez-vous sont connues des la construction :
        // c'est `Core::new` qui publie, et sans cette publication le canal
        // garderait son `Catalogue::default()` vide. Un afficheur relaye avant la
        // premiere preselection — donc avant tout changement — lirait alors
        // « aucune source », et un client MPD repondrait un `listplaylists` vide.
        //
        // Assere la valeur **courante** du canal, celle que le relais envoie a la
        // connexion, et non un changement : c'est exactement ce que voit un
        // afficheur qui arrive.
        let (core, _pc, _sc, _rx, _d) = setup();
        let cat_rx = core.catalogue_tx.subscribe();
        assert_eq!(
            noms(&cat_rx.borrow()),
            vec!["cd".to_string(), "radio".into()],
            "le catalogue doit porter les sources du demarrage des la construction"
        );
    }

    #[tokio::test]
    async fn le_catalogue_ne_republie_pas_pour_une_liste_identique() {
        // Meme deduplication que l'etat : une source qui reannonce la meme liste
        // — la radio le fait a chaque enregistrement de sa page d'admin — ne doit
        // pas reveiller les afficheurs.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut cat_rx = core.catalogue_tx.subscribe();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(cat_rx.has_changed().unwrap(), "la premiere liste, elle, est une nouvelle");
        let _ = cat_rx.borrow_and_update();

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(!cat_rx.has_changed().unwrap(), "la meme liste ne doit rien reveiller");

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP 2")]));
        assert!(cat_rx.has_changed().unwrap(), "une liste differente, si");
    }

    #[tokio::test]
    async fn publier_letat_ne_republie_pas_le_catalogue() {
        // La propriete des deux canaux separes. Sans elle, 51 noms de station
        // voyageraient sur chaque trame par seconde de lecture.
        //
        // Ce qui est assere est **la notification**, pas l'absence d'appel : un
        // couplage qui passerait par `publie_catalogue` serait dedoublonne, donc
        // n'atteindrait aucun afficheur, donc ne casserait pas la propriete. Un
        // `catalogue_tx.send(...)` depuis `publie_etat` — l'ecriture naturelle du
        // couplage — la casse, et ce test tombe.
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        let cat_rx = core.catalogue_tx.subscribe();
        let vu = cat_rx.borrow().clone();
        let _ = etat_rx.borrow_and_update();

        core.handle_command(Command::VolumeUp).await.unwrap();
        core.publie_etat();
        assert!(etat_rx.has_changed().unwrap(), "l'etat, lui, a bien bouge");
        assert!(!cat_rx.has_changed().unwrap(), "le catalogue a bouge pour rien");
        assert_eq!(*cat_rx.borrow(), vu, "et il porte toujours la meme chose");
    }

    #[tokio::test]
    async fn la_veille_gagne_sur_le_statut_de_la_source() {
        // L'appareil dort : ce que raconte la source n'a plus cours, même si
        // elle continue (en pratique elle ne le fait pas) à en déclarer un.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut update = update_nu();
        update.status = Some("FIP".into());
        core.handle_source_update("radio", update);
        assert_eq!(core.etat_lecteur().status.as_deref(), Some("FIP"));

        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(
            core.etat_lecteur().status.as_deref(),
            Some("STANDBY"),
            "le mot de veille gagne sur le statut mémorisé de la source"
        );

        // Révision I2 (revue de branche) : ce test affirmait auparavant que le
        // réveil rendait la main au statut mémorisé ("FIP"), inchangé tant que
        // la Source n'en redéclarait pas un nouveau. C'était exactement le
        // bogue signalé par la revue — le statut d'une source pouvait survivre
        // à la veille et réapparaître sous une source qui n'a encore rien dit
        // (voir `le_statut_de_la_source_ne_survit_pas_a_la_mise_en_veille`).
        // La veille l'oublie désormais, comme `preset_count`.
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(
            core.etat_lecteur().status,
            None,
            "le réveil ne doit pas faire réapparaître un statut que la source n'a pas redéclaré"
        );
    }

    // -- État partiel (`known`) et pochette : tâche 5 -----------------------

    // -- Pochette embarquée, lue par le cœur : tâche 6 ----------------------

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
        let cablage = Cablage {
            sources: HashMap::new(),
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
            metadata: cablage_muet(vec![]),
            catalogue: watch::channel(Catalogue::default()).0,
        };
        let (pochette_tx, _pochette_rx) = mpsc::channel::<(String, bool)>(4);
        let (app_state, core) = crate::assemble_covers_et_core(
            FakePlayer::default(),
            cablage,
            pochette_tx,
            mpsc::channel(4).0,
            crate::status::tests_support::app_state(),
        );
        assert!(
            Arc::ptr_eq(core.app_covers(), &app_state.covers),
            "le coeur et l'AppState HTTP doivent partager le meme Arc<CoverCache>"
        );
    }

}
