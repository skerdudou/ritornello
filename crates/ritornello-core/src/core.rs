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

    pub async fn resume(&mut self) -> Result<()> {
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        if let Some(locale) = self.locale.clone() {
            for name in self.source_order.clone() {
                if let Some(src) = self.sources.get(&name) {
                    if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                        tracing::warn!("SetLocale to {name}: {e}");
                    }
                }
            }
        }
        if let Some(action) = self.demande_active(SourceReq::Wake).await? {
            self.apply(action).await?;
        }
        // L'IHM doit connaître le volume et la source dès le premier affichage,
        // sans attendre qu'on touche à quelque chose.
        self.publie_etat();
        Ok(())
    }

    /// Rejoue le contenu courant de la source active (`Activate` demande à la
    /// source de redonner l'URI en cours, pas de passer au contenu suivant).
    pub async fn retry_stream(&mut self) -> Result<()> {
        if !self.standby && self.expecting_stream {
            if let Some(action) = self.demande_active(SourceReq::Activate).await? {
                self.apply(action).await?;
            }
        }
        Ok(())
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

    /// Applique la sélection qu'une trame déclare : le numéro de présélection et
    /// son nom lisible. Convention « absent = garder la valeur courante », à
    /// l'inverse de `status`.
    ///
    /// Appelée par `applique_les_faits_declares` seule, qui la relaie aux deux
    /// sorties de `handle_source_update` : la trame qui recompose la vue
    /// l'applique **après** l'identité (`set_identity(None)` efface la sélection,
    /// une déclaration explicite doit gagner), celle qui ne fait qu'annoncer un
    /// fait l'applique avant de rendre la main. Deux copies de ces quatre lignes
    /// divergeraient.
    fn applique_selection(&mut self, preset: Option<u8>, nom: Option<String>) {
        if let Some(p) = preset {
            self.preset = Some(p);
        }
        if let Some(n) = nom {
            self.preset_name = Some(n);
        }
    }

    /// Applique la pochette qu'une trame de Source déclare.
    ///
    /// La pochette suit la même convention que `preset`/`preset_count` :
    /// absente = rien de neuf, jamais « plus de pochette » — une Source n'en
    /// répète pas la déclaration sur chaque trame de statut qui suit (voir
    /// `SourceUpdate::cover`). C'est pourquoi `set_cover_de_source` ne doit être
    /// appelée que lorsque le champ vaut `Some`.
    ///
    /// **Appelée par `applique_les_faits_declares`**, exactement comme
    /// `applique_selection` et pour la même raison — c'est elle qui la relaie aux
    /// deux sorties de `handle_source_update`. Sur le chemin qui recompose
    /// la vue, l'appel vient **après** l'identité : `set_identity` remet à zéro
    /// tout ce que `Metadonnees` retenait, pochette de la Source comprise, donc
    /// une trame qui porterait à la fois une nouvelle identité et sa pochette
    /// doit laisser l'identité parler d'abord — sans quoi la pochette tout juste
    /// déclarée serait effacée dans la foulée par ce reset. C'est exactement le
    /// piège que le commentaire d'`applique_selection`, plus haut, signale déjà
    /// pour la sélection.
    ///
    /// Sur le chemin du retour anticipé, il n'y a par construction ni identité ni
    /// statut, donc l'ordre n'y a pas de sens. **C'est celui-là qui compte** : une
    /// pochette de Source arrive seule, en notification spontanée, donc elle passe
    /// par là presque toujours — le chemin qui recompose la vue ne sert qu'à la
    /// trame qui porterait une pochette *en même temps* qu'une identité ou un
    /// statut. Sans l'appel du retour anticipé, la pochette n'est pas « appliquée
    /// plus tard » : elle est perdue en silence, et c'est le défaut réel que la
    /// fusion du chantier des pochettes avait introduit.
    ///
    /// Appelée depuis `handle_source_update` et non depuis la boucle `select!`
    /// de `main` : la garde de tête (`standby || name != self.active_source`)
    /// doit s'appliquer à la pochette comme à tout le reste de la trame. Une
    /// source non active pourrait sinon faire apparaître sa pochette sur le
    /// morceau que joue la source **active**.
    ///
    /// `validee` ici, comme `Enrichment::cleaned` le fait sur l'autre canal :
    /// une pochette entre dans le cœur par deux portes, et la couche
    /// `ritornello-proto` — celle qui possède la validation de forme — ne
    /// gardait que l'une des deux. Rien n'était exploitable, les contrôles
    /// propres du cœur couvrant ce chemin, mais une règle de forme appliquée à
    /// une porte sur deux finit par diverger. Une référence refusée vaut « rien
    /// de neuf », jamais « plus de pochette » : c'est la convention du champ
    /// (voir `SourceUpdate::cover`), et effacer sur une trame mal formée
    /// retirerait l'image valide qu'une trame précédente avait déclarée.
    fn applique_pochette_de_source(
        &mut self,
        cover: Option<ritornello_proto::CoverRef>,
        name: &str,
    ) {
        if let Some(cover) = cover.and_then(ritornello_proto::CoverRef::validee) {
            self.set_cover_de_source(Some(cover), name);
        }
    }

    /// Change ce qui joue : remet l'ardoise des métadonnées à zéro, prévient les
    /// plugins `metadata`, et rafraîchit affichage et état diffusé.
    ///
    /// `None` = plus rien ne joue. Le cœur ne regarde jamais **dans** l'identité :
    /// il la compare par égalité et la relaie telle quelle.
    fn set_identity(&mut self, identity: Option<serde_json::Value>) {
        // « Plus rien ne joue » emporte la sélection courante avec lui : la
        // touche mise en évidence désigne **ce qui joue**, pas la dernière
        // pression. Fait avant le garde d'égalité : une identité déjà à
        // `None` (arrêt répété, bascule de source après un stop) doit quand
        // même laisser la sélection effacée.
        if identity.is_none() {
            self.preset = None;
            self.preset_name = None;
        }
        if !self.metadonnees.set_identity(identity) {
            return;
        }
        // Le morceau a changé : l'ancre du précédent ne doit pas continuer
        // d'avancer sous le titre du suivant. La dernière position publiée
        // doit disparaître avec elle, sans quoi la trame émise dans la
        // foulée porterait la position de l'ancien morceau sous le titre du
        // nouveau, jusqu'au prochain tick (jusqu'à une seconde).
        self.ancre_position = None;
        self.position_s = None;
        let np = NowPlaying {
            source: self.active_source.clone(),
            identity: self.metadonnees.identity().cloned(),
            // Toujours vide à cet instant précis (le reset ci-dessus vient
            // d'effacer tout ce que `Metadonnees` savait), mais lu depuis
            // `known()` plutôt qu'un `Known::default()` figé : la valeur
            // reste correcte si le reset venait un jour à changer, et
            // `publie_etat` republie ce même champ dès qu'il cesse d'être
            // vide.
            known: self.metadonnees.known(),
        };
        // Échec impossible en pratique : un `watch::Sender::send` n'échoue que
        // quand plus aucun récepteur ne vit, et `main` garde le sien pour
        // alimenter les connexions de plugins `metadata` à venir. De toute
        // façon sans conséquence sur la lecture : un `warn` suffirait à noyer
        // les journaux si aucun plugin metadata n'était déclaré.
        let _ = self.now_playing_tx.send(np);
        // L'ardoise a changé, donc l'affichage doit suivre — comme le font
        // `handle_icy_title` et `handle_enrichment`. Sans ce rafraîchissement,
        // `Command::Stop` laissait le titre du morceau arrêté **figé sur
        // l'afficheur** jusqu'à la prochaine action de l'utilisateur, alors que
        // la SPA, elle, se vidait correctement. `etat_lecteur` lit
        // `self.metadonnees.etat()` à chaque appel, donc ce seul `publie_etat`
        // suffit : plus besoin du second appel conditionnel à l'incrustation
        // qu'exigeait l'ancien canal de vues composées.
        self.publie_etat();
    }

    /// Titre annoncé par le flux lui-même (en-tête ICY vu par mpv).
    fn handle_icy_title(&mut self, titre: String) {
        // Deux gardes, et **aucune** ne consulte l'identité : cette couche doit
        // fonctionner sans plugin `metadata` et même face à une Source qui ne
        // déclare aucune identité, sinon la seule couche qui marche toute seule
        // se taisait en silence.
        //
        // En veille, rien ne doit atteindre l'affichage — même garde que
        // `handle_source_update`. Le chemin est réel : `Command::Power` attend
        // la réponse de la Source à `Deactivate` (jusqu'à 5 s) pendant que mpv
        // joue encore, et un titre émis dans cet intervalle arrive après que la
        // vue de veille a été poussée.
        //
        // `expecting_stream` est ce que le cœur sait **de lui-même** de la
        // lecture : mis à vrai sur chaque `Play` qu'il applique, à faux sur
        // `Stop`. C'est ce qui empêche un titre en retard de s'afficher, et d'y
        // rester, après un arrêt.
        if self.standby || !self.expecting_stream {
            return;
        }
        if !self.metadonnees.set_icy(titre) {
            return;
        }
        self.publie_etat();
    }

    /// Tags portés par le fichier joué, tels que mpv les expose.
    ///
    /// Mêmes gardes que l'ICY, à une différence près qui est tout l'objet du
    /// champ `lecture` : la garde « ça joue » ne peut pas être
    /// `expecting_stream`, qui vaut **faux** précisément pendant la lecture
    /// d'un contenu fini — donc pendant la seule lecture où des tags de
    /// fichier existent. S'en servir aurait produit une couche qui ne
    /// s'affiche jamais, sans rien dans les journaux.
    fn handle_file_tags(&mut self, morceau: ritornello_proto::Morceau) {
        if self.standby || !self.lecture {
            return;
        }
        if !self.metadonnees.set_tags(morceau) {
            return;
        }
        self.publie_etat();
    }

    /// Chemin du fichier que mpv a réellement ouvert (propriété `path`), pour
    /// en tirer la pochette embarquée. N'arme **qu'**une extraction détachée :
    /// voir `extraction_arrivee` pour la suite, à l'arrivée du résultat.
    ///
    /// Même garde « ça joue » que les tags (`lecture`, pas `expecting_stream`,
    /// pour la même raison) : `path` est republié aussi bien pour un flux que
    /// pour un fichier.
    ///
    /// **Le cœur complète, il n'écrase pas** : si une pochette est déjà tenue
    /// — le `folder.jpg` d'une Source, notamment — l'extraction n'est même
    /// pas lancée, ce qui économise une lecture de fichier pour rien et
    /// préserve la préséance voulue par `Metadonnees::cover_retenue`.
    ///
    /// **Toujours détachée, jamais exécutée sur ce fil.** `mpv::
    /// pochette_embarquee` ouvre et parcourt le fichier avec `lofty`, un
    /// appel strictement bloquant, potentiellement sur un partage réseau qui
    /// peut ne jamais répondre. L'exécuter ici figerait la boucle du cœur
    /// entière — mpv, les commandes, l'HTTP — le temps du blocage, pas
    /// seulement cette extraction. Ce projet a déjà vécu cet incident sur un
    /// montage cifs muet (voir `sante.rs`), d'où `Sante::borne` : `spawn_blocking`
    /// pour sortir du fil asynchrone, sous délai, avec un disjoncteur par
    /// point de montage pour ne pas perdre un fil du pool à chaque nouvelle
    /// piste tant que le partage reste muet.
    fn handle_path(&mut self, chemin: String) {
        // Retenu avant toute garde ci-dessous : c'est ce qu'`extraction_arrivee`
        // compare à l'arrivée pour rejeter une réponse tardive sur une piste
        // déjà remplacée, y compris quand `standby`/`lecture` ont changé
        // entre-temps.
        self.chemin_courant = Some(chemin.clone());
        if self.standby || !self.lecture {
            return;
        }
        if self.metadonnees.known().cover {
            return;
        }
        // Un flux n'a pas de tags, et `lofty` n'a rien à ouvrir sur une URL :
        // autant ne pas payer l'aller-retour tâche + canal pour un cas qui ne
        // peut jamais aboutir (`pochette_embarquee` le refuserait de toute
        // façon).
        if chemin.contains("://") {
            return;
        }
        if self.extraction_en_vol.as_deref() == Some(chemin.as_str()) {
            return;
        }
        self.extraction_en_vol = Some(chemin.clone());
        let tx = self.extraction_tx.clone();
        let sante = self.sante.clone();
        tokio::spawn(async move {
            let a_lire = chemin.clone();
            let r = sante
                .borne(std::path::Path::new(&chemin), move || mpv::pochette_embarquee(&a_lire))
                .await
                .flatten();
            let _ = tx.send((chemin, r)).await;
        });
    }

    /// Une extraction détachée de pochette embarquée (`handle_path`) s'est
    /// terminée. Symétrique de `pochette_arrivee` : la vérification de
    /// péremption se fait ici, à l'arrivée, pas au lancement.
    pub async fn extraction_arrivee(&mut self, chemin: String, r: Option<ritornello_proto::CoverRef>) {
        // Libéré quelle que soit l'issue et avant toute vérification
        // ci-dessous — même raison que `pochette_en_vol` dans
        // `pochette_arrivee` : sans cela, cette même piste rejouée plus tard
        // resterait bloquée pour le reste du processus.
        if self.extraction_en_vol.as_deref() == Some(chemin.as_str()) {
            self.extraction_en_vol = None;
        }
        // mpv est déjà passé à un autre fichier : cette réponse décrit une
        // piste qui n'est plus jouée, et ne doit pas s'installer sur la
        // suivante.
        if self.chemin_courant.as_deref() != Some(chemin.as_str()) {
            return;
        }
        // Une autre voie a fourni une pochette pendant que celle-ci était en
        // vol (la Source, ou un greffon) : le cœur complète, il n'écrase pas.
        if self.metadonnees.known().cover {
            return;
        }
        if !self.metadonnees.set_cover_tags(r) {
            return;
        }
        self.lance_pochette();
        self.publie_etat();
    }

    /// Enrichissement remonté par un plugin `metadata`. Rien ne se passe s'il
    /// est périmé, vide, ou émis par un plugin non déclaré (voir
    /// `Metadonnees::ajoute`).
    pub fn handle_enrichment(&mut self, plugin: &str, e: Enrichment) {
        if !self.metadonnees.ajoute(plugin, e) {
            return;
        }
        // On journalise **le gagnant**, pas celui qui vient de répondre : un
        // plugin moins prioritaire peut être retenu en réserve sans rien
        // afficher, et un journal qui le nommerait mentirait dans le seul cas
        // où on le consulte — celui d'un affichage douteux à attribuer.
        match self.metadonnees.gagnant() {
            Some(gagnant) if gagnant != plugin => {
                tracing::debug!("metadata displayed: {gagnant} (response from {plugin} held in reserve)");
            }
            Some(gagnant) => tracing::debug!("metadata displayed: {gagnant}"),
            None => {}
        }
        // Poser l'ancre à la réception : c'est le seul instant où l'écoulé
        // annoncé est exact.
        //
        // **Seulement quand c'est le gagnant qui vient de parler**, et c'est un
        // défaut trouvé en relecture. Un plugin retenu en réserve peut répondre
        // à tout moment (un titre corrigé, une pochette) sans rien apprendre de
        // neuf sur l'avancement : réancrer alors relirait la position
        // **inchangée** du gagnant en la datant de maintenant, et la barre
        // reculerait d'un coup de tout ce qu'elle avait avancé. Le `match`
        // ci-dessus distingue déjà les deux cas pour le journal.
        //
        // Un gagnant qui réémet à l'identique n'arrive jamais ici : `ajoute`
        // déduplique et rend `false`. Et un plugin plus prioritaire qui répond
        // pour la première fois **devient** le gagnant, donc son annonce ancre
        // bien, ce qui est voulu.
        if self.metadonnees.gagnant() == Some(plugin) {
            self.ancre_position = self.metadonnees.position_s().map(|p| (p, Instant::now()));
        }
        // L'enrichissement qui vient d'être retenu peut avoir changé la
        // pochette que `cover_retenue` désigne (un greffon qui écrase
        // répondant après un `fill_only`, par exemple) : `ajoute` a déjà
        // invalidé la clé publiée dans ce cas, à `lance_pochette` de relancer
        // la récupération pour la nouvelle cible.
        self.lance_pochette();
        self.publie_etat();
    }

    /// Retient la pochette qu'une Source vient de déclarer sur son propre
    /// canal (voir `SourceMessage::cover`, Task 2).
    pub fn set_cover_de_source(&mut self, c: Option<ritornello_proto::CoverRef>, origine: &str) {
        if self.metadonnees.set_cover_source(c, origine) {
            self.lance_pochette();
            self.publie_etat();
        }
    }

    /// Détache la récupération de la pochette retenue, si elle n'est pas déjà
    /// en cache ni en vol.
    ///
    /// Détachée, parce qu'un téléchargement de dix secondes ne doit pas
    /// retenir la boucle qui répond aux commandes. Et **abandonnée si
    /// l'identité change** : c'est `pochette_arrivee` qui vérifie, à
    /// l'arrivée, que la clé décrit encore ce qui joue — même garde-fou que
    /// l'écho d'identité du texte (`Metadonnees::ajoute`), pour la même
    /// raison : une réponse tardive sur le morceau précédent ne doit jamais
    /// s'installer sur le suivant.
    pub fn lance_pochette(&mut self) {
        let Some((r, _)) = self.metadonnees.cover_retenue() else {
            // Plus rien à montrer (identité changée, pochette retirée) :
            // effacer l'URL publiée plutôt que de laisser pointer une image
            // qui ne correspond plus à ce qui joue.
            self.metadonnees.set_cover_href(None);
            return;
        };
        let cle = crate::cover::cle(&r);
        if self.metadonnees.cover_publiee() == Some(cle.as_str()) {
            // Déjà publiée sous cette même clé : rien à refaire. Sans cette
            // garde, un enrichissement retenu qui republie à l'identique (une
            // station qui reconfirme ses métadonnées toutes les trente
            // secondes, par exemple) relancerait une tâche, un `contient` et
            // un aller-retour de canal pour un travail déjà fait — et
            // réarmerait `pochette_en_vol` sans nécessité.
            return;
        }
        if self.pochette_en_vol.as_deref() == Some(cle.as_str()) {
            // Déjà en vol pour cette même cible : une seconde requête
            // n'apprendrait rien de plus tôt, et doublerait le trafic réseau.
            return;
        }
        let covers = self.covers.clone();
        let tx = self.pochette_tx.clone();
        self.pochette_en_vol = Some(cle.clone());
        tokio::spawn(async move {
            if covers.contient(&cle).await {
                let _ = tx.send((cle, true)).await;
                return;
            }
            match crate::cover::recupere(&r).await {
                Some(p) => {
                    covers.insere(cle.clone(), p).await;
                    let _ = tx.send((cle, true)).await;
                }
                // Échec silencieux : l'appareil n'affiche pas d'image, et
                // c'est tout. Un 404 du Cover Art Archive est le cas courant.
                // Rapporté quand même (`false`) : c'est ce qui libère
                // `pochette_en_vol`, sans quoi cette clé resterait bloquée
                // pour le reste du processus — y compris si le même dossier
                // (donc la même clé) redevient la cible plus tard.
                None => {
                    tracing::debug!("no cover for {cle}");
                    let _ = tx.send((cle, false)).await;
                }
            }
        });
    }

    /// Une récupération détachée s'est terminée (`succes`), qu'elle ait
    /// abouti ou non. Publie l'URL locale, **si elle décrit encore ce qui
    /// joue** : la vérification se fait ici, à l'arrivée, pas au lancement —
    /// c'est ce qui empêche la pochette d'un morceau déjà remplacé de
    /// s'installer sur le suivant.
    pub async fn pochette_arrivee(&mut self, cle: String, succes: bool) {
        // Le marqueur se libère dès que cette clé revient, **quelle que soit
        // l'issue** — échec réseau, pochette qui n'est plus retenue, ou
        // succès — et **avant** toute vérification de péremption ci-dessous.
        // Sans cela, un échec ou un morceau déjà remplacé laissait cette clé
        // bloquée pour le reste du processus : `lance_pochette` refusait
        // ensuite de relancer une récupération pour cette même clé, même
        // quand elle redevenait la cible (le même dossier d'album, donc la
        // même clé, est rejoué plus tard) et même si les octets finissaient
        // par être en cache.
        if self.pochette_en_vol.as_deref() == Some(cle.as_str()) {
            self.pochette_en_vol = None;
        }
        // Le contrôle de péremption vaut pour les **deux** issues, et c'est
        // délibéré : un échec qui arrive après un changement de morceau décrit
        // une référence que ce qui joue maintenant ne vise pas. L'inscrire au
        // registre des échecs du morceau courant y noircirait une clé jamais
        // essayée pour lui — et si un contributeur proposait cette même image
        // ici, elle serait écartée sans qu'on l'ait tentée une seule fois.
        // L'échec vaut pour le morceau où il a eu lieu, comme tout le reste de
        // cet état (voir `Metadonnees::pochettes_echouees`).
        let Some((r, _)) = self.metadonnees.cover_retenue() else {
            // Plus rien ne joue, ou plus aucune pochette retenue : la
            // réponse arrive trop tard pour avoir un sens.
            return;
        };
        if crate::cover::cle(&r) != cle {
            // La pochette du morceau précédent (ou d'une référence
            // remplacée depuis) : sans cette vérification, elle s'installerait
            // sur le morceau courant.
            return;
        }
        if !succes {
            // L'échec est **retenu**, et c'est ce qui débloque les
            // contributeurs situés en dessous. Une référence retenue n'est
            // qu'une promesse : sans cette note, `cover_retenue` continuait de
            // préférer une URL morte, `known.cover` restait vrai, et
            // `musicbrainz` — muet parce qu'il croit une pochette tenue —
            // n'avait aucune chance de compenser. C'est exactement le cas que
            // la conception anticipe : « un motif qui casse rend un silence ».
            //
            // Relancer et republier seulement si la référence retenue a
            // réellement changé : c'est ce qui donne sa chance au contributeur
            // du dessous, et ce qui évite de republier pour rien.
            if self.metadonnees.marque_pochette_echouee(cle) {
                self.lance_pochette();
                self.publie_etat();
            }
            return;
        }
        // Recontrôlé plutôt que fait confiance à `succes` seul : le cache est
        // borné (`ENTREES` entrées, éviction FIFO) et cette clé a pu être
        // évincée entre le dépôt et la consommation de ce message par la
        // boucle de `main` — un cas d'autant plus réel que le canal est
        // volontairement étroit (capacité 4).
        if !self.covers.contient(&cle).await {
            return;
        }
        self.metadonnees.set_cover_href(Some(cle));
        self.publie_etat();
    }

    /// Le cache que la tâche détachée de `lance_pochette` remplit — **le
    /// même** que celui de l'`AppState` HTTP, voir la doc du champ `covers`.
    /// Réservé aux tests : c'est ce qui leur permet de prouver le partage
    /// sans passer par `main.rs`, qui n'est pas testable en tant que tel.
    #[cfg(test)]
    pub(crate) fn app_covers(&self) -> &Arc<crate::cover::CoverCache> {
        &self.covers
    }

    /// Relit où on en est, auprès du fournisseur qui a le droit de parler.
    ///
    /// Deux fournisseurs, jamais en concurrence : mpv pour un contenu fini,
    /// un plugin `metadata` pour un flux. Le `time-pos` d'un flux compte
    /// depuis le début de la connexion et n'a aucun rapport avec le morceau —
    /// il est lu et jeté, jamais publié.
    ///
    /// Ne publie rien : l'appelant décide (le tick publie, `handle_command`
    /// publie déjà en sortie).
    pub async fn rafraichit_position(&mut self) {
        if self.standby || !self.lecture {
            self.oublie_position();
            return;
        }
        if self.expecting_stream {
            // Flux : le `time-pos` de mpv compte depuis le début de la
            // connexion, sans rapport avec le morceau. La position vient donc
            // d'un plugin `metadata`, ancrée à sa réception et avancée ici.
            self.duree_mesuree_s = None;
            self.position_s = self.ancre_position.map(|(depart, pose)| {
                let ecoule = pose.elapsed().as_secs();
                let brute = depart.saturating_add(u32::try_from(ecoule).unwrap_or(u32::MAX));
                // Plafonnée par la durée annoncée : un morceau qui finit avant
                // que la station ne l'annonce ne doit pas afficher
                // « 4:31 / 4:14 ».
                match self.metadonnees.duration_s() {
                    Some(duree) => brute.min(duree),
                    None => brute,
                }
            });
            return;
        }
        match self.player.progression().await {
            Ok(p) => {
                self.position_s = p.position_s.map(|s| s as u32);
                self.duree_mesuree_s = p.duration_s.filter(|d| *d > 0.0).map(|s| s as u32);
            }
            Err(e) => {
                // Une position illisible n'arrête pas la musique : on cesse
                // simplement d'en annoncer une.
                tracing::debug!("playback progress unavailable: {e}");
                self.position_s = None;
                self.duree_mesuree_s = None;
            }
        }
    }

    /// Plus rien ne joue : plus rien à situer.
    fn oublie_position(&mut self) {
        self.position_s = None;
        self.duree_mesuree_s = None;
        self.ancre_position = None;
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

    pub async fn handle_command(&mut self, cmd: Command) -> Result<()> {
        if self.standby && cmd != Command::Power {
            return Ok(());
        }
        let issue = self.appliquer_commande(cmd).await;
        // Publication à la sortie de **toute** commande, plutôt qu'un appel dans
        // chacune : volume, muet, veille et source active y changent, et une
        // branche oubliée laisserait l'IHM afficher un état périmé sans que rien
        // ne le signale. Le canal déduplique, donc publier pour rien ne coûte
        // aucune trame. Publié même en cas d'erreur : l'état partiel atteint est
        // ce que l'IHM doit montrer.
        self.publie_etat();
        issue
    }

    /// Volume absolu, la seule voie pour un réglage qui ne vient d'aucune touche :
    /// le `setvol` de MPD. Mêmes effets de bord que le pas relatif — mpv, disque,
    /// incrustation — parce qu'un volume changé depuis le réseau doit s'annoncer à
    /// l'écran comme celui changé depuis la télécommande.
    async fn set_volume(&mut self, v: u8) -> Result<()> {
        self.volume = v.min(100);
        self.player.set_volume(self.volume).await?;
        self.persist();
        self.show_overlay().await;
        Ok(())
    }

    /// One volume step (±5), applied to mpv, persisted, shown as an overlay.
    /// Shared by fresh presses and held repeats; only the caller decides how
    /// to re-arm `volume_deadline`.
    async fn step_volume(&mut self, up: bool) -> Result<()> {
        let v = self.volume as i16 + if up { 5 } else { -5 };
        self.set_volume(v.clamp(0, 100) as u8).await
    }

    /// Entry point for everything that used to call `handle_command`: fresh
    /// commands pass through unchanged; held (autorepeat) volume commands are
    /// paced by `volume_deadline`. Held on any other command is a no-op — the
    /// remote's autorepeat only means something for the volume.
    pub async fn handle_input(&mut self, msg: InputMessage) -> Result<()> {
        if !msg.held {
            return self.handle_command(msg.cmd).await;
        }
        if self.standby {
            return Ok(());
        }
        let up = match msg.cmd {
            Command::VolumeUp => true,
            Command::VolumeDown => false,
            _ => return Ok(()),
        };
        let Some(deadline) = self.volume_deadline else { return Ok(()) };
        if Instant::now() < deadline {
            return Ok(());
        }
        let issue = self.step_volume(up).await;
        self.volume_deadline =
            Some(Instant::now() + Duration::from_millis(self.settings.volume_repeat_interval_ms.into()));
        // Same publication contract as `handle_command`: the UI must see the
        // new volume even if mpv errored mid-way.
        self.publie_etat();
        issue
    }

    /// New settings from `PUT /api/settings` (via the `select!` loop of main).
    /// No bounds check here: the HTTP layer validates, and tests rely on tiny
    /// timings.
    pub fn set_settings(&mut self, s: crate::state::Settings) {
        // Poussé dans le cache de pochettes, qui est le seul autre porteur de
        // ces réglages. Ici et non dans un bras du `select!` : `set_settings`
        // est le point de passage **unique** de tout changement de réglages —
        // la route HTTP comme le chargement au démarrage —, donc le seul
        // endroit où la propagation ne peut pas être oubliée par un futur
        // appelant. Synchrone parce que `CoverCache` garde ces réglages sous un
        // verrou `std::sync` exprès pour ça.
        self.covers.set_reglages(crate::cover::Reglages::from(&s));
        self.settings = s;
        self.persist();
    }

    /// Startup: what the process does with the active source when it
    /// launches, per `settings.startup_power`. Called once by `main`, which
    /// treats a failure as best-effort — startup must never put systemd in a
    /// restart loop.
    ///
    /// The `On` branch persists before waking: `start_in_standby` writes
    /// `standby: true` on the other side, and without this the file would
    /// keep saying "in standby" after a boot that woke everything up — a
    /// later switch to `Previous` would then resurrect a standby the device
    /// left behind long ago.
    pub async fn demarrage(&mut self) -> Result<()> {
        let en_veille = match self.settings.startup_power {
            StartupPower::On => false,
            StartupPower::Standby => true,
            StartupPower::Previous => self.veille_persistee,
        };
        if en_veille {
            return self.start_in_standby().await;
        }
        // `resume` est aussi la moitie « reveil » de `Command::Power`, ou le
        // drapeau est deja baisse ; ici c'est cette methode qui le baisse,
        // pour que le fichier decrive un appareil reveille.
        self.standby = false;
        self.persist();
        self.resume().await
    }

    /// Startup in standby (`settings.startup_power`): mpv is configured
    /// (volume, audio device) so a later wake starts right, but the active
    /// source is not woken and the display shows the standby status.
    ///
    /// `standby_status` is not resolved here: it already is, since
    /// construction (see its doc) — no catalogue read on this path.
    pub async fn start_in_standby(&mut self) -> Result<()> {
        self.standby = true;
        // Written before touching mpv, like the standby half of
        // `Command::Power`: what is on disk must describe the device even if
        // the calls below fail.
        self.persist();
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        self.publie_etat();
        // A held key must re-press after standby: stale deadlines don't survive it.
        self.volume_deadline = None;
        Ok(())
    }

    async fn appliquer_commande(&mut self, cmd: Command) -> Result<()> {
        // Any command other than Plus10/Select abandons a pending tens
        // sequence: pressing volume mid-sequence is a change of mind, not a
        // step of it. When an offset was actually armed, its `+NN` overlay
        // must go with it: `etat_lecteur` gives the overlay absolute
        // priority, and none of the arms below (PlayPause, Stop, Next, Prev,
        // Eject) rewrite it on their own, so without clearing it here the
        // display would keep showing an offset that no longer applies until
        // the deadline expires on its own. `handle_command`'s trailing
        // `publie_etat` picks up the clear — no need to publish here too.
        // The `!= 0` guard on `mem::take` keeps VolumeUp/Mute/Power
        // unaffected: they already overwrite or clear `self.overlay` right
        // after this.
        if !matches!(cmd, Command::Plus10 | Command::Select(_))
            && std::mem::take(&mut self.pending_tens) != 0
        {
            self.overlay = None;
        }
        match cmd {
            Command::Select(n) => {
                let tens = std::mem::take(&mut self.pending_tens);
                if tens != 0 {
                    // The consumed offset's overlay has said what it had to
                    // say; the source's own status takes over.
                    self.overlay = None;
                }
                let n = n.saturating_add(tens);
                if n == 0 {
                    // Key 0 with no pending offset: nothing to select.
                    return Ok(());
                }
                self.retry_count = 0;
                if let Some(action) = self.demande_active(SourceReq::Select(n)).await? {
                    self.apply(action).await?;
                }
            }
            // `Next`/`Prev` portent maintenant les deux sémantiques : la
            // source active décide (préselection pour la radio, piste pour
            // le cd — voir `SourcePlugin::next`/`prev` de chaque plugin).
            // Remettre `retry_count` à 0 ici est correct pour un changement
            // de préselection (nouveau flux radio, un retry sur l'ancien
            // n'aurait plus de sens) et inoffensif pour un changement de
            // piste cd (`retry_count` ne concerne que la relance d'un flux
            // réseau attendu, pas la lecture cd) : rien à distinguer entre
            // les deux sources sur ce point.
            Command::Next => {
                self.retry_count = 0;
                if let Some(action) = self.demande_active(SourceReq::Next).await? {
                    self.apply(action).await?;
                }
            }
            Command::Prev => {
                self.retry_count = 0;
                if let Some(action) = self.demande_active(SourceReq::Prev).await? {
                    self.apply(action).await?;
                }
            }
            Command::Eject => {
                if let Some(action) = self.demande_active(SourceReq::Eject).await? {
                    self.apply(action).await?;
                }
            }
            Command::VolumeUp | Command::VolumeDown => {
                self.step_volume(cmd == Command::VolumeUp).await?;
                self.volume_deadline = Some(
                    Instant::now() + Duration::from_millis(self.settings.volume_repeat_initial_ms.into()),
                );
            }
            Command::SetVolume(v) => {
                // Pas de `volume_deadline` a rearmer : ce n'est pas une touche,
                // rien ne peut etre maintenu.
                self.set_volume(v).await?;
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
                self.show_overlay().await;
            }
            Command::PlayPause => {
                if self.lecture {
                    // Basculer la croyance **après** que mpv a accepté, jamais
                    // avant. Le `?` propage un échec de `toggle_pause` et laisse
                    // `paused` intact : c'est cette valeur-là que
                    // `PlayerState.playback` publie, et à laquelle le greffon
                    // MPD compare ses `pause 0`/`pause 1` — un cœur qui se croit
                    // en pause devant un mpv qui joue fait répondre « paused » à
                    // un client dont la musique continue, et le `pause 0`
                    // suivant est alors jugé sans effet et ignoré.
                    self.player.toggle_pause().await?;
                    self.paused = !self.paused;
                } else {
                    // Rien n'est chargé : `stop` **vide la liste de mpv**, si
                    // bien que « basculer la pause » n'a plus rien à reprendre.
                    // La touche Lecture ne faisait donc rien du tout après un
                    // Stop, sur toutes les sources — mesuré sur la radio comme
                    // sur les fichiers. On redemande à la source active de
                    // jouer, ce qui est exactement ce que la touche promet.
                    //
                    // `lecture` et non `expecting_stream` : la première dit
                    // « quelque chose joue, de quelque nature », la seconde ne
                    // vaut que pour les flux relançables. Une pause, elle, ne
                    // touche ni l'une ni l'autre — la reprise reste donc un
                    // simple basculement, sans rechargement.
                    if let Some(action) = self.demande_active(SourceReq::Activate).await? {
                        self.apply(action).await?;
                    }
                }
            }
            Command::Stop => {
                self.expecting_stream = false;
                self.lecture = false;
                self.player.stop().await?;
                // Oublier l'identité **avant** de prévenir la Source : cet
                // appel efface le titre de l'afficheur, et une Source
                // injoignable ferait attendre jusqu'à 5 s (timeout de
                // `SourceClient::request`) avec le morceau arrêté encore à
                // l'écran.
                self.set_identity(None);
                // La Source n'a pas été consultée pour cet arrêt : le lui dire,
                // sinon celle qui tient un état de lecture propre (le cd) le
                // garderait faux et annoncerait plus tard des métadonnées pour
                // un morceau à l'arrêt. Au mieux : une Source muette n'empêche
                // rien.
                if let Err(e) = self.demande_active(SourceReq::Stop).await {
                    tracing::debug!("stop notification to source: {e}");
                }
            }
            Command::Power => {
                self.standby = !self.standby;
                // Persister **avant** de prevenir la Source, pour la meme
                // raison qu'au `SourceCycle` plus bas : une Source
                // injoignable fait attendre jusqu'a 5 s, et `StartupPower::
                // Previous` doit retrouver la veille voulue meme si le
                // courant est coupe pendant cette attente.
                self.persist();
                if self.standby {
                    let _ = self.demande_active(SourceReq::Deactivate).await;
                    self.player.stop().await?;
                    self.expecting_stream = false;
                    self.lecture = false;
                    // Même raison qu'au-dessus : la réponse de la Source à
                    // `Deactivate` est ignorée, et la vue de veille qui suit
                    // passerait outre le garde-fou de `handle_source_update`.
                    self.set_identity(None);
                    // Le compte de présélections et le statut n'ont de sens que
                    // pour la Source active : la veille les oublie tous les
                    // deux, et la prochaine Source (activate/wake) les
                    // redéclarera si elle en a. Sans cet effacement, le statut
                    // de l'ancienne source (« PAS DE DISQUE ») survivait à la
                    // veille en mémoire, prêt à réapparaître au réveil avant
                    // que la Source n'ait reparlé.
                    self.preset_count = None;
                    self.source_status = None;
                    // Même sort pour la capacité d'éjection : en veille aucune
                    // commande ne passe de toute façon (`handle_command`), et
                    // la Source la redéclarera au réveil.
                    self.can_eject = false;
                    // L'incrustation volume/muet ne survit pas à la mise en
                    // veille : elle garde la priorité dans `etat_lecteur`, et
                    // « VOLUME 65 % » restait à l'écran jusqu'à 2 s après
                    // l'extinction avant que le mot de veille n'apparaisse.
                    self.overlay = None;
                    // `standby_status` n'est pas résolu ici : il l'est déjà,
                    // depuis la construction et chaque `set_locale` (voir sa
                    // doc) — plus jamais au moment de poser la veille.
                    // A held key must re-press after standby: stale deadlines don't survive it.
                    self.volume_deadline = None;
                } else {
                    self.resume().await?;
                }
            }
            Command::SourceCycle => {
                // La source active peut ne plus être dans l'ordre : c'est l'état
                // que laisse `oublie_source_morte` — le greffon a disparu, la
                // musique continue, et son nom reste affiché. Dans ce cas, la
                // touche Source doit repartir de la **première** source
                // disponible. Un `position().unwrap_or(0)` suivi du `+ 1`
                // sautait la première pour aller à la seconde, ce qui rendait
                // une source inatteignable au clavier tant qu'on n'avait pas
                // fait un tour complet.
                let suivante = match self.source_order.iter().position(|n| n == &self.active_source) {
                    Some(idx) => self.source_order.get((idx + 1) % self.source_order.len()).cloned(),
                    None => self.source_order.first().cloned(),
                };
                self.bascule_source(suivante).await?;
            }
            Command::SelectSource(nom) => {
                // Inconnue : ignorée en silence, comme une touche non liée. Le
                // greffon MPD a déjà répondu `ACK 50` de son côté — il ne
                // propose que des noms reçus du catalogue, donc arriver ici
                // veut dire que la source a disparu entre-temps (un greffon
                // qu'on vient d'éteindre depuis l'IHM, par exemple).
                if !self.source_order.iter().any(|n| n == &nom) {
                    tracing::debug!("unknown source {nom} ignored");
                    return Ok(());
                }
                // Déjà active : ne rien faire. Un `load` redondant ne doit pas
                // couper ce qui joue, et c'est exactement ce qu'un client
                // envoie en rouvrant son écran.
                if nom != self.active_source {
                    // `Some(nom)` : `bascule_source` admet `None` — « plus
                    // aucune source » — mais ce chemin-ci désigne toujours un
                    // nom, le garde ci-dessus l'ayant vérifié dans l'ordre.
                    self.bascule_source(Some(nom)).await?;
                }
            }
            Command::Plus10 => {
                let next = self.pending_tens.saturating_add(10);
                self.pending_tens = match self.preset_count {
                    // Wrap past the last useful decade: the largest
                    // reachable multiple of 10 is (count / 10) * 10
                    // (station 20 is +10 +10 then 0, so offset 20 must
                    // stay allowed for a count of 20).
                    Some(count) if next > (count / 10) * 10 => 0,
                    // No known count: saturate, don't wrap — we can't know
                    // where the end is.
                    None => next.min(240),
                    _ => next,
                };
                if self.pending_tens == 0 {
                    self.overlay = None;
                } else {
                    self.show_tens_overlay().await;
                }
            }
            Command::SeekForward | Command::SeekBackward => {
                // Ignorée en silence sur un contenu non parcourable : la
                // touche se comporte comme une touche non liée, ce que la
                // télécommande sait déjà faire. Un message n'apprendrait rien
                // à qui vient d'appuyer.
                if self.lecture && !self.expecting_stream {
                    let pas = i64::from(self.settings.seek_step_s);
                    let delta = if cmd == Command::SeekForward { pas } else { -pas };
                    self.player.seek_relative(delta).await?;
                    self.rafraichit_position().await;
                }
            }
            Command::SeekTo(position_s) => {
                if self.lecture && !self.expecting_stream {
                    self.player.seek_absolute(position_s).await?;
                    self.rafraichit_position().await;
                }
            }
        }
        Ok(())
    }

    pub async fn handle_event(&mut self, ev: Event) -> EventOutcome {
        match ev {
            // Un seul endroit décide quelles variantes attestent la vivacité
            // du flux : la boucle de `main` (qui tient l'échéance `retry_at`)
            // et ce compteur suivent le même verdict via `StreamAlive`, au
            // lieu de dupliquer la liste des variantes de part et d'autre.
            Event::Title(_) | Event::PlaybackActive => {
                self.retry_count = 0;
                return EventOutcome::StreamAlive;
            }
            // Volontairement sans effet sur `retry_count` : la vivacité du flux
            // est déjà attestée par `PlaybackActive`, et un titre ICY n'est pas
            // une preuve de lecture (une station peut en envoyer un puis se
            // taire). Ici, uniquement des métadonnées.
            Event::IcyTitle(titre) => self.handle_icy_title(titre),
            // Même statut que l'ICY vis-à-vis de `retry_count` : des
            // métadonnées ne prouvent pas que la lecture est vivante.
            Event::FileTags(morceau) => self.handle_file_tags(morceau),
            // Même statut que les tags vis-à-vis de `retry_count` : le chemin
            // n'atteste rien de la vivacité du flux, il sert uniquement à la
            // pochette embarquée.
            Event::Path(chemin) => self.handle_path(chemin),
            // Le lecteur a changé de piste de lui-même : fin de piste d'un
            // disque, pression sur aucune touche. Le cœur le sait (mpv le lui
            // dit) mais ne peut pas corriger l'identité — elle est opaque pour
            // lui. Il le dit donc à la Source, qui renverra vue et identité par
            // le canal habituel. Sans cela, l'affichage et les métadonnées
            // restaient sur la piste précédente jusqu'à la prochaine commande.
            //
            // L'événement arrive aussi pour les changements **demandés** (la
            // Source vient déjà de se recaler) : elle renvoie alors la même
            // identité, que le cœur reconnaît comme inchangée, et la vue
            // identique n'est pas repoussée.
            Event::TrackChanged(n) => {
                if !self.standby {
                    if let Err(e) = self.demande_active(SourceReq::PlayerTrack(n)).await {
                        tracing::debug!("track notification to source: {e}");
                    }
                }
            }
            Event::PlaybackIdle => {
                if !self.standby && self.expecting_stream {
                    let delay = (RETRY_BASE * 2u32.pow(self.retry_count)).min(RETRY_MAX);
                    self.retry_count = (self.retry_count + 1).min(4);
                    return EventOutcome::RetryIn(delay);
                }
                // Fin de lecture **normale** (fin de disque, notamment) : le
                // dire à la Source, seule à pouvoir recaler son état de
                // lecture, sa vue et son identité — le cœur ne peut pas
                // inventer « plus rien ne joue » à sa place, l'identité est
                // opaque. Sans cela, la fin d'un disque laissait la dernière
                // piste et ses métadonnées affichées indéfiniment.
                // Idempotent quand l'arrêt vient d'une commande (la Source a
                // déjà été prévenue par `Command::Stop`).
                //
                // Plus rien ne joue : sans cela, les tags du dernier fichier
                // resteraient recevables et un ultime rafraîchissement de mpv
                // les remettrait à l'écran après la fin de la liste.
                self.lecture = false;
                if !self.standby {
                    if let Err(e) = self.demande_active(SourceReq::Stop).await {
                        tracing::debug!("stop notification to source: {e}");
                    }
                }
            }
        }
        EventOutcome::Nothing
    }

    /// Requête à la source active, **s'il y en a une**.
    ///
    /// `Ok(None)` n'est pas une erreur : depuis l'enregistrement à chaud, le
    /// cœur peut tourner sans aucune source. Un greffon `source` qui rate la
    /// fenêtre de rendez-vous s'annonce à t+30 s et est câblé sans redémarrage,
    /// et refuser de démarrer à t+10 s pour l'attendre supprimait la page de
    /// statut précisément quand on voulait l'y voir figé.
    ///
    /// C'est ce que le `panic!("unknown active source")` d'avant interdisait :
    /// il ne protégeait aucun invariant — `Core::new` retombe déjà sur la
    /// première source triée, donc le nom n'est introuvable que si la table est
    /// **vide** — et il aurait échangé un refus de démarrer lisible contre un
    /// arrêt brutal au démarrage, sans page pour le raconter.
    ///
    /// Sans source, une commande **ne fait rien** et le dit en `debug` : ce
    /// n'est pas une anomalie, seulement un appareil qui n'a rien à lire.
    /// Un `warn` remplirait le tampon d'erreurs de l'IHM à chaque touche.
    async fn demande_active(&self, req: SourceReq) -> Result<Option<SourceAction>> {
        let Some(source) = self.sources.get(&self.active_source) else {
            tracing::debug!("no active source, dropping {req:?}");
            return Ok(None);
        };
        source.request(req).await.map(Some)
    }

    /// Nom de la source actuellement active (pour la page de statut vivante).
    pub fn active_source(&self) -> &str {
        &self.active_source
    }

    /// Langue courante, à transmettre au lancement d'un greffon rallumé.
    ///
    /// La langue est passée au processus via `RITORNELLO_LOCALE` : un greffon
    /// rallumé sur un appareil en français doit la retrouver au démarrage,
    /// sans attendre un `SetLocale` — le piège déjà rencontré avec `cd`, qui
    /// réaffichait `NO DISC` faute de langue tant qu'aucun changement de
    /// langue ne survenait après coup.
    pub fn locale_courante(&self) -> Option<String> {
        self.locale.clone()
    }

    /// Ajoute une source découverte **après** le démarrage : un greffon qui a
    /// raté le rendez-vous, ou qu'on a relancé à la main. Renvoie `true` si
    /// c'est un remplacement (ré-annonce d'un greffon déjà câblé).
    ///
    /// `source_order` est **retrié** : le cycle de sources suit l'ordre
    /// alphabétique, et une source arrivée en retard doit y prendre sa place
    /// normale, pas la queue — sinon `SourceCycle` change de sens selon la
    /// chronologie du démarrage.
    ///
    /// Si aucune source n'était active — un démarrage où *aucune* n'avait
    /// répondu — la nouvelle le devient : c'est le seul cas où l'arrivée d'un
    /// greffon change ce qui joue.
    ///
    /// **Ne réveille rien** : cette fonction n'affecte que la table et le nom
    /// de l'active. Le câblage à chaud passe par `cable_source_a_chaud`, qui
    /// enchaîne le réveil — sans quoi une première source arrivée en retard
    /// serait active et muette.
    pub fn add_source(&mut self, name: String, client: Arc<dyn Source>) -> bool {
        let premiere = self.sources.is_empty();
        let remplacement = self.sources.insert(name.clone(), client).is_some();
        if !self.source_order.contains(&name) {
            self.source_order.push(name.clone());
            self.source_order.sort();
        }
        if premiere {
            self.active_source = name;
        }
        // Le catalogue vient de changer de longueur : une source de plus y
        // figure, sans présélections tant qu'elle n'en a pas déclaré. Voir
        // `publie_catalogue` pour la liste à jour de ses points d'appel —
        // `remove_source` en est le symétrique.
        self.publie_catalogue();
        remplacement
    }

    /// Bascule vers `suivante` (ou vers **aucune** source si `None`) : arrêt,
    /// `Deactivate` de la sortante, oublis, persistance, `Activate` de l'entrante.
    ///
    /// Extraite de `Command::SourceCycle` et non recopiée : la désactivation d'un
    /// greffon fait exactement la même chose, et deux versions de cette séquence
    /// divergeraient au premier oubli ajouté d'un côté.
    ///
    /// Trois appelants, donc : `SourceCycle` (qui calcule le nom suivant dans
    /// l'ordre), `SelectSource` (qui le reçoit déjà tout fait, du greffon MPD)
    /// et `remove_source` (qui peut n'avoir aucun nom à donner). Séquence
    /// commune : arrêt du lecteur, `Deactivate` en best-effort, oubli de
    /// l'identité, du compte de présélections, du statut et de l'éjection,
    /// `persist()` **avant** `Activate`, publication finale.
    async fn bascule_source(&mut self, suivante: Option<String>) -> Result<()> {
        // Changer de source, c'est toujours changer de ce qui joue — et c'est
        // le cœur qui arrête, sans dépendre des réponses des plugins. Avant,
        // l'action renvoyée par `Deactivate` (le `Stop` du plugin radio) était
        // ignorée, et l'arrêt reposait sur le `Play` de l'`Activate` suivant —
        // que le cd sans disque ne renvoie pas (`Noop`) : l'ancien flux
        // continuait de jouer sous un affichage qui annonçait la nouvelle
        // source, titres ICY compris.
        self.expecting_stream = false;
        self.lecture = false;
        self.player.stop().await?;
        // L'ancienne source est prévenue en best-effort : son arrêt est déjà
        // fait, elle n'a plus qu'à recaler son propre état.
        if let Err(e) = self.demande_active(SourceReq::Deactivate).await {
            tracing::debug!("deactivate: {e}");
        }
        self.active_source = suivante.unwrap_or_default();
        // On l'acte ici sans attendre que la nouvelle Source le déclare :
        // sinon une Source qui omettrait de le faire laisserait l'identité de
        // l'autre en place, et les plugins `metadata` continueraient
        // d'enrichir le morceau précédent.
        self.set_identity(None);
        // Le compte de présélections et le statut annoncés par l'ancienne
        // Source ne veulent rien dire pour la nouvelle : les garder
        // afficherait une fenêtre de numéros qui ne correspond à aucune
        // présélection réelle, ou un statut (« PAS DE DISQUE ») sous le nom
        // d'une source qui n'a encore rien dit — tant que la nouvelle Source
        // n'a pas parlé (ce qui peut ne jamais arriver : une présélection
        // vide déclare une trame éphémère, qui ne touche pas au statut
        // mémorisé).
        self.preset_count = None;
        self.source_status = None;
        // Idem pour l'éjection : la capacité décrit la Source qui s'en va.
        // Sans cet effacement, quitter le cd pour la radio laissait la touche
        // Eject active jusqu'à la première trame de la radio — et pour de bon
        // si elle restait muette.
        self.can_eject = false;
        self.retry_count = 0;
        // Persister **avant** `Activate` : si la nouvelle source ne répond
        // pas (timeout de 5 s du SDK), l'état mémoire, l'état sur disque et
        // l'affichage disent déjà tous la même chose — nouvelle source, rien
        // ne joue. Sans cela, l'échec laissait la bascule à moitié faite :
        // « cd » à l'écran, « radio » dans state.json.
        self.persist();
        if let Some(action) = self.demande_active(SourceReq::Activate).await? {
            self.apply(action).await?;
        }
        // La séquence n'est complète qu'une fois le nouvel état publié : tous
        // les chemins ci-dessus (`set_identity`, `apply`) ne publient que
        // lorsqu'ils changent quelque chose, et rien ne garantit qu'au moins
        // un d'eux le fasse — désactiver l'unique source, ou la désactiver
        // pendant qu'elle joue sans qu'une Source muette ne réponde à temps,
        // n'en déclenche aucun. `handle_command` publie déjà après chaque
        // commande, mais un appelant hors de ce chemin (le décâblage à chaud
        // d'un greffon) laisserait sinon les afficheurs décrire une source qui
        // n'existe plus. Le canal déduplique (`publie_etat`), donc cet appel
        // ne coûte rien de plus sur le chemin `SourceCycle`.
        self.publie_etat();
        Ok(())
    }

    /// Oublie une source dont le greffon est mort **de lui-même** — panique,
    /// `SIGSEGV`, tué à la main. Rend `false` si ce nom n'était pas une source.
    ///
    /// **La différence avec `remove_source` est délibérée, et elle tient en une
    /// phrase : celui-là bascule, celui-ci non.** Les deux évincent la même
    /// chose du catalogue, pour la même raison (un client MPD ne doit pas voir
    /// une liste enregistrée pour une source qu'il ne peut plus atteindre) ;
    /// seule diffère la conséquence sur ce qui joue, parce que seule diffère la
    /// question de qui a décidé.
    ///
    /// * `remove_source` : **l'opérateur a demandé** que cette source s'en aille.
    ///   Basculer vers la suivante est la suite de son geste, et arrêter le
    ///   lecteur d'abord est ce qui empêche l'ancien flux de continuer sous le
    ///   nom de la nouvelle source.
    /// * ici : **personne n'a rien demandé**. Un greffon de Source est un
    ///   *contrôleur* — il dit quoi jouer, il ne joue pas. Le flux est tenu par
    ///   mpv, qui est un enfant du cœur et que la mort du greffon ne touche pas.
    ///   Arrêter mpv et basculer sur le cd, c'est transformer la panne d'un
    ///   contrôleur en silence, puis présenter à l'écran une source que
    ///   l'utilisateur n'a pas choisie : deux fautes, dont la seconde est du
    ///   mensonge. On ne fait donc ni l'un ni l'autre — la musique continue,
    ///   `active_source` garde le nom de la source qui a disparu, et la page de
    ///   statut dit la vérité entière (« radio », active, non joint).
    ///
    /// Ce qui est quand même oublié : les présélections nommées (le catalogue ne
    /// doit pas proposer d'agir sur un greffon mort) et, si c'était l'active, les
    /// deux **capacités** qu'elle avait déclarées — `preset_count` et
    /// `can_eject`. Celles-là décrivent ce qu'un greffon sait faire, et il n'est
    /// plus là pour le faire : laisser la touche Eject allumée ou la grille de
    /// présélections ouverte donnerait des commandes qui ne peuvent plus aboutir.
    /// `bascule_source` les efface déjà pour ce motif exact.
    ///
    /// Ce qui est gardé, et c'est aussi voulu : `source_status` et l'identité de
    /// ce qui joue. Elles décrivent **le morceau en cours**, qui joue encore ;
    /// les effacer noircirait l'afficheur au milieu d'un titre. `persist()` n'est
    /// pas appelée : `active_source` n'a pas changé, et l'état sur disque nomme
    /// donc toujours la source que l'utilisateur a choisie — au prochain
    /// démarrage le greffon est relancé et la retrouve.
    ///
    /// Non-`async` : c'est la conséquence directe de ne pas basculer. Aucun
    /// `Deactivate` à envoyer (le pair est mort), aucun `Activate` à attendre.
    pub fn oublie_source_morte(&mut self, name: &str) -> bool {
        let Some(pos) = self.source_order.iter().position(|n| n == name) else {
            return false;
        };
        self.sources.remove(name);
        self.source_order.remove(pos);
        self.presets_par_source.remove(name);
        if self.active_source == name {
            self.preset_count = None;
            self.can_eject = false;
        }
        self.publie_catalogue();
        // Publier l'état aussi : `can_eject` et `preset_count` en font partie, et
        // aucun autre chemin ne le fera — ce bras-ci n'est pas une commande.
        self.publie_etat();
        true
    }

    /// Retire une source décâblée — un greffon qu'on vient d'éteindre depuis
    /// l'IHM. Rend `false` si ce nom n'était pas une source.
    ///
    /// **À ne pas confondre avec `oublie_source_morte`**, qui traite la mort
    /// *subie* du même greffon : celle-là ne bascule pas et n'arrête pas le
    /// lecteur. La doc de l'autre porte la comparaison des deux chemins.
    ///
    /// Si c'était l'active, la **suivante du cycle** prend sa place, ou aucune
    /// s'il n'en reste pas : `demande_active` tolère déjà l'absence de source, et
    /// démarrer sans source est légitime depuis l'enregistrement à chaud.
    ///
    /// L'ordre est délicat : la bascule doit avoir lieu **avant** le retrait de la
    /// table, parce que c'est elle qui envoie `Deactivate` à la source sortante —
    /// retirée d'abord, elle ne recevrait rien et le greffon garderait son état
    /// interne pour sa prochaine vie.
    pub async fn remove_source(&mut self, name: &str) -> Result<bool> {
        let Some(pos) = self.source_order.iter().position(|n| n == name) else {
            return Ok(false);
        };
        if self.active_source == name {
            let suivante = if self.source_order.len() > 1 {
                Some(self.source_order[(pos + 1) % self.source_order.len()].clone())
            } else {
                None
            };
            // Pas de `?` : la bascule peut échouer (l'entrante ne répond pas à
            // `Activate`, ou l'arrêt lui-même échoue), mais le retrait qui suit
            // doit avoir lieu quand même. Un greffon qu'on éteint doit finir
            // entièrement décâblé — jamais à moitié, avec un `SourceCycle` qui
            // pourrait encore retomber sur un processus qui n'existe plus —
            // c'est tout le principe d'un accusé qui ne décrit qu'un état déjà
            // vrai.
            if let Err(e) = self.bascule_source(suivante.clone()).await {
                tracing::warn!("switching away from {name} while removing it: {e:#}");
                // `bascule_source` pose `active_source` **avant** son étage qui
                // peut échouer (`Activate`) mais **après** un `stop()` qui peut
                // lui aussi échouer : selon l'étage en cause, `active_source`
                // peut encore nommer la source qu'on est en train de retirer de
                // la table. La reposer ici est sans risque dans les deux cas.
                self.active_source = suivante.unwrap_or_default();
            }
        }
        self.sources.remove(name);
        self.source_order.remove(pos);
        // Les présélections nommées de la source partent avec elle, et le
        // catalogue est republié dans la foulée.
        //
        // Ce n'est pas du ménage : le catalogue est le seul canal par lequel un
        // client MPD apprend qu'une liste enregistrée existe. Laissée en place,
        // l'entrée ferait figurer dans `listplaylists` une source qui n'existe
        // plus, et un client pourrait **agir** dessus — un `load "radio"` sur un
        // greffon éteint. Le garde de `Command::SelectSource` le refuserait
        // (`source_order` ne porte plus le nom), mais l'utilisateur, lui, verrait
        // une liste qui ment jusqu'au redémarrage : les clients MPD mettent
        // volontiers `listplaylists` en cache.
        //
        // `source_order` est vidé juste au-dessus, donc `catalogue()` ne cite
        // déjà plus cette source ; retirer aussi la table évite qu'un greffon
        // rallumé sous le même nom hérite silencieusement de la liste de sa vie
        // précédente au lieu d'attendre son propre `ListPresets` (voir
        // `cable_source_a_chaud`).
        self.presets_par_source.remove(name);
        self.publie_catalogue();
        Ok(true)
    }

    /// Câble une source qui s'annonce **après** le démarrage. Renvoie `true`
    /// s'il s'agit d'un remplacement (ré-annonce d'un greffon déjà câblé).
    ///
    /// Deux chemins, et c'est tout l'intérêt de les tenir ensemble ici :
    ///
    /// - **Première source du cœur** (la table était vide) : le démarrage est
    ///   rejoué par `resume`, donc `SetLocale` puis `Wake`, dans cet ordre.
    ///   `add_source` ne fait que désigner l'active ; sans ce réveil, une source
    ///   arrivée à t+30 s serait active et **muette** jusqu'à ce que
    ///   l'utilisateur touche quelque chose — l'appareil aurait l'air en panne
    ///   alors que tout est câblé.
    /// - **Source supplémentaire, ou cœur en veille** : seule la langue est due.
    ///   Réveiller ici rallumerait un appareil qu'on a volontairement éteint, et
    ///   changerait ce qui joue parce qu'un greffon a fini de démarrer.
    ///
    /// L'état est publié dans les deux cas : le nom de la source vient
    /// d'apparaître dans la trame, et la SPA comme les afficheurs annonçaient
    /// jusque-là « aucune source ». (`resume` publie déjà pour le premier.)
    pub async fn cable_source_a_chaud(
        &mut self,
        name: String,
        client: Arc<dyn Source>,
    ) -> Result<bool> {
        let premiere = self.sources.is_empty();
        let remplacement = self.add_source(name.clone(), client);
        if premiere && !self.standby {
            self.resume().await?;
        } else {
            self.envoie_locale_a(&name).await;
            self.publie_etat();
        }
        Ok(remplacement)
    }

    /// Pousse la langue courante à **une seule** source : celle qui vient
    /// d'être câblée à chaud.
    ///
    /// `resume` et `set_locale` ne servent que les sources présentes dans la
    /// table au moment de leur appel. Une source arrivée après — greffon qui a
    /// raté le rendez-vous, ou relancé à la main sans son argument de langue —
    /// n'aurait jamais reçu `SetLocale` : sur un appareil en français, un `cd`
    /// relancé revenait en affichant `NO DISC` dans sa ligne de statut, et le
    /// serait resté jusqu'au prochain changement de langue.
    ///
    /// Sans effet si le cœur n'a pas de langue réglée : le greffon garde alors
    /// son défaut, qui est le même que celui du cœur. Best-effort comme les
    /// deux autres chemins — une source qui ne répond pas à `SetLocale` ne doit
    /// pas empêcher son câblage.
    pub async fn envoie_locale_a(&self, name: &str) {
        let Some(locale) = self.locale.clone() else {
            return;
        };
        if let Some(src) = self.sources.get(name) {
            if let Err(e) = src.request(SourceReq::SetLocale(locale)).await {
                tracing::warn!("SetLocale to {name}: {e}");
            }
        }
    }

    /// Remplace l'ordre d'arbitrage des plugins `metadata`.
    ///
    /// Appelé après chaque annonce tardive avec la liste **complète**
    /// recalculée depuis le manifeste : la priorité est celle de
    /// `plugins.toml`, jamais celle d'arrivée des annonces.
    pub fn set_metadata_order(&mut self, ordre: Vec<String>) {
        self.metadonnees.set_ordre(ordre);
    }

    async fn apply(&mut self, action: SourceAction) -> Result<()> {
        match action {
            SourceAction::Noop => {}
            SourceAction::Play { uri, start, finite, playlist } => {
                // La machinerie de relance (`expecting_stream` puis
                // `PlaybackIdle` → retry) n'existe que pour les flux réseau :
                // un contenu qui se termine est une fin normale, pas une
                // panne. Le confondre avec une coupure faisait redémarrer le
                // disque en boucle : fin du disque → mpv idle → relance ~2 s
                // → `Activate` → `Play cdda://` → piste 1.
                //
                // C'est la Source qui le déclare, et non le cœur qui le
                // devine : celui-ci reniflait `cdda://`, si bien qu'un chemin
                // de fichier — mesuré au banc, mpv passant `idle` en fin de
                // liste exactement comme lors d'une coupure — tombait du
                // mauvais côté.
                self.expecting_stream = !finite;
                self.lecture = true;
                // Seul endroit où `lecture` passe à vrai : c'est ici, et
                // nulle part ailleurs, que `paused` doit retomber, sans quoi
                // une pause d'hier rendrait une lecture neuve « en pause ».
                self.paused = false;
                // `loadlist` pour une liste, `loadfile` pour un média : c'est la
                // Source qui le déclare, et le cœur ne le devine pas. Un `.m3u8`
                // est une liste pour un lecteur de fichiers et un flux HLS pour
                // une radio ; renifler l'URI casserait l'un ou l'autre.
                if playlist {
                    self.player.load_list(&uri).await?;
                } else {
                    self.player.play(&uri).await?;
                }
                // Positionnement après le chargement, et cet ordre n'est sûr que
                // grâce à `loadlist` : avec `loadfile`, mpv ne déplie la liste
                // qu'après coup — mesuré — et cet index tombait hors bornes
                // avant que la lecture ne reparte de la première piste.
                if let Some(n) = start {
                    self.player.set_playlist_pos(n).await?;
                }
            }
            SourceAction::Stop => {
                self.expecting_stream = false;
                self.lecture = false;
                self.player.stop().await?;
            }
            SourceAction::PlayerNext => self.player.next().await?,
            SourceAction::PlayerPrev => self.player.prev().await?,
        }
        Ok(())
    }

    /// Applies an output choice from the config page. `None` means "follow
    /// the system default": mpv gets its native `auto` back (settable at
    /// runtime), and nothing is recorded on disk — the same state as a fresh
    /// install, where `resume()` sends no device at all.
    pub async fn set_audio_device(&mut self, device: Option<String>) -> Result<()> {
        match &device {
            Some(d) => self.player.set_audio_device(d).await?,
            None => self.player.set_audio_device("auto").await?,
        }
        self.audio_device = device;
        self.persist();
        Ok(())
    }

    /// Change la langue courante : reconstruit le catalogue partagé du cœur
    /// (lu par la page de statut), persiste l'état, et pousse `SetLocale` à
    /// chaque plugin Source connecté (best-effort).
    ///
    /// Appelée depuis la boucle `select!` de `main` sur réception du canal
    /// `locale_rx`, lui-même alimenté par la route `PUT /api/locale`.
    ///
    /// Résout aussi `standby_status` dans le catalogue tout neuf, et publie
    /// l'état : sans ce dernier, changer de langue pendant la veille laissait
    /// le mot affiché dans l'ancienne langue jusqu'au prochain cycle
    /// `Command::Power` (voir la doc de `standby_status`).
    pub async fn set_locale(&mut self, locale: String) -> Result<()> {
        self.locale = Some(locale.clone());
        let nouveau = Catalog::load("core", &locale, &self.locales_root, crate::i18n::EN);
        self.standby_status = Some(resout_standby_status(&nouveau));
        *self.catalog.write().await = nouveau;
        self.persist();
        for name in self.source_order.clone() {
            if let Some(src) = self.sources.get(&name) {
                if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                    tracing::warn!("SetLocale to {name}: {e}");
                }
            }
        }
        self.publie_etat();
        Ok(())
    }

    /// Change le thème courant et le persiste. Contrairement à `set_locale`,
    /// rien n'est poussé aux plugins : le thème est un réglage d'apparence de
    /// l'IHM web, dont aucun plugin n'a connaissance.
    ///
    /// Appelée depuis la boucle `select!` de `main` sur réception du canal
    /// `theme_rx`, lui-même alimenté par la route `PUT /api/theme`.
    pub fn set_theme(&mut self, t: crate::theme::ThemeState) {
        self.theme = Some(t.theme);
        self.mode = Some(t.mode);
        self.persist();
    }

    fn persist(&self) {
        let st = PersistedState {
            active_source: self.active_source.clone(),
            volume: self.volume,
            standby: self.standby,
            audio_device: self.audio_device.clone(),
            locale: self.locale.clone(),
            theme: self.theme.clone(),
            mode: self.mode.clone(),
            settings: self.settings.clone(),
        };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistence failed: {e}");
        }
    }

    /// Affiche (ou prolonge) l'overlay temporaire volume/muet : ligne 1 le
    /// libellé "volume", ligne 2 le pourcentage courant ou le message
    /// "muet" selon `self.muted`. Chaque appel repousse l'échéance de
    /// `overlay_ms` (une pression de plus garde l'overlay visible).
    ///
    /// `overlay_ms`, distinct de `tens_window_ms` (voir le commentaire de
    /// `Settings`) : cette incrustation masque la vue « en écoute » et
    /// pourrait vouloir raccourcir un jour, sans affecter le temps laissé
    /// pour composer un `+NN`. `expire_overlay` n'a pas besoin de savoir
    /// laquelle des deux durées a posé l'échéance qu'il désarme : elle est
    /// stockée avec le message, dans `self.overlay`.
    async fn show_overlay(&mut self) {
        let mot = if self.muted {
            let cat = self.catalog.read().await;
            cat.get("muted").to_string()
        } else {
            format!("{} %", self.volume)
        };
        let label = self.catalog.read().await.get("volume_label").to_string();
        let echeance = Instant::now() + Duration::from_millis(self.settings.overlay_ms.into());
        self.overlay = Some((
            Overlay::Volume {
                level: self.volume,
                muted: self.muted,
                text: format!("{label} {mot}"),
                remaining_ms: self.settings.overlay_ms,
            },
            echeance,
        ));
    }

    /// Overlay for the pending tens offset ("+10", "+20"): same slot as the
    /// volume overlay, but its own deadline from `tens_window_ms` — the
    /// time left to press the second digit, independent from
    /// `overlay_ms` (see `Settings`). Each press pushes the deadline back,
    /// and `expire_overlay` clears the overlay and the offset together
    /// regardless of which duration is stored here: it reads whatever
    /// deadline is in `self.overlay`, never which field produced it, so
    /// the two stay aligned by construction whatever values the two
    /// settings take.
    async fn show_tens_overlay(&mut self) {
        let label = self.catalog.read().await.get("preset_label").to_string();
        let echeance = Instant::now() + Duration::from_millis(self.settings.tens_window_ms.into());
        self.overlay = Some((
            Overlay::Tens {
                offset: self.pending_tens,
                text: format!("{label} +{}", self.pending_tens),
                remaining_ms: self.settings.tens_window_ms,
            },
            echeance,
        ));
    }

    /// Échéance de l'overlay actif, s'il y en a un (à lire dans `main` avant
    /// le `select!`, à l'image de `retry_at`, pour bâtir la temporisation).
    pub fn overlay_deadline(&self) -> Option<Instant> {
        self.overlay.as_ref().map(|(_, deadline)| *deadline)
    }

    /// Le cœur veut-il être rappelé dans une seconde pour rafraîchir la
    /// position ?
    ///
    /// Armé seulement quand il y a effectivement une position à publier : la
    /// lecture en cours, hors veille, ET (un contenu fini — donc mpv a la
    /// parole sur sa position — OU une ancre posée par un plugin `metadata`).
    /// `!self.standby && self.lecture` seul armait à tort dans deux cas
    /// trouvés en relecture : un flux qu'aucun plugin `metadata` ne suit (rien
    /// ne fournira jamais de position, l'ancre ne se pose jamais) et la pause
    /// (qui ne remet pas `lecture` à faux). Aucune trame n'en ressortait —
    /// `publie_etat` déduplique — mais l'appareil interrogeait mpv deux fois
    /// par seconde indéfiniment, pour rien à afficher.
    pub fn tick_position(&self) -> bool {
        !self.standby && self.lecture && (!self.expecting_stream || self.ancre_position.is_some())
    }

    /// Efface l'overlay expiré et laisse réapparaître l'état permanent
    /// (source, présélection, statut, morceau), tenu à jour entre-temps par
    /// les autres chemins du cœur.
    ///
    /// Seul appelant : la boucle de `main`, sans aucune autre publication
    /// après — contrairement aux commandes, qui publient elles-mêmes à la
    /// sortie de `handle_command`. Un oubli ici ne casse rien à la
    /// compilation, mais l'écran cesse de se mettre à jour à l'expiration.
    pub fn expire_overlay(&mut self) {
        self.overlay = None;
        self.pending_tens = 0;
        self.publie_etat();
    }
}

/// Prochaine échéance du tick de position, à partir de l'état d'armement et
/// de l'échéance courante.
///
/// Fonction pure, et c'est tout son intérêt : la boucle `select!` de `main`
/// n'est couverte par aucun test, et le défaut que cette logique corrige — une
/// échéance **relative**, recréée à chaque tour, qui repartait de zéro à chaque
/// réveil de la boucle et repoussait le tick indéfiniment sur un appareil
/// actif — ne se voit pas en lisant le code appelant.
///
/// `arme` = le cœur veut être rappelé ; `courante` = l'échéance déjà posée,
/// s'il y en a une ; `maintenant` = l'instant de référence, injecté pour que
/// le test n'ait pas d'horloge à attendre.
pub fn prochaine_echeance(arme: bool, courante: Option<Instant>, maintenant: Instant) -> Option<Instant> {
    match (arme, courante) {
        (false, _) => None,
        (true, Some(at)) => Some(at),
        (true, None) => Some(maintenant + Duration::from_secs(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakePlayer {
        calls: Arc<Mutex<Vec<String>>>,
        /// Ce que le lecteur factice prétend savoir de sa progression.
        /// `Mutex` et non champ simple : les tests le règlent après
        /// construction, `Player` ne prenant que `&self`.
        progression: Arc<Mutex<crate::player::Progression>>,
        /// Quand c'est vrai, `toggle_pause` échoue — mpv absent, socket coupé.
        /// Partagé et posé après construction, pour la même raison que
        /// `progression`.
        pause_echoue: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::player::Player for FakePlayer {
        async fn play(&self, uri: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("play {uri}"));
            Ok(())
        }
        async fn load_list(&self, uri: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("load_list {uri}"));
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("stop".into());
            Ok(())
        }
        async fn toggle_pause(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("pause".into());
            if self.pause_echoue.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("mpv injoignable");
            }
            Ok(())
        }
        async fn next(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("next".into());
            Ok(())
        }
        async fn prev(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("prev".into());
            Ok(())
        }
        async fn set_playlist_pos(&self, n: i64) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("playlist-pos {n}"));
            Ok(())
        }
        async fn set_volume(&self, v: u8) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("vol {v}"));
            Ok(())
        }
        async fn set_mute(&self, m: bool) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("mute {m}"));
            Ok(())
        }
        async fn set_audio_device(&self, device: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("audio_device {device}"));
            Ok(())
        }
        async fn progression(&self) -> anyhow::Result<crate::player::Progression> {
            Ok(*self.progression.lock().unwrap())
        }
        async fn seek_relative(&self, delta_s: i64) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("seek_relative {delta_s}"));
            Ok(())
        }
        async fn seek_absolute(&self, position_s: u32) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("seek_absolute {position_s}"));
            Ok(())
        }
    }

    struct FakeSource {
        name: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Source for FakeSource {
        async fn request(&self, req: SourceReq) -> Result<SourceAction> {
            self.calls.lock().unwrap().push(format!("{}:{:?}", self.name, req));
            // Un nom réservé pour simuler un greffon qui ne répond plus :
            // `remove_source` doit rester correct même quand la bascule vers
            // l'entrante échoue, et c'est le seul moyen de le tester sans
            // truquer `FakePlayer`.
            if self.name == "casse" {
                anyhow::bail!("plugin casse ne répond pas");
            }
            Ok(match (self.name, req) {
                ("radio", SourceReq::Activate) => SourceAction::play("http://fip"),
                ("radio", SourceReq::Select(3)) => SourceAction::play("http://inter"),
                ("radio", SourceReq::Select(_)) => SourceAction::Noop,
                // `.finite()` comme le vrai plugin cd : sans cette
                // déclaration, la fin du disque passerait pour une coupure de
                // flux et la relance rejouerait le disque en boucle.
                ("cd", SourceReq::Activate) => SourceAction::play("cdda://").finite(),
                (_, SourceReq::Eject) if self.name == "cd" => SourceAction::Stop,
                ("radio", SourceReq::Wake) => SourceAction::play("http://fip"),
                ("cd", SourceReq::Wake) => SourceAction::Noop,
                _ => SourceAction::Noop,
            })
        }
    }

    /// Alias pour le montage de test (clippy::type_complexity) : cœur factice,
    /// journaux d'appels du lecteur et des sources, récepteur d'état, répertoire temporaire.
    type Montage = (Core<FakePlayer>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>, watch::Receiver<PlayerState>, tempfile::TempDir);

    /// Câblage métadonnées sans observateur : les récepteurs sont lâchés
    /// aussitôt, les `send` du cœur échouent silencieusement (c'est déjà le cas
    /// en production quand aucun plugin `metadata` n'est déclaré). Les tests qui
    /// observent ces canaux utilisent `setup_metadonnees`.
    fn cablage_muet(plugins: Vec<String>) -> MetadataCablage {
        MetadataCablage {
            plugins,
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            etat: watch::channel(PlayerState::default()).0,
        }
    }

    /// Câblage minimal des pochettes pour les montages qui n'en ont pas
    /// l'usage : un cache neuf, et un émetteur dont personne ne lit la
    /// réception (le récepteur est lâché aussitôt — un envoi ultérieur
    /// échoue alors en silence, ce que `lance_pochette` ignore déjà).
    fn covers_de_test() -> (Arc<crate::cover::CoverCache>, mpsc::Sender<(String, bool)>) {
        (Arc::new(crate::cover::CoverCache::new()), mpsc::channel(4).0)
    }

    /// Mise à jour ne portant rien : tous les champs à `None`/`false`. Base
    /// commode pour composer une trame minimale dans un test (voir les tests
    /// de statut).
    fn update_nu() -> SourceUpdate {
        SourceUpdate::default()
    }

    /// Mise à jour ne portant qu'une identité.
    fn joue(identity: serde_json::Value) -> SourceUpdate {
        SourceUpdate {
            identity: Some(IdentityUpdate::Playing(identity)),
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: None,
            can_eject: None,
            presets: None,
            cover: None,
        }
    }

    /// Une présélection nommée, forme courte pour les tests.
    fn pres(index: u8, name: &str) -> Preset {
        Preset { index, name: name.into() }
    }

    /// Trame ne portant **que** des présélections nommées : c'est exactement la
    /// forme sous laquelle la réponse à `ListPresets` atteint le cœur, l'action
    /// corrélée (`Noop`) partant par l'autre voie.
    fn avec_presets(presets: Vec<Preset>) -> SourceUpdate {
        let mut u = update_nu();
        u.presets = Some(presets);
        u
    }

    /// Les noms d'un catalogue, dans l'ordre où il les porte.
    fn noms(cat: &Catalogue) -> Vec<String> {
        cat.sources.iter().map(|s| s.name.clone()).collect()
    }

    fn setup() -> Montage {
        setup_persiste(PersistedState::default())
    }

    /// `setup` with a say on what `state.json` held at launch — what
    /// `StartupPower::Previous` reads.
    fn setup_persiste(persisted: PersistedState) -> Montage {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let core = Core::new(
            player,
            Cablage {
                sources,
                persisted,
                state_path: dir.path().join("state.json"),
                catalog,
                locales_root: root,
                catalogue: watch::channel(Catalogue::default()).0,
                metadata: MetadataCablage {
                    plugins: vec![],
                    now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
                    etat: etat_tx,
                },
            },
            covers,
            pochette_tx,
            mpsc::channel(4).0,
        );
        (core, player_calls, source_calls, etat_rx, dir)
    }

    /// Montage observant les deux canaux de métadonnées : ce qui descend vers
    /// les plugins, et l'état structuré qui monte vers la SPA et les afficheurs.
    ///
    /// `plugins` porte l'ordre de déclaration, donc la priorité d'arbitrage.
    #[allow(clippy::type_complexity)]
    fn setup_metadonnees(
        plugins: Vec<String>,
    ) -> (Core<FakePlayer>, watch::Receiver<NowPlaying>, watch::Receiver<PlayerState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls }));
        let (np_tx, np_rx) = watch::channel(NowPlaying { source: "radio".into(), identity: None, ..Default::default() });
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let core = Core::new(
            FakePlayer::default(),
            Cablage {
                sources,
                persisted: PersistedState::default(),
                state_path: dir.path().join("state.json"),
                catalog,
                locales_root: root,
                catalogue: watch::channel(Catalogue::default()).0,
                metadata: MetadataCablage { plugins, now_playing: np_tx, etat: etat_tx },
            },
            covers,
            pochette_tx,
            mpsc::channel(4).0,
        );
        (core, np_rx, etat_rx, dir)
    }

    /// Alias de `setup_metadonnees(vec![])` : les tests de l'état partiel
    /// n'ont besoin d'aucun greffon `metadata`, seulement du montage que
    /// `setup_metadonnees` sait déjà construire.
    fn core_de_test() -> (Core<FakePlayer>, watch::Receiver<NowPlaying>, watch::Receiver<PlayerState>, tempfile::TempDir) {
        setup_metadonnees(vec![])
    }

    /// Comme `core_de_test`, mais **garde** le récepteur du canal
    /// d'extraction de pochette embarquée plutôt que de le lâcher.
    ///
    /// Nécessaire pour tout test qui laisse réellement tourner la tâche
    /// détachée de `handle_path` sur un vrai fichier : celle-ci est l'unique
    /// écrivaine légitime du fichier temporaire, et un test qui relirait les
    /// tags une seconde fois de son côté (pour reconstituer le `CoverRef`
    /// attendu) écrirait en concurrence avec elle sur le même chemin — une
    /// vraie course entre deux écrivains, découverte à l'usage (voir le
    /// rapport de tâche 6, ruling 1 de la revue).
    #[allow(clippy::type_complexity)]
    fn core_de_test_avec_extraction() -> (
        Core<FakePlayer>,
        watch::Receiver<PlayerState>,
        mpsc::Receiver<(String, Option<ritornello_proto::CoverRef>)>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls }));
        let (np_tx, _np_rx) =
            watch::channel(NowPlaying { source: "radio".into(), identity: None, ..Default::default() });
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let (extraction_tx, extraction_rx) = mpsc::channel(4);
        let core = Core::new(
            FakePlayer::default(),
            Cablage {
                sources,
                persisted: PersistedState::default(),
                state_path: dir.path().join("state.json"),
                catalog,
                locales_root: root,
                catalogue: watch::channel(Catalogue::default()).0,
                metadata: MetadataCablage { plugins: vec![], now_playing: np_tx, etat: etat_tx },
            },
            covers,
            pochette_tx,
            extraction_tx,
        );
        (core, etat_rx, extraction_rx, dir)
    }

    impl Core<FakePlayer> {
        /// Règle ce que le lecteur factice prétend savoir de sa progression.
        fn regle_progression(&self, position_s: Option<f64>, duration_s: Option<f64>) {
            *self.player.progression.lock().unwrap() =
                crate::player::Progression { position_s, duration_s };
        }

        /// Recule l'ancre de `duree` : le test avance le temps sans dormir.
        fn avance_ancre_pour_test(&mut self, duree: std::time::Duration) {
            if let Some((p, pose)) = self.ancre_position {
                self.ancre_position = Some((p, pose - duree));
            }
        }
    }

    #[tokio::test]
    async fn resume_active_la_source_persistee() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Wake"));
    }

    #[tokio::test]
    async fn resume_envoie_wake_pas_activate() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:Wake"));
        assert!(!calls.iter().any(|c| c == "radio:Activate"));
    }

    #[test]
    fn active_source_retourne_la_source_courante() {
        let (core, _pc, _sc, _rx, _d) = setup();
        // PersistedState::default().active_source == "radio".
        assert_eq!(core.active_source(), "radio");
    }

    /// Cœur sans aucune source : le démarrage où *aucune* n'a répondu. C'est
    /// exactement la situation dont le câblage à chaud doit pouvoir sortir, et
    /// celle que le cœur doit désormais savoir servir — la page de statut est là
    /// pour montrer les greffons figés.
    ///
    /// Le récepteur d'état est rendu (et non lâché comme dans `cablage_muet`) :
    /// « aucune source » est un état à observer, pas seulement à survivre.
    fn setup_sans_source() -> (Core<FakePlayer>, watch::Receiver<PlayerState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
            "core",
            "en",
            &root,
            crate::i18n::EN,
        )));
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let (covers, pochette_tx) = covers_de_test();
        let core = Core::new(
            FakePlayer::default(),
            Cablage {
                sources: HashMap::new(),
                persisted: PersistedState::default(),
                state_path: dir.path().join("state.json"),
                catalog,
                locales_root: root,
                catalogue: watch::channel(Catalogue::default()).0,
                metadata: MetadataCablage {
                    plugins: vec![],
                    now_playing: watch::channel(NowPlaying {
                        source: String::new(),
                        identity: None,
                        ..Default::default()
                    })
                    .0,
                    etat: etat_tx,
                },
            },
            covers,
            pochette_tx,
            mpsc::channel(4).0,
        );
        (core, etat_rx, dir)
    }

    #[test]
    fn add_source_retrie_lordre_du_cycle_au_lieu_dajouter_en_queue() {
        // `SourceCycle` suit l'ordre alphabétique. Une source arrivée en retard
        // qui resterait en queue ferait changer le sens du cycle selon la
        // chronologie du démarrage — l'utilisateur presserait la même touche et
        // n'obtiendrait pas la même source d'un jour à l'autre.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let nouvelle = Arc::new(FakeSource { name: "files", calls: source_calls });
        assert!(!core.add_source("files".into(), nouvelle), "ce n'est pas un remplacement");
        assert_eq!(core.source_order, vec!["cd".to_string(), "files".into(), "radio".into()]);
        assert_eq!(
            core.active_source(),
            "radio",
            "une source deja active ne doit pas etre supplantee par une arrivee tardive"
        );
    }

    #[test]
    fn add_source_signale_un_remplacement_sans_dupliquer_lordre() {
        // Ré-annonce d'un greffon déjà câblé : le client est remplacé, le cycle
        // ne gagne pas une entrée en double.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let remplacant = Arc::new(FakeSource { name: "radio", calls: source_calls });
        assert!(core.add_source("radio".into(), remplacant));
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
        assert_eq!(core.active_source(), "radio");
    }

    #[test]
    fn add_source_active_la_premiere_source_et_seulement_la_premiere() {
        // Le seul cas où l'arrivée d'un greffon change ce qui joue : aucune
        // source n'avait répondu au démarrage, donc rien n'était actif.
        let (mut core, _rx, dir) = setup_sans_source();
        assert_eq!(core.active_source(), "");
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.add_source("radio".into(), Arc::new(FakeSource { name: "radio", calls: calls.clone() }));
        assert_eq!(core.active_source(), "radio");
        // La deuxième n'y touche pas, même si son nom passe avant dans l'ordre.
        core.add_source("cd".into(), Arc::new(FakeSource { name: "cd", calls }));
        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
        drop(dir);
    }

    #[tokio::test]
    async fn remove_source_bascule_sur_la_suivante() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        assert_eq!(core.active_source(), "radio");

        assert!(core.remove_source("radio").await.unwrap());

        assert_eq!(core.active_source(), "cd", "la suivante du cycle prend la place");
        assert_eq!(core.source_order, vec!["cd".to_string()]);
        let calls = source_calls.lock().unwrap();
        assert!(
            calls.iter().any(|c| c == "radio:Deactivate"),
            "la sortante est prévenue avant de disparaître : {calls:?}"
        );
        assert!(calls.iter().any(|c| c == "cd:Activate"), "l'entrante est activée : {calls:?}");
    }

    #[tokio::test]
    async fn une_reponse_de_preselections_en_retard_ne_ressuscite_pas_une_source_retiree() {
        // La course : `ListPresets` est détaché, donc sa réponse peut arriver
        // après l'extinction du greffon. Sans protection, elle réinsérait
        // l'entrée que `remove_source` venait d'évincer — et le catalogue
        // recommençait à annoncer à un client MPD une liste enregistrée sur
        // laquelle il pouvait agir. C'est exactement le défaut que l'éviction
        // existe pour empêcher.
        //
        // **Ce qui protège est le retour anticipé en tête de
        // `handle_source_update`** (`!self.sources.contains_key(name)`), et non
        // une garde posée près de l'insertion. Ce test existe parce que rien ne
        // l'épinglait : le retour anticipé est arrivé pour la trame *entière*,
        // et sa doc décrit bien ce cas, mais aucune assertion ne l'aurait
        // empêché de disparaître. Vérifié par mutation : le retirer fait tomber
        // ce test.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(core.remove_source("radio").await.unwrap());
        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string()]);

        // La réponse en retard arrive maintenant, sur un nom que le cœur ne
        // câble plus.
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(5, "OUI FM")]));

        assert!(
            !core.presets_par_source.contains_key("radio"),
            "une source retirée ne doit pas revenir par une réponse en vol"
        );
        assert_eq!(
            noms(&core.catalogue()),
            vec!["cd".to_string()],
            "et le catalogue ne doit pas la réannoncer"
        );
    }

    #[tokio::test]
    async fn retirer_une_source_la_sort_du_catalogue_avec_ses_preselections() {
        // Fusion des deux chantiers : `remove_source` (extinction à chaud d'un
        // greffon) est arrivé par un côté, `presets_par_source` et le canal de
        // catalogue par l'autre — et rien ne les reliait. Laissée en place,
        // l'entrée faisait figurer dans le `listplaylists` d'un client MPD une
        // source éteinte, sur laquelle il pouvait **agir** : le `load` serait
        // refusé par le garde de `SelectSource`, mais l'utilisateur verrait une
        // liste qui mente jusqu'au redémarrage, les clients MPD mettant
        // volontiers ce catalogue en cache.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(5, "OUI FM")]));
        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string(), "radio".into()]);

        assert!(core.remove_source("radio").await.unwrap());

        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string()], "la source sort du catalogue");
        assert!(
            !core.presets_par_source.contains_key("radio"),
            "ses présélections partent avec elle : un greffon rallumé sous le même nom \
             doit attendre son propre ListPresets, pas hériter de sa vie précédente"
        );
    }

    #[tokio::test]
    async fn une_source_disparue_ne_recoit_plus_de_bascule_et_sort_du_catalogue() {
        // **Le danger commun aux deux chemins de disparition d'un greffon.** Un
        // greffon disparu qui laissait son nom dans `source_order` et ses
        // présélections dans `presets_par_source` faisait garder à un client MPD
        // sa liste enregistrée en cache, et un `load` dessus **passait** le garde
        // de `SelectSource`. La bascule partait alors vers un socket mort et
        // payait jusqu'à deux délais de 5 s du protocole des sources —
        // `Deactivate` puis `Activate` — dans la boucle principale, muette
        // pendant ce temps. Ce test-ci prend le chemin volontaire
        // (`remove_source`) ; son jumeau juste en dessous prend celui de la mort
        // subie (`oublie_source_morte`), et c'est leur *différence* qui est
        // épinglée là-bas.
        //
        // Le test épingle les deux moitiés à la suite : la sortie du catalogue,
        // et le fait qu'un `SelectSource` sur ce nom ne parle plus à personne.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(noms(&core.catalogue()).contains(&"radio".to_string()));

        // Ce que fait le bras `plugin_waits` quand la mort n'était pas voulue.
        assert!(core.remove_source("radio").await.unwrap());
        // La bascule vers « cd » a déjà eu lieu et a parlé : on ne veut observer
        // que ce qui suit.
        source_calls.lock().unwrap().clear();

        // Ce qu'un client MPD envoie encore, son catalogue étant en cache.
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();

        let appels = source_calls.lock().unwrap().clone();
        assert!(
            appels.is_empty(),
            "aucune requete ne doit partir apres la disparition de la source, obtenu {appels:?}"
        );
        assert_eq!(core.active_source(), "cd", "et ce qui joue n'a pas bouge");
        assert!(
            !noms(&core.catalogue()).contains(&"radio".to_string()),
            "la source disparue ne doit plus figurer au catalogue"
        );
        assert!(!core.presets_par_source.contains_key("radio"));
    }

    #[tokio::test]
    async fn la_mort_subie_du_greffon_actif_evince_sans_arreter_la_musique_ni_changer_de_source() {
        // **La décision du constat 3, épinglée.** Le bras de sortie de processus
        // appelait `remove_source`, qui bascule quand c'était l'active : une
        // panique du greffon radio arrêtait donc mpv et affichait « cd » sur un
        // appareil dont l'utilisateur avait choisi la radio. Or un greffon de
        // Source est un *contrôleur* — le flux est tenu par mpv, enfant du cœur,
        // que la mort du greffon ne touche pas.
        //
        // Trois propriétés dans un seul test, parce que c'est leur conjonction
        // qui est la décision : rien ne s'arrête, rien ne bascule, et le
        // catalogue oublie quand même.
        let (mut core, player_calls, source_calls, etat_rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // la radio joue
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert_eq!(etat_rx.borrow().playback, Playback::Playing);
        player_calls.lock().unwrap().clear();
        source_calls.lock().unwrap().clear();

        assert!(core.oublie_source_morte("radio"));

        assert_eq!(
            core.active_source(),
            "radio",
            "personne n'a demande de changer de source : le nom affiche doit rester celui \
             que l'utilisateur a choisi, greffon mort ou non"
        );
        assert_eq!(
            etat_rx.borrow().playback,
            Playback::Playing,
            "la panne d'un controleur ne doit pas faire taire mpv, qui n'est pas dans le greffon"
        );
        assert!(
            player_calls.lock().unwrap().is_empty(),
            "aucun ordre au lecteur : obtenu {:?}",
            player_calls.lock().unwrap()
        );
        assert!(
            source_calls.lock().unwrap().is_empty(),
            "ni Deactivate ni Activate : le pair est mort et l'autre source n'a rien demande, \
             obtenu {:?}",
            source_calls.lock().unwrap()
        );
        // Et l'eviction, elle, a bien eu lieu : c'est la moitie commune aux deux
        // chemins.
        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string()]);
        assert!(!core.presets_par_source.contains_key("radio"));
        // Les capacites de la source morte sont oubliees : une touche Eject
        // allumee ou une grille de preselections ouverte proposeraient des
        // commandes qui ne peuvent plus aboutir.
        assert!(!etat_rx.borrow().can_eject);
        assert_eq!(etat_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn apres_la_mort_de_la_source_active_la_touche_source_repart_de_la_premiere() {
        // Le corollaire de la décision ci-dessus : `active_source` ne figure plus
        // dans `source_order`, et `SourceCycle` doit quand même mener quelque
        // part d'utile. Un `position().unwrap_or(0)` suivi d'un `+ 1` sautait la
        // première source, qui devenait inatteignable au clavier.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let files = Arc::new(FakeSource { name: "files", calls: source_calls });
        core.add_source("files".into(), files);
        assert_eq!(core.source_order, vec!["cd".to_string(), "files".into(), "radio".into()]);
        assert!(core.oublie_source_morte("radio"));

        core.handle_command(Command::SourceCycle).await.unwrap();

        assert_eq!(core.active_source(), "cd", "la premiere source restante, pas la seconde");
    }

    #[tokio::test]
    async fn une_reponse_de_catalogue_encore_en_vol_ne_ressuscite_pas_une_source_evincee() {
        // Le fan-out des `ListPresets` est **détaché** : la requête part dans sa
        // propre tâche, et `remove_source` peut s'exécuter entre elle et sa
        // réponse. Cette réponse-là arrive donc pour de vrai après l'éviction, et
        // `presets_par_source.insert` se fait délibérément **avant** le garde de
        // source active (le catalogue décrit toutes les sources, pas celle qui
        // joue) : la liste était donc ré-insérée après coup, le catalogue
        // republié annonçait une liste enregistrée pour une source qui n'existe
        // plus, et un client MPD pouvait `load` dessus.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(core.remove_source("radio").await.unwrap());
        assert!(!noms(&core.catalogue()).contains(&"radio".to_string()));

        // La réponse en vol, telle que le `SourceClient` la relaie : une liste
        // non vide, sans identité ni statut — la forme exacte qu'une trame de
        // `ListPresets` prend sur le fil.
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(5, "OUI FM")]));

        assert!(
            !core.presets_par_source.contains_key("radio"),
            "une reponse pour une source que le coeur ne connait plus doit etre jetee"
        );
        assert!(
            !noms(&core.catalogue()).contains(&"radio".to_string()),
            "et le catalogue ne doit pas la faire reapparaitre"
        );
    }

    #[tokio::test]
    async fn une_reponse_de_catalogue_pour_une_source_inactive_mais_vivante_est_toujours_prise() {
        // Le pendant du test ci-dessus, et il est nécessaire : un garde trop
        // large aurait aussi jeté les listes des sources **vivantes mais non
        // actives**, ce qui est justement le cas que `presets_par_source` existe
        // pour servir — `listplaylistinfo "radio"` pendant que le cd joue.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(core.active_source(), "cd");

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));

        assert_eq!(
            core.presets_par_source.get("radio").map(|p| p.len()),
            Some(1),
            "la source n'est pas active, mais elle existe : sa liste doit entrer au catalogue"
        );
    }

    #[tokio::test]
    async fn le_catalogue_est_republie_quand_une_source_est_retiree() {
        // Le retrait ne suffit pas : sans la publication, les afficheurs déjà
        // connectés garderaient la version précédente du catalogue — le canal
        // étant `watch`, personne ne la redemande.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut cat_rx = core.catalogue_tx.subscribe();
        cat_rx.borrow_and_update();

        assert!(core.remove_source("radio").await.unwrap());

        assert!(cat_rx.has_changed().unwrap(), "le canal du catalogue doit avoir bougé");
        assert_eq!(noms(&cat_rx.borrow_and_update()), vec!["cd".to_string()]);
    }

    #[tokio::test]
    async fn desactiver_la_source_active_republie_letat_sans_les_reliquats_de_la_sortante() {
        // Fix de revue finale : `bascule_source` est emprunté par
        // `remove_source` (donc par la désactivation à chaud d'un greffon)
        // en dehors de `handle_command`, seul endroit qui publiait jusqu'ici.
        // Sans un `publie_etat` propre à `bascule_source`, la trame reçue par
        // la SPA et les afficheurs continuait de nommer la source sortante,
        // avec son compte de présélections, son statut et sa capacité
        // d'éjection.
        let (mut core, _pc, _sc, etat_rx, _d) = setup();
        core.handle_source_update(
            "radio",
            SourceUpdate {
                identity: Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream"}))),
                transient: false,
                preset: Some(3),
                preset_count: Some(23),
                preset_name: Some("France Inter".into()),
                status: Some("EN DIRECT".into()),
                can_eject: Some(true),
                presets: None,
                cover: None,
            },
        );
        assert_eq!(etat_rx.borrow().source, "radio");
        assert_eq!(etat_rx.borrow().preset_count, Some(23));
        assert!(etat_rx.borrow().can_eject);

        assert!(core.remove_source("radio").await.unwrap());

        let etat = etat_rx.borrow();
        assert_eq!(etat.source, "cd", "la trame doit nommer l'entrante, pas la sortante");
        assert_eq!(etat.preset_count, None, "le compte de preselections de la sortante ne doit pas survivre");
        assert_eq!(etat.status, None, "le statut de la sortante ne doit pas survivre");
        assert!(!etat.can_eject, "la capacite d'ejection decrit la sortante, pas l'entrante");
    }

    #[tokio::test]
    async fn remove_source_de_la_derniere_laisse_le_coeur_sans_source() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(core.remove_source("cd").await.unwrap());
        assert!(core.remove_source("radio").await.unwrap());

        // Aucune source est un état légitime : `demande_active` le tolère, et
        // démarrer sans source est accepté depuis l'enregistrement à chaud.
        assert_eq!(core.active_source(), "");
        assert!(core.source_order.is_empty());
        // Et une commande dans cet état ne panique pas.
        core.handle_input(InputMessage::from(Command::Next)).await.unwrap();
    }

    #[tokio::test]
    async fn remove_source_dune_source_inactive_ne_touche_pas_a_ce_qui_joue() {
        let (mut core, player_calls, _sc, _rx, _d) = setup();

        assert!(core.remove_source("cd").await.unwrap());

        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["radio".to_string()]);
        assert!(
            !player_calls.lock().unwrap().iter().any(|c| c == "stop"),
            "retirer une source inactive n'arrête pas ce qui joue"
        );
    }

    #[tokio::test]
    async fn remove_source_dun_nom_inconnu_est_un_non_evenement() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(!core.remove_source("jamais-vu").await.unwrap());
        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
    }

    #[tokio::test]
    async fn remove_source_reste_complet_quand_lentrante_echoue_a_lactivation() {
        // Retirer la source active bascule vers la suivante du cycle ; ici la
        // suivante est "casse", dont `Activate` échoue systématiquement (voir
        // `FakeSource::request`). Le retrait doit malgré tout être complet :
        // un greffon qu'on éteint ne doit jamais rester à moitié câblé, avec
        // un `SourceCycle` qui pourrait retomber sur un processus déjà tué.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.add_source("casse".into(), Arc::new(FakeSource { name: "casse", calls: source_calls }));
        assert_eq!(core.source_order, vec!["casse".to_string(), "cd".into(), "radio".into()]);
        assert_eq!(core.active_source(), "radio");

        assert!(
            core.remove_source("radio").await.unwrap(),
            "le retrait a bien lieu malgré l'échec de la bascule vers l'entrante"
        );

        assert!(
            !core.sources.contains_key("radio"),
            "la source tuée ne doit plus figurer dans la table, même si la bascule a échoué"
        );
        assert!(!core.source_order.contains(&"radio".to_string()));
        assert_ne!(
            core.active_source(),
            "radio",
            "le cœur ne doit plus nommer une source qu'il vient de retirer de sa table"
        );
    }

    #[tokio::test]
    async fn une_source_cablee_a_chaud_recoit_la_langue_courante() {
        // `resume` et `set_locale` ne servent que les sources présentes dans la
        // table au moment de leur appel. Sans ce chemin-là, une source arrivée
        // après n'aurait jamais reçu `SetLocale` : sur un appareil en français,
        // un `cd` relancé à la main revenait en affichant `NO DISC`.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.set_locale("fr".into()).await.unwrap();

        let tardives: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud(
            "files".into(),
            Arc::new(FakeSource { name: "files", calls: tardives.clone() }),
        )
        .await
        .unwrap();

        // La langue, et **rien d'autre** : `files` n'est pas la première source
        // du cœur, donc elle n'est pas réveillée — ce qui joue ne change pas
        // parce qu'un greffon a fini de démarrer.
        assert_eq!(
            tardives.lock().unwrap().as_slice(),
            ["files:SetLocale(\"fr\")".to_string()]
        );
        assert_eq!(core.active_source(), "radio");
        assert_eq!(
            source_calls.lock().unwrap().iter().filter(|c| c.starts_with("radio:SetLocale")).count(),
            1,
            "seule la source cablee a chaud est concernee, les autres ne sont pas renotifiees"
        );
    }

    #[tokio::test]
    async fn sans_langue_reglee_rien_nest_pousse_a_la_source_cablee_a_chaud() {
        // Aucune langue côté cœur : le greffon garde son défaut, qui est le
        // même. Pousser `SetLocale(None)` n'existe pas, et pousser « en » de
        // force écraserait un greffon lancé avec sa propre langue.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let tardives: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud(
            "files".into(),
            Arc::new(FakeSource { name: "files", calls: tardives.clone() }),
        )
        .await
        .unwrap();
        assert!(tardives.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resume_sans_aucune_source_publie_letat_au_lieu_de_paniquer() {
        // Le premier appelant de la source active au démarrage, et donc le
        // premier à mourir : `active()` paniquait sur une table vide, et `resume`
        // tourne avant que le serveur web n'ait servi une seule page. Un
        // `panic!` là aurait supprimé la page de statut précisément quand on
        // voulait y voir les greffons figés.
        let (mut core, mut etat_rx, dir) = setup_sans_source();
        core.resume().await.unwrap();
        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(etat.source, "", "la chaine vide EST l'absence, c'est au rendu de la nommer");
        assert!(!etat.standby, "le coeur demarre, il n'entre pas en veille pour autant");
        drop(dir);
    }

    #[tokio::test]
    async fn les_commandes_sans_aucune_source_ne_font_rien_et_ne_paniquent_pas() {
        // Les treize requêtes à la source active passaient par le même
        // `panic!` : la moindre touche de télécommande sur un appareil sans
        // source arrêtait le cœur. Sans source, une commande **ne fait rien**,
        // et le journal le dit en `debug` — ce n'est pas une anomalie.
        let (mut core, _rx, dir) = setup_sans_source();
        for cmd in [
            Command::Select(1),
            Command::Next,
            Command::Prev,
            Command::Eject,
            Command::Stop,
            Command::PlayPause,
            Command::SourceCycle,
            // Veille, puis réveil : le second repasse par `resume`.
            Command::Power,
            Command::Power,
        ] {
            let libelle = format!("{cmd:?}");
            core.handle_command(cmd).await.unwrap_or_else(|e| panic!("{libelle}: {e}"));
        }
        // Les deux événements du lecteur qui notifient la source, et la relance
        // de flux : mêmes appels, même table vide.
        core.handle_event(Event::TrackChanged(2)).await;
        core.handle_event(Event::PlaybackIdle).await;
        core.retry_stream().await.unwrap();
        assert_eq!(core.active_source(), "", "aucune commande n'a pu designer une source");
        drop(dir);
    }

    #[tokio::test]
    async fn la_premiere_source_cablee_a_chaud_est_reveillee() {
        // `add_source` ne fait que désigner l'active : ni `SetLocale`, ni `Wake`,
        // ni `Activate`. Une source arrivée à t+30 s serait donc active et
        // **muette** jusqu'à ce que l'utilisateur touche quelque chose —
        // l'appareil aurait l'air en panne alors que tout est câblé.
        let (mut core, mut etat_rx, dir) = setup_sans_source();
        core.set_locale("fr".into()).await.unwrap();
        let vus: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        assert!(
            !core
                .cable_source_a_chaud(
                    "radio".into(),
                    Arc::new(FakeSource { name: "radio", calls: vus.clone() })
                )
                .await
                .unwrap(),
            "premier cablage, pas un remplacement"
        );

        assert_eq!(
            vus.lock().unwrap().as_slice(),
            ["radio:SetLocale(\"fr\")".to_string(), "radio:Wake".into()],
            "la langue AVANT le reveil, exactement comme au demarrage"
        );
        // Le `Play` renvoyé par `Wake` a bien été appliqué : quelque chose joue.
        assert!(core.player.calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert_eq!(etat_rx.borrow_and_update().source, "radio");
        drop(dir);
    }

    #[tokio::test]
    async fn la_premiere_source_cablee_a_chaud_ne_reveille_pas_un_coeur_en_veille() {
        // La veille est un état **voulu** : l'arrivée d'un greffon ne rallume pas
        // l'appareil. Seule la langue est due, pour que la source ne compose pas
        // sa première trame dans la langue de son lancement.
        let (mut core, _rx, dir) = setup_sans_source();
        core.set_locale("fr".into()).await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        let vus: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud(
            "radio".into(),
            Arc::new(FakeSource { name: "radio", calls: vus.clone() }),
        )
        .await
        .unwrap();

        assert_eq!(vus.lock().unwrap().as_slice(), ["radio:SetLocale(\"fr\")".to_string()]);
        assert!(
            !core.player.calls.lock().unwrap().iter().any(|c| c.starts_with("play")),
            "rien ne doit se mettre a jouer pendant la veille"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn un_metadata_tardif_prend_sa_place_du_manifeste_dans_larbitrage() {
        // L'invariant le plus facile à casser du câblage à chaud : la priorité
        // est celle de `plugins.toml`, jamais celle d'arrivée des annonces.
        // Seul `musicbrainz` s'est annoncé à temps ; `ouifm` arrive après le
        // démarrage alors que le manifeste le déclare **avant** lui. Un ajout en
        // queue le ferait perdre l'arbitrage, et la priorité dépendrait de la
        // chronologie du démarrage.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["musicbrainz".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("musicbrainz", enrichissement(id.clone(), "Base", "En ligne"));
        assert_eq!(etat_rx.borrow().morceau.artist.as_deref(), Some("Base"));

        // Ce que fait `main` à la réception d'une annonce tardive : recalculer la
        // liste **complète** depuis le manifeste, puis la remettre au cœur. La
        // logique d'ordre reste dans `register::metadata_order`, un seul endroit.
        let manifeste = vec!["ouifm".to_string(), "musicbrainz".to_string()];
        let mut rassemble = crate::register::Gathered::default();
        for nom in ["musicbrainz", "ouifm"] {
            rassemble.announcements.insert(
                nom.to_string(),
                ritornello_proto::Announcement {
                    name: nom.to_string(),
                    kinds: vec![ritornello_proto::PluginKind::Metadata],
                    admin: false,
                    covers: false,
                },
            );
        }
        core.set_metadata_order(crate::register::metadata_order(&manifeste, &rassemble));

        core.handle_enrichment("ouifm", enrichissement(id, "Station", "Direct"));
        assert_eq!(
            core.metadonnees.gagnant(),
            Some("ouifm"),
            "le tardif est declare avant dans le manifeste : il doit gagner"
        );
        assert_eq!(etat_rx.borrow().morceau.artist.as_deref(), Some("Station"));
    }

    #[test]
    fn en_embarque_du_coeur_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(crate::i18n::EN).unwrap().is_empty());
    }

    #[tokio::test]
    async fn select_relaye_a_la_source_active_sans_changer_active_source() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://inter".to_string()));
        // Select agit sur la source deja active ; seul SourceCycle change active_source.
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "radio");
    }

    #[tokio::test]
    async fn source_cycle_bascule_et_persiste() {
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Deactivate"));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Activate"));
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
    }

    #[tokio::test]
    async fn le_cycle_de_source_se_comporte_exactement_comme_avant_lextraction() {
        // Filet de l'extraction : le corps a change de fonction, pas de sens.
        // Memes assertions que `source_cycle_bascule_et_persiste`, la preuve
        // que basculer_vers rejoue exactement le comportement du bloc qu'elle
        // remplace.
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Deactivate"));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Activate"));
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
    }

    #[tokio::test]
    async fn la_source_par_son_nom_bascule_comme_le_cycle() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        assert_eq!(core.active_source(), "cd");
    }

    #[tokio::test]
    async fn une_source_inconnue_est_ignoree_sans_rien_couper() {
        // La garde qui compte : sans elle, un nom errant viderait la source active.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SelectSource("nexistepas".into())).await.unwrap();
        assert_eq!(core.active_source(), "radio");
    }

    #[tokio::test]
    async fn selectionner_la_source_deja_active_ne_coupe_pas_ce_qui_joue() {
        // C'est exactement ce qu'un client MPD envoie en rouvrant son ecran : un
        // `load` redondant ne doit pas arreter la lecture.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        // La bascule complete (stop puis Activate) ramenerait aussi a `Playing`
        // pour cette source factice : le champ `playback` seul ne distingue pas
        // un redondant traite en no-op d'un redondant qui a coupe puis relance.
        // L'absence de tout nouvel appel `stop` est la preuve qui bite.
        player_calls.lock().unwrap().clear();
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        assert!(
            !player_calls.lock().unwrap().iter().any(|c| c == "stop"),
            "un load redondant ne doit meme pas arreter puis relancer mpv"
        );
    }

    #[tokio::test]
    async fn standby_bloque_tout_sauf_power() {
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
        core.handle_command(Command::Select(3)).await.unwrap();
        // aucun nouvel appel "play" apres la veille tant qu'on n'a pas fait Power a nouveau
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 1);
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 2);
    }

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
    async fn resume_applique_la_sortie_audio_persistee() {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let persisted = PersistedState {
            active_source: "radio".into(),
            volume: 60,
            standby: false,
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            theme: None,
            mode: None,
            settings: crate::state::Settings::default(),
        };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device bluealsa:DEV=XX".to_string()));
    }

    #[tokio::test]
    async fn set_audio_device_applique_et_persiste() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device(Some("hw:CARD=Headphones".into())).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device hw:CARD=Headphones".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device.as_deref(), Some("hw:CARD=Headphones"));
    }

    #[tokio::test]
    async fn set_audio_device_none_revient_au_defaut_systeme() {
        // "System default" from the config page: nothing imposed on mpv
        // anymore (its native `auto`), and no device recorded on disk.
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device(Some("hw:CARD=Headphones".into())).await.unwrap();
        core.set_audio_device(None).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device auto".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device, None);
    }

    /// Extrait le délai d'un `RetryIn`, ou échoue en nommant ce qui est arrivé.
    fn relance(outcome: EventOutcome) -> Duration {
        match outcome {
            EventOutcome::RetryIn(d) => d,
            autre => panic!("attendu RetryIn, obtenu {autre:?}"),
        }
    }

    #[tokio::test]
    async fn stop_intentionnel_ne_declenche_pas_de_retry() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
    }

    #[tokio::test]
    async fn backoff_croissant_puis_reinitialise_par_un_titre() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        let d1 = relance(core.handle_event(Event::PlaybackIdle).await);
        let d2 = relance(core.handle_event(Event::PlaybackIdle).await);
        assert!(d2 > d1);
        // Un titre atteste la vivacité du flux : c'est aussi le verdict que la
        // boucle de `main` suit pour annuler l'échéance de relance.
        assert_eq!(core.handle_event(Event::Title("ok".into())).await, EventOutcome::StreamAlive);
        let d3 = relance(core.handle_event(Event::PlaybackIdle).await);
        assert_eq!(d3, d1);
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
    async fn le_mot_de_veille_est_traduit_par_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (etat_tx, mut etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "fr", &root, crate::i18n::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            etat: etat_tx,
        };
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("VEILLE"));
    }

    #[tokio::test]
    async fn changer_de_langue_en_veille_republie_aussitot_le_mot_de_veille() {
        // Régression (M1+M9, revue de branche) : le mot de veille n'était
        // résolu qu'au moment de poser la veille (`Command::Power`), et
        // `set_locale` ne publiait de toute façon aucun état. Changer de
        // langue *pendant* la veille laissait donc le mot affiché dans
        // l'ancienne langue jusqu'au prochain cycle Power.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (etat_tx, mut etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        // Construction en anglais : "STANDBY", la valeur embarquée de la clé.
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            etat: etat_tx,
        };
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));
        core.set_locale("fr".into()).await.unwrap();
        assert_eq!(
            etat_rx.borrow_and_update().status.as_deref(),
            Some("VEILLE"),
            "set_locale doit republier aussitot le nouveau mot de veille, sans attendre un nouveau cycle Power"
        );
    }

    #[tokio::test]
    async fn le_statut_de_lancienne_source_ne_survit_pas_a_un_changement_de_source() {
        // Régression I2 (revue de branche) : `source_status` n'était effacé
        // qu'à la trame suivante de la nouvelle Source. Un "cd" sans disque
        // déclare "pas de disque" ; l'utilisateur bascule sur "radio" qui n'a
        // aucune présélection configurée (une trame transitoire ne touche pas
        // au statut mémorisé) : sans ce correctif, l'écran continuait
        // d'afficher "pas de disque" sous la source "radio".
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update = update_nu();
        update.status = Some("pas de disque".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("pas de disque"));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(
            etat_rx.borrow_and_update().status,
            None,
            "le statut de l'ancienne source ne doit pas survivre au changement de source"
        );
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
    async fn wake_noop_ne_declenche_pas_de_retry_cd_reste_silencieux() {
        // Regression (revue finale 2.2) : le cd repond Noop a Wake (pas de lecture
        // au boot/reveil). L'ancienne porte de retry (!stopped) laissait quand meme
        // planifier une relance sur le prochain PlaybackIdle, ce qui faisait
        // demarrer le cd tout seul ~2s apres. Avec expecting_stream, aucun Play
        // n'a ete emis => pas de retry.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: Arc::new(Mutex::new(Vec::new())) }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
    }

    #[tokio::test]
    async fn un_contenu_fini_n_arme_pas_la_relance_un_flux_live_si() {
        // Mesuré au banc mpv 0.37 : en fin de liste de fichiers, mpv passe
        // `idle` exactement comme lors d'une coupure de flux. Tant que le cœur
        // reniflait l'URI (`cdda://`), un chemin de fichier tombait du mauvais
        // côté — relance exponentielle au lieu de l'arrêt propre, et la liste
        // repartait en boucle depuis la première piste.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("/var/lib/ritornello/plugin-files.m3u").finite())
            .await
            .unwrap();
        assert!(!core.expecting_stream, "un contenu fini ne doit pas armer la relance");

        core.apply(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
        assert!(core.expecting_stream, "un flux live doit rester relançable");
    }

    #[tokio::test]
    async fn une_liste_se_charge_par_load_list_puis_se_positionne() {
        // Le défaut que ce test aurait dû attraper, et qu'il attrape désormais.
        //
        // Avec `loadfile`, mpv ne déplie un `.m3u` qu'**après** coup : mesuré
        // sur mpv 0.37, `playlist-count` vaut 1, puis 3 seulement après un
        // `end-file`/`start-file`. Le `playlist-pos` enchaîné arrivait donc hors
        // bornes, la lecture repartait de la première piste, et l'affichage
        // perdait présélection et titre. `loadlist` déplie sur-le-champ — sa
        // réponse porte même `num_entries` — ce qui rend cet enchaînement sûr.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(
            SourceAction::play("/var/lib/ritornello/plugin-files.m3u")
                .playlist()
                .starting_at(4)
                .finite(),
        )
        .await
        .unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec![
                "load_list /var/lib/ritornello/plugin-files.m3u".to_string(),
                "playlist-pos 4".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn la_touche_lecture_relance_quand_rien_ne_joue() {
        // Défaut signalé, et il touchait **toutes** les sources : `stop` vide la
        // liste de mpv, donc « basculer la pause » n'avait plus rien à reprendre
        // et la touche Lecture ne faisait rien du tout. Mesuré sur la radio comme
        // sur les fichiers avant correction.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://fip")).await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        player_calls.lock().unwrap().clear();

        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec!["play http://fip".to_string()],
            "la source active doit etre redemandee, pas une pause dans le vide"
        );
    }

    #[tokio::test]
    async fn la_touche_lecture_bascule_la_pause_quand_ca_joue() {
        // Garde-fou du test précédent : une pause doit rester une pause, et non
        // devenir un rechargement qui repartirait du début de la piste.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://fip")).await.unwrap();
        player_calls.lock().unwrap().clear();

        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["pause".to_string()]);
        // Et une deuxième fois : mettre en pause ne fait pas « cesser de
        // jouer », donc la reprise reste un simple basculement.
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["pause".to_string(), "pause".to_string()]);
    }

    #[tokio::test]
    async fn la_pause_et_la_reprise_se_lisent_dans_letat_publie() {
        // Le champ le plus lu de la commande `status` de MPD : sans lui, aucun
        // client ne peut afficher le bon bouton.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Stopped);
    }

    #[tokio::test]
    async fn un_echec_de_mpv_ne_change_pas_la_croyance_du_coeur_sur_la_pause() {
        // `paused` était basculé **avant** `toggle_pause`, donc un échec de mpv
        // laissait le cœur croire l'inverse de la vérité. Ce n'est pas cosmétique
        // : c'est cette valeur que `PlayerState.playback` publie, et à laquelle le
        // greffon MPD compare ses `pause 0`/`pause 1`. Un cœur qui se croit en
        // pause devant un mpv qui joue répond « paused » à un client dont la
        // musique continue, puis juge le `pause 0` suivant sans effet.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);

        core.player.pause_echoue.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            core.handle_command(Command::PlayPause).await.is_err(),
            "l'echec de mpv doit remonter, il ne doit pas etre avale"
        );
        assert_eq!(
            core.etat_lecteur().playback,
            Playback::Playing,
            "mpv a refuse : le coeur doit continuer de dire ce qui est vrai"
        );

        // Et la reprise du dialogue remet la bascule en marche : le drapeau n'a
        // pas ete abime, il n'a simplement pas bouge.
        core.player.pause_echoue.store(false, std::sync::atomic::Ordering::SeqCst);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
    }

    #[tokio::test]
    async fn une_pause_ne_survit_pas_a_un_nouveau_play() {
        // Le seul effacement de `paused` est celui du `Play` applique : si on
        // l'oubliait, une pause d'hier rendrait une lecture neuve « en pause ».
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture (radio, http://fip)
        core.handle_command(Command::PlayPause).await.unwrap(); // met en pause
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
        // Selectionne une autre preselection radio (`http://inter`) : un nouveau
        // `Play` est applique, et `paused` doit retomber par ce seul chemin.
        core.handle_command(Command::Select(3)).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
    }

    #[tokio::test]
    async fn la_veille_dit_larret_meme_si_la_pause_etait_posee() {
        // Ce test isole seulement l'oubli du drapeau `paused` : `Command::Power`
        // pose `standby = true` et `lecture = false` dans le meme pas, donc il
        // ne peut pas distinguer laquelle des deux conditions fait le travail.
        // Ce qu'il prouve : un `paused` pose plus tot ne doit pas fuiter dans
        // l'etat rapporte pendant la veille.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture
        core.handle_command(Command::PlayPause).await.unwrap(); // met en pause
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Stopped);
    }

    #[tokio::test]
    async fn un_media_reste_charge_par_loadfile() {
        // La distinction est déclarée par la Source, jamais devinée de l'URI :
        // un `.m3u8` est une liste pour un lecteur de fichiers et un flux HLS
        // pour une radio. Renifler l'extension casserait l'un des deux.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://icecast/fip.m3u8")).await.unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec!["play http://icecast/fip.m3u8".to_string()]
        );
    }

    #[tokio::test]
    async fn un_play_sans_index_ne_positionne_rien() {
        // Le chemin de la radio : aucune commande superflue sur la socket mpv.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["play http://icecast/fip.mp3".to_string()]);
    }

    #[tokio::test]
    async fn wake_play_declenche_bien_un_retry_apres_idle() {
        // Contraste avec le test precedent : quand Wake resulte en Play (radio),
        // un flux est bien attendu, donc un PlaybackIdle doit programmer un retry.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(matches!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::RetryIn(_)));
    }

    #[tokio::test]
    async fn la_fin_du_disque_ne_relance_pas_la_lecture_et_previent_la_source() {
        // Régression (revue 2026-07-27) : `Play cdda://` posait
        // `expecting_stream`, donc la fin du disque (mpv idle) déclenchait la
        // machinerie de relance des flux réseau : `Activate` → `Play cdda://`
        // → le disque repartait de la piste 1, indéfiniment.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        // Unique source : SourceCycle re-active « cd », qui répond `Play cdda://`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        // Fin du disque : pas de relance, et la Source est prévenue — elle
        // seule peut recaler sa vue et son identité sur « plus rien ne joue ».
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Stop"));
    }

    /// Source qui n'a jamais rien à jouer : un lecteur cd sans disque.
    struct SourceVide;

    #[async_trait::async_trait]
    impl Source for SourceVide {
        async fn request(&self, _req: SourceReq) -> Result<SourceAction> {
            Ok(SourceAction::Noop)
        }
    }

    #[tokio::test]
    async fn changer_de_source_arrete_la_lecture_meme_si_la_nouvelle_na_rien_a_jouer() {
        // Régression (revue 2026-07-27) : l'action renvoyée par `Deactivate`
        // était ignorée et l'arrêt reposait sur le `Play` de l'`Activate`
        // suivant — que le cd sans disque ne renvoie pas (`Noop`). La radio
        // continuait de jouer sous un affichage qui annonçait « cd », titres
        // ICY compris.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        sources.insert("cd".into(), Arc::new(SourceVide));
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            etat: etat_tx,
        };
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::SourceCycle).await.unwrap();
        // C'est le cœur qui a arrêté mpv, sans dépendre des plugins.
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
        // Et un titre ICY en retard de l'ancien flux n'atteint plus personne :
        // plus aucun flux n'est attendu.
        core.handle_event(Event::IcyTitle("en retard".into())).await;
        assert_eq!(etat_rx.borrow().morceau.title, None);
    }

    /// Source dont l'activation échoue — un plugin bloqué, que le SDK
    /// sanctionne par un timeout.
    struct SourceEnPanne;

    #[async_trait::async_trait]
    impl Source for SourceEnPanne {
        async fn request(&self, req: SourceReq) -> Result<SourceAction> {
            match req {
                SourceReq::Activate => anyhow::bail!("timeout"),
                _ => Ok(SourceAction::Noop),
            }
        }
    }

    #[tokio::test]
    async fn un_echec_dactivation_laisse_la_bascule_coherente() {
        // Régression (revue 2026-07-27) : `persist()` n'était appelé qu'après
        // un `Activate` réussi. Son échec laissait la bascule à moitié faite :
        // « cd » en mémoire et à l'écran, « radio » dans state.json, et
        // l'ancien flux toujours audible.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        sources.insert("cd".into(), Arc::new(SourceEnPanne));
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert!(core.handle_command(Command::SourceCycle).await.is_err());
        // L'état est cohérent : nouvelle source partout, et rien ne joue.
        assert_eq!(core.active_source(), "cd");
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
    }

    #[tokio::test]
    async fn set_locale_persiste_et_notifie_les_sources() {
        let (mut core, _pc, source_calls, _rx, dir) = setup();
        core.set_locale("fr".into()).await.unwrap();
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:SetLocale(\"fr\")"));
        assert!(calls.iter().any(|c| c == "cd:SetLocale(\"fr\")"));
        drop(calls);
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.locale.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn le_volume_absolu_remplace_le_volume_et_le_borne() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SetVolume(40)).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 40);
        core.handle_command(Command::SetVolume(200)).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 100, "borne haute");
        core.handle_command(Command::SetVolume(0)).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 0);
    }

    #[tokio::test]
    async fn le_volume_absolu_ecrit_une_incrustation_comme_le_pas_relatif() {
        // Un volume change depuis le reseau doit s'annoncer a l'ecran comme celui
        // change depuis la telecommande.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SetVolume(40)).await.unwrap();
        assert!(core.etat_lecteur().overlay.is_some());
    }

    #[tokio::test]
    async fn volume_up_affiche_temporairement_le_volume() {
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        etat_rx.borrow_and_update();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let e = etat_rx.borrow_and_update().clone();
        // PersistedState::default().volume == 60, VolumeUp += 5.
        assert_eq!(e.volume, 65);
        match e.overlay {
            Some(Overlay::Volume { level, muted, text, .. }) => {
                assert_eq!(level, 65);
                assert!(!muted);
                assert_eq!(text, "VOLUME 65 %");
            }
            autre => panic!("attendu une incrustation Volume, obtenu {autre:?}"),
        }
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    async fn mute_affiche_loverlay_muet() {
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        etat_rx.borrow_and_update();
        core.handle_command(Command::Mute).await.unwrap();
        match etat_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Volume { muted, text, .. }) => {
                assert!(muted);
                assert_eq!(text, "VOLUME MUTED");
            }
            autre => panic!("attendu une incrustation Volume, obtenu {autre:?}"),
        }
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    async fn une_mise_a_jour_source_pendant_loverlay_ne_le_remplace_pas_et_reapparait_a_expiration() {
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let avec_overlay = etat_rx.borrow_and_update().clone();
        assert!(matches!(avec_overlay.overlay, Some(Overlay::Volume { .. })));

        // La mise a jour source arrive pendant l'overlay : elle est memorisee
        // (le nom de présélection change) mais l'overlay reste affiche.
        let mut update = update_nu();
        update.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update);
        let pendant = etat_rx.borrow().clone();
        assert!(matches!(pendant.overlay, Some(Overlay::Volume { .. })), "l'overlay reste affiche");
        assert_eq!(pendant.preset_name.as_deref(), Some("FIP"), "mais l'etat sous-jacent est deja a jour");

        // A l'expiration, l'overlay disparait et la mise a jour memorisee est visible.
        core.expire_overlay();
        let apres = etat_rx.borrow_and_update().clone();
        assert!(apres.overlay.is_none());
        assert_eq!(apres.preset_name.as_deref(), Some("FIP"));
        assert!(core.overlay_deadline().is_none());
    }

    #[test]
    fn overlay_deadline_est_none_sans_overlay_actif() {
        let (core, _pc, _sc, _rx, _d) = setup();
        assert!(core.overlay_deadline().is_none());
    }

    #[tokio::test]
    async fn une_nouvelle_pression_repousse_lecheance_de_lloverlay() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let d1 = core.overlay_deadline().unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let d2 = core.overlay_deadline().unwrap();
        // Strictement supérieur : `>=` passerait aussi avec une échéance
        // jamais repoussée (`d2 == d1`), soit exactement le défaut que ce
        // test prétend attraper. Deux `Instant::now()` successifs sont
        // toujours distincts sur les horloges monotones visées.
        assert!(d2 > d1);
    }

    #[tokio::test]
    async fn la_mise_en_veille_efface_lincrustation_volume() {
        // Régression (revue 2026-07-27) : l'incrustation garde la priorité
        // dans `etat_lecteur`, donc « VOLUME 65 % » restait affiché jusqu'à 2 s
        // après l'extinction avant que le mot de veille n'apparaisse.
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        assert!(matches!(etat_rx.borrow_and_update().overlay, Some(Overlay::Volume { .. })));
        core.handle_command(Command::Power).await.unwrap();
        let veille = etat_rx.borrow_and_update().clone();
        assert!(veille.overlay.is_none());
        assert_eq!(veille.status.as_deref(), Some("STANDBY"));
        assert!(core.overlay_deadline().is_none());
    }

    #[tokio::test]
    async fn le_tick_ne_s_arme_pas_quand_rien_ne_joue() {
        let (mut core, _, _, _, _dir) = setup();
        assert!(!core.tick_position(), "rien ne joue : rien à rafraîchir");
        // Bascule vers `cd`, contenu fini : mpv a la parole sur sa position,
        // le tick a donc quelque chose à publier.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(core.tick_position(), "contenu fini en cours de lecture : on suit sa position");
        core.handle_command(Command::Stop).await.unwrap();
        assert!(!core.tick_position());
    }

    /// Cas trouvé en relecture : `radio` n'est pas un contenu fini (mpv ne
    /// fournit pas sa position) et aucun plugin `metadata` n'a posé d'ancre —
    /// personne ne suit ce flux, il n'y a rien à publier. Sans ce garde,
    /// l'appareil interrogerait mpv deux fois par seconde indéfiniment pour
    /// une trame que la déduplication absorbe systématiquement.
    #[tokio::test]
    async fn un_flux_sans_ancre_narme_pas_le_tick() {
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(!core.tick_position(), "flux sans ancre : rien a publier");
    }

    #[tokio::test]
    async fn le_tick_ne_s_arme_pas_en_veille() {
        let (mut core, _, _, _, _dir) = setup();
        // Bascule vers `cd`, contenu fini : le tick a une position à publier.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(core.tick_position());
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.tick_position(), "l'appareil dort");
        // Le garde `!standby` est défensif : aucun chemin atteignable ne pose
        // aujourd'hui la veille en laissant `lecture` vrai (`Command::Power`
        // remet les deux). On construit donc l'état à la main, sans quoi ce
        // test passerait à l'identique si le garde disparaissait. `expecting_stream`
        // reste `false` (contenu fini) pour isoler précisément le garde de veille.
        core.lecture = true;
        core.standby = true;
        assert!(!core.tick_position(), "la veille l'emporte, même si la lecture n'a pas été remise à zéro");
    }

    /// L'échéance déjà posée **survit** aux tours de boucle : c'est tout
    /// l'objet du correctif. Une échéance relative recréée à chaque réveil du
    /// `select!` — commande, événement mpv, enrichissement — repartait de zéro,
    /// et le tick n'arrivait jamais sur un appareil actif.
    #[test]
    fn une_echeance_posee_ne_se_deplace_pas_aux_tours_suivants() {
        let t0 = Instant::now();
        let posee = prochaine_echeance(true, None, t0).unwrap();
        assert_eq!(posee, t0 + Duration::from_secs(1));
        // Trois tours de boucle plus tard, sur un appareil très occupé :
        for retard in [10, 200, 900] {
            let plus_tard = t0 + Duration::from_millis(retard);
            assert_eq!(
                prochaine_echeance(true, Some(posee), plus_tard),
                Some(posee),
                "l'échéance a glissé de {retard} ms"
            );
        }
    }

    #[test]
    fn desarme_l_echeance_est_oubliee() {
        let t0 = Instant::now();
        assert_eq!(prochaine_echeance(false, Some(t0), t0), None);
        assert_eq!(prochaine_echeance(false, None, t0), None);
    }

    /// La règle qui protège les messages éphémères : le tick republie l'état
    /// **avec** l'incrustation en cours, intacte, et sans toucher à son
    /// échéance. C'est l'afficheur qui décide de la mettre par-dessus ou à
    /// côté ; le cœur reste seul maître du moment où elle disparaît.
    #[tokio::test]
    async fn un_rafraichissement_de_position_laisse_l_incrustation_intacte() {
        let (mut core, _, _, _, _dir) = setup();
        // Un contenu **fini** : c'est le seul cas où mpv fournit une position,
        // donc le seul où le rafraîchissement a quelque chose à publier.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let echeance_avant = core.overlay_deadline();
        assert!(core.etat_lecteur().overlay.is_some(), "l'incrustation volume est là");
        core.regle_progression(Some(30.0), Some(254.0));
        core.rafraichit_position().await;
        assert!(core.etat_lecteur().overlay.is_some(), "et elle y reste");
        assert_eq!(core.overlay_deadline(), echeance_avant, "son échéance n'a pas bougé");
        assert_eq!(core.etat_lecteur().position_s, Some(30));
    }

    fn enrichissement(identity: serde_json::Value, artist: &str, title: &str) -> Enrichment {
        Enrichment {
            identity,
            artist: Some(artist.into()),
            title: Some(title.into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn la_selection_declaree_est_diffusee_puis_oubliee_quand_rien_ne_joue() {
        // La touche numérotée mise en évidence sur la télécommande de l'IHM désigne
        // **ce qui joue** : elle suit la déclaration de la Source, et
        // disparaît à l'arrêt plutôt que de rester sur la dernière pression.
        // Le nom de présélection suit exactement la même règle : c'est le
        // point du cahier des charges qui compte (le cycle de vie de
        // `preset_name` est celui de `preset`, verrouillé ici).
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        assert_eq!(etat_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(etat_rx.borrow().preset, None);
        assert_eq!(etat_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn changer_de_source_oublie_la_selection_de_lancienne() {
        // La présélection 2 de la radio ne veut rien dire pour le cd : la
        // laisser en évidence après la bascule désignerait une touche au
        // hasard. Même chose pour son nom : "France Inter" affiché après un
        // passage au cd serait un nom de station attribué à un disque.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        assert_eq!(etat_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(etat_rx.borrow().preset, None);
        assert_eq!(etat_rx.borrow().preset_name, None);
    }

    /// Mise à jour ne portant qu'un compte de présélections déclaré par la Source.
    fn update_avec_compte(compte: Option<u8>) -> SourceUpdate {
        SourceUpdate {
            identity: None,
            transient: false,
            preset: None,
            preset_count: compte,
            preset_name: None,
            status: None,
            can_eject: None,
            presets: None,
            cover: None,
        }
    }

    /// Mise à jour ne portant qu'un nom de présélection déclaré par la Source.
    fn update_avec_nom(nom: Option<&str>) -> SourceUpdate {
        SourceUpdate {
            identity: None,
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: nom.map(str::to_string),
            status: None,
            can_eject: None,
            presets: None,
            cover: None,
        }
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

    /// Mise à jour ne portant que la capacité d'éjection déclarée par la Source.
    fn update_avec_ejection(peut: Option<bool>) -> SourceUpdate {
        SourceUpdate {
            identity: None,
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: None,
            can_eject: peut,
            presets: None,
            cover: None,
        }
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
    async fn lidentite_declaree_par_la_source_est_annoncee_aux_plugins() {
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"kind": "stream", "url": "http://ouifm"});
        core.handle_source_update("radio", joue(id.clone()));
        let np = np_rx.borrow().clone();
        assert_eq!(np.source, "radio");
        assert_eq!(np.identity, Some(id));
    }

    #[tokio::test]
    async fn une_identite_dune_source_inactive_est_ignoree() {
        // Le cd peut rapporter l'insertion d'un disque pendant que la radio
        // joue : annoncer cette identité ferait travailler les plugins sur un
        // morceau qui ne sort d'aucun haut-parleur.
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("cd", joue(serde_json::json!({"kind": "disc"})));
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn licy_est_diffuse_a_la_spa() {
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        // `resume` met la radio en lecture : sans quoi le cœur écarte à raison
        // tout titre ICY, rien ne jouant.
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        assert_eq!(etat_rx.borrow().morceau.title, None);

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.morceau.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(etat.morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn un_enrichissement_de_plugin_ecrase_licy() {
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        // Texte de remplissage réellement émis par OUI FM sur son flux principal.
        core.handle_event(Event::IcyTitle("Now Playing info goes here".into())).await;
        // Sans ce contrôle, la suite du test passerait aussi bien si l'ICY
        // n'était jamais entré : ce n'est pas « l'enrichissement gagne » qu'on
        // vérifierait, mais « l'ICY est absent ».
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("Now Playing info goes here"));
        core.handle_enrichment("ouifm", enrichissement(id, "Shaka Ponk", "Wanna Get Free"));
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.morceau.artist.as_deref(), Some("Shaka Ponk"));
        assert_eq!(etat.morceau.title.as_deref(), Some("Wanna Get Free"));
        assert_eq!(etat.morceau.origin.as_deref(), Some("ouifm"));
    }

    #[tokio::test]
    async fn un_enrichissement_perime_ne_touche_pas_laffichage() {
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        etat_rx.borrow_and_update();
        core.handle_enrichment(
            "ouifm",
            enrichissement(serde_json::json!({"url": "un"}), "Ancien", "Morceau"),
        );
        assert!(!etat_rx.has_changed().unwrap(), "la reponse en retard ne doit rien publier");
        assert!(core.etat_lecteur().morceau.est_vide());
    }

    #[tokio::test]
    async fn changer_de_morceau_efface_immediatement_le_precedent() {
        // Le morceau précédent ne doit pas rester à l'écran pendant qu'on
        // attend le suivant : c'est un comportement, pas un détail.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("So What"));

        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        assert!(etat_rx.borrow().morceau.est_vide(), "l'ardoise doit etre nette aussitot");
    }

    #[tokio::test]
    async fn larret_demande_par_la_telecommande_efface_le_titre_de_lafficheur() {
        // Défaut trouvé en revue : `set_identity` ne rafraîchissait pas
        // l'affichage. La SPA se vidait (canal d'état), mais l'afficheur
        // physique gardait le titre du morceau arrêté jusqu'à la prochaine
        // action de l'utilisateur — toute la nuit sur un appareil qu'on arrête
        // le soir. L'ancien test n'assertionnait que le canal `now_playing` :
        // il passait aussi bien contre le code faux.
        let (mut core, np_rx, etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("So What"));

        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None, "les plugins doivent cesser leur travail");
        assert!(etat_rx.borrow().morceau.est_vide(), "le titre ne doit pas rester affiche");
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
    async fn changer_de_source_diffuse_la_nouvelle_source() {
        // Piege : `SourceCycle` appelle `set_identity(None)`, qui sort sans rien
        // publier quand l'identite etait **deja** nulle — cas du cd sans disque.
        // La source active a pourtant change. C'est ce qui justifie de publier a
        // la sortie de la commande plutot que depuis `set_identity`.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        assert_eq!(etat_rx.borrow().source, "");
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(etat_rx.borrow().source, "cd");
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
    async fn une_avance_de_piste_du_lecteur_est_relayee_a_la_source() {
        // mpv rapporte l'avance, le cœur ne peut pas corriger une identité
        // opaque : il la fait corriger par la Source, seule à savoir ce que
        // « piste 2 » veut dire.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_event(Event::TrackChanged(2)).await;
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:PlayerTrack(2)"));
    }

    #[tokio::test]
    async fn une_avance_de_piste_en_veille_nest_pas_relayee() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_event(Event::TrackChanged(2)).await;
        assert!(source_calls.lock().unwrap().is_empty(), "rien ne doit partir en veille");
    }

    #[tokio::test]
    async fn larret_est_notifie_a_la_source_active() {
        // `Command::Stop` est la seule commande qui change l'état de lecture
        // sans consulter la Source : sans cette notification, une Source qui
        // tient un état de lecture propre (le cd) le garderait faux.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Stop"));
    }

    #[tokio::test]
    async fn un_titre_icy_arrivant_en_veille_natteint_pas_letat_publie() {
        // Chemin réel : `Command::Power` attend la réponse de la Source à
        // `Deactivate` (jusqu'à 5 s) pendant que mpv joue encore. Un titre émis
        // dans cet intervalle arrive après que l'état de veille a été publié —
        // et rien ne se produisant plus en veille, il y resterait des semaines.
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.status.as_deref(), Some("STANDBY"));
        assert!(etat.morceau.est_vide(), "aucun titre ne doit se coller sur l'etat de veille");
    }

    #[tokio::test]
    async fn la_veille_bloque_licy_meme_avec_une_identite_vivante() {
        // Deux gardes couvrent ce chemin, et celle-ci n'est pas redondante : la
        // mise en veille efface normalement l'identité, mais `Command::Power`
        // peut rendre la main sur l'erreur de `player.stop()` **avant** de le
        // faire, laissant la veille active avec une identité vivante. L'état est
        // donc posé directement ici pour éprouver la garde de veille seule.
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap(); // pose `expecting_stream` (la radio joue)
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        etat_rx.borrow_and_update();
        // Veille posée directement : c'est l'état atteint quand `Command::Power`
        // rend la main sur l'erreur de `player.stop()`, donc avec une lecture
        // encore attendue. La garde de veille est alors la seule à agir.
        core.standby = true;
        assert!(core.expecting_stream, "sans quoi ce test n'eprouverait pas la garde de veille");

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        assert!(!etat_rx.has_changed().unwrap(), "rien ne doit atteindre l'etat publie en veille");
        assert_eq!(etat_rx.borrow().morceau.title, None);
    }

    #[tokio::test]
    async fn licy_saffiche_meme_si_la_source_ne_declare_aucune_identite() {
        // Régression rencontrée en essai réel : la couche ICY était
        // conditionnée à la déclaration d'identité de la Source, donc muette
        // face à un plugin qui ne la déclare pas — et muette **en silence**,
        // sans une ligne de journal. C'est pourtant la seule couche censée
        // fonctionner sans aucun plugin `metadata`.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        // Aucune identité n'est jamais déclarée : seul le nom de présélection arrive.
        core.handle_source_update("radio", update_avec_nom(Some("FIP")));
        core.handle_event(Event::IcyTitle("Made Up - TAHITI 80".into())).await;
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("Made Up - TAHITI 80"));
        assert_eq!(etat_rx.borrow().morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn un_titre_icy_arrivant_apres_un_arret_est_ignore() {
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::Stop).await.unwrap();
        etat_rx.borrow_and_update();

        core.handle_event(Event::IcyTitle("un titre en retard".into())).await;
        assert!(!etat_rx.has_changed().unwrap(), "rien ne doit etre publie");
        assert_eq!(etat_rx.borrow().morceau.title, None, "la SPA ne doit pas annoncer de morceau");
    }

    #[tokio::test]
    async fn la_mise_en_veille_oublie_lidentite() {
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn la_mise_en_veille_oublie_la_selection_et_son_nom() {
        // Le point du cahier des charges qui compte : `preset_name` vit et
        // meurt avec `preset`, et le seul endroit qui les efface est
        // `set_identity(None)` — que `Command::Power` atteint en entrant en
        // veille, comme `Stop` et `SourceCycle` déjà couverts plus haut.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        assert_eq!(etat_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::Power).await.unwrap(); // entre en veille
        assert_eq!(etat_rx.borrow().preset, None);
        assert_eq!(etat_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn changer_de_source_oublie_lidentite_precedente() {
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::SourceCycle).await.unwrap();
        let np = np_rx.borrow().clone();
        assert_eq!(np.identity, None);
        assert_eq!(np.source, "cd", "l'annonce porte la nouvelle source active");
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

    /// Trame à la forme que `serve_source` produit vraiment : `can_eject`
    /// estampillé, parce que le SDK l'estampille sur **chaque** trame qu'il
    /// écrit (voir la doc de `SourceMessage::can_eject`).
    ///
    /// À préférer à `update_nu()` dans tout test qui prétend décrire une trame
    /// venue d'un vrai greffon : `SourceUpdate::default()` laisse `can_eject` à
    /// `None`, une forme que le SDK ne peut pas émettre, et un test bâti dessus
    /// peut attester un mode de défaillance qui n'existe pas.
    fn trame_du_sdk() -> SourceUpdate {
        SourceUpdate { can_eject: Some(false), ..SourceUpdate::default() }
    }

    #[tokio::test]
    async fn une_pochette_seule_est_retenue_et_nefface_pas_le_statut() {
        // **Le défaut que la fusion du chantier des pochettes a produit : chaque
        // pochette de Source perdue en silence.** Une pochette arrive
        // volontairement seule, en notification spontanée, sans identité ni
        // statut (voir `SourceMessage::cover`) : c'est sa forme normale. Elle
        // prend donc le retour anticipé — et l'application posée par la fusion
        // vivait tout en bas de `handle_source_update`, après ce `return`. Elle
        // n'était jamais atteinte.
        //
        // Ce qui est épinglé ici est donc **l'application sur le chemin du
        // retour anticipé**, et non le fait que `cover` figure dans
        // `porte_un_fait` : ce prédicat est une tautologie, `serve_source`
        // estampillant `can_eject` sur chaque trame (voir le corps de
        // `handle_source_update`). La trame passait déjà le garde avant qu'on y
        // ajoute `cover`.
        //
        // La trame est donc construite par `trame_du_sdk()` et non `update_nu()` :
        // avec `can_eject: None`, elle décrirait une forme que le SDK ne peut pas
        // émettre, et l'assertion sur le statut y attesterait un mode de
        // défaillance qui n'existe pas. Cette assertion reste, en second rang :
        // elle vaudra si l'estampille devient un jour conditionnelle.
        let (mut core, _np_rx, _etat_rx, tmp) = core_de_test();
        let mut permanent = trame_du_sdk();
        permanent.status = Some("EN DIRECT".into());
        core.handle_source_update("radio", permanent);

        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let mut pochette_seule = trame_du_sdk();
        pochette_seule.cover = Some(ritornello_proto::CoverRef::Path {
            path: image.to_string_lossy().into_owned(),
        });
        core.handle_source_update("radio", pochette_seule);

        assert!(
            core.metadonnees.cover_retenue().is_some(),
            "la pochette doit etre retenue : le retour anticipe est le seul chemin \
             par lequel une pochette de Source atteint le coeur"
        );
        assert_eq!(
            core.etat_lecteur().status.as_deref(),
            Some("EN DIRECT"),
            "et le statut memorise doit survivre"
        );
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
    async fn une_source_cablee_a_chaud_entre_dans_le_catalogue() {
        // Un greffon qui a rate le rendez-vous doit apparaitre dans la liste que
        // les clients interrogent, sans redemarrage — donc `add_source` publie.
        let (mut core, _rx, dir) = setup_sans_source();
        let mut cat_rx = core.catalogue_tx.subscribe();
        assert!(core.catalogue().sources.is_empty(), "aucune source au demarrage");
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud("radio".into(), Arc::new(FakeSource { name: "radio", calls }))
            .await
            .unwrap();
        assert!(cat_rx.has_changed().unwrap(), "les afficheurs doivent l'apprendre");
        assert_eq!(noms(&cat_rx.borrow_and_update()), vec!["radio".to_string()]);
        drop(dir);
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
    async fn une_source_cablee_a_chaud_finit_avec_ses_preselections() {
        // Le chemin complet du greffon qui a rate le rendez-vous : il entre dans
        // le catalogue avec une liste vide, puis sa reponse a `ListPresets` — que
        // le cablage a chaud demande desormais, comme le demarrage — la remplit.
        //
        // La source cablee en second n'est **pas** l'active, ce qui est le cas
        // reel (une `radio` tardive pendant que le `cd` joue) : la liste doit donc
        // franchir le garde de source active, et la publication doit remplacer la
        // liste vide au lieu d'etre dedoublonnee.
        let (mut core, _rx, dir) = setup_sans_source();
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud("cd".into(), Arc::new(FakeSource { name: "cd", calls: calls.clone() }))
            .await
            .unwrap();
        core.cable_source_a_chaud("radio".into(), Arc::new(FakeSource { name: "radio", calls }))
            .await
            .unwrap();
        assert_eq!(core.active_source(), "cd", "la premiere cablee reste l'active");
        let mut cat_rx = core.catalogue_tx.subscribe();
        assert_eq!(noms(&cat_rx.borrow()), vec!["cd".to_string(), "radio".into()]);

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(9, "OUI FM")]));
        assert!(cat_rx.has_changed().unwrap(), "les afficheurs doivent l'apprendre");
        let cat = cat_rx.borrow_and_update().clone();
        let radio = cat.sources.iter().find(|s| s.name == "radio").expect("radio est declaree");
        assert_eq!(radio.presets, vec![pres(1, "FIP"), pres(9, "OUI FM")]);
        drop(dir);
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

    #[tokio::test]
    async fn un_enrichissement_pendant_loverlay_ne_le_remplace_pas() {
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_command(Command::VolumeUp).await.unwrap();
        let avec_overlay = etat_rx.borrow_and_update().clone();
        assert!(matches!(avec_overlay.overlay, Some(Overlay::Volume { .. })));

        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        let pendant = etat_rx.borrow().clone();
        assert!(matches!(pendant.overlay, Some(Overlay::Volume { .. })), "l'overlay volume reste affiche");
        assert_eq!(pendant.morceau.title.as_deref(), Some("So What"), "mais le morceau est deja a jour dessous");
        // ... et le titre reste disponible dès l'expiration.
        core.expire_overlay();
        assert_eq!(etat_rx.borrow_and_update().morceau.title.as_deref(), Some("So What"));
    }

    #[tokio::test]
    async fn un_plugin_metadata_declare_mais_muet_neclipse_pas_licy() {
        // Un plugin déclaré qui ne répond jamais (processus mort, socket muette)
        // ne doit pas priver l'appareil de la couche de base : le titre annoncé
        // par le flux doit continuer de s'afficher, attribué à `icy`.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["mort".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.morceau.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(etat.morceau.origin.as_deref(), Some("icy"));
    }

    /// Short timings so pacing tests run in tens of milliseconds. The core does
    /// not validate bounds (that's the HTTP layer's job), so this is legal.
    fn reglages_rapides() -> crate::state::Settings {
        crate::state::Settings {
            volume_repeat_initial_ms: 30,
            volume_repeat_interval_ms: 25,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn volume_maintenu_est_ignore_avant_le_delai_initial() {
        // Échéance **pilotée**, jamais attendue. La version precedente reposait
        // sur le fait que deux lignes consecutives s'executent en moins des
        // 30 ms du delai initial : sous charge -- un `cargo test --workspace`
        // qui compile encore pendant qu'il teste -- l'ordonnancement depassait
        // cette marge et le test tombait, une fois sur quelques dizaines. Ce
        // qu'il verifie ne depend plus de la vitesse de la machine.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 60 -> 65, arme l'echeance
        // Repoussee loin : la repetition n'a aucune raison d'avoir lieu, quelle
        // que soit la lenteur de ce qui precede.
        core.volume_deadline = Some(Instant::now() + Duration::from_secs(60));
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65, "une repetition avant le delai initial ne fait rien");
    }

    #[tokio::test]
    async fn volume_maintenu_repete_apres_le_delai_puis_a_lintervalle() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65

        // Échéance atteinte : la premiere repetition passe. `Instant::now()` est
        // deja dans le passe quand `handle_input` le relit -- le temps ne
        // recule pas, donc ce declenchement est certain.
        let posee = Instant::now();
        core.volume_deadline = Some(posee);
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70, "premiere repetition apres le delai initial");

        // Elle a rearme l'echeance pour l'intervalle suivant. Compare a celle
        // qu'on avait **posee**, et non a `Instant::now()` : la nouvelle vaut
        // « instant de la repetition + intervalle », donc elle est posterieure a
        // `posee` quoi qu'il arrive. La comparer au present reintroduirait la
        // course que ce test existe pour supprimer.
        let rearmee = core.volume_deadline.expect("l'intervalle doit etre rearme");
        assert!(rearmee > posee, "l'echeance n'a pas ete rearmee apres la repetition");

        // Une echeance dans le futur bloque la repetition suivante.
        core.volume_deadline = Some(Instant::now() + Duration::from_secs(60));
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70);

        // Intervalle ecoule : une repetition de plus, et une seule.
        core.volume_deadline = Some(Instant::now());
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 75, "puis une par intervalle");
    }

    #[tokio::test]
    async fn volume_maintenu_sans_pression_initiale_ne_fait_rien() {
        // A held event with no prior press (core restarted mid-hold): no
        // deadline is armed, nothing moves.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_input(InputMessage { cmd: Command::VolumeDown, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 60);
    }

    #[tokio::test]
    async fn held_sur_une_commande_non_volume_est_ignore() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_input(InputMessage { cmd: Command::Next, held: true }).await.unwrap();
        assert!(source_calls.lock().unwrap().is_empty(), "un Next maintenu ne doit pas atteindre la source");
    }

    #[tokio::test]
    async fn volume_maintenu_est_bloque_en_veille() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65, arms the deadline
        core.handle_command(Command::Power).await.unwrap();    // standby
        // Aucun sommeil : la veille court-circuite `handle_input` **avant** de
        // regarder l'echeance, donc attendre qu'elle expire ne prouvait rien.
        // L'echeance est posee au passe pour que le test echoue si ce
        // court-circuit disparaissait.
        core.volume_deadline = Some(Instant::now());
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65);
    }

    #[tokio::test]
    async fn handle_input_non_held_equivaut_a_handle_command() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_input(InputMessage::from(Command::Select(3))).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn plus10_saffiche_et_repousse_son_echeance() {
        // Chaque appui montre le cumul (+10, +20) dans l'incrustation, avec la
        // même échéance que le volume.
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(core.overlay_deadline().is_some());
        match etat_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Tens { offset, text, .. }) => {
                assert_eq!(offset, 10);
                assert_eq!(text, "PRESET +10");
            }
            autre => panic!("attendu une incrustation Tens, obtenu {autre:?}"),
        };
        core.handle_command(Command::Plus10).await.unwrap();
        match etat_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Tens { offset, text, .. }) => {
                assert_eq!(offset, 20);
                assert_eq!(text, "PRESET +20");
            }
            autre => panic!("attendu une incrustation Tens, obtenu {autre:?}"),
        };
    }

    #[tokio::test]
    async fn le_decalage_est_consomme_par_la_touche_chiffre() {
        // +10 puis 4 = présélection 14 ; le décalage ne survit pas à sa
        // consommation.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_avec_compte(Some(23)));
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Select(4)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(14)")));
        core.handle_command(Command::Select(4)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(4)")));
    }

    #[tokio::test]
    async fn la_touche_zero_seule_ne_fait_rien() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Select(0)).await.unwrap();
        assert!(!source_calls.lock().unwrap().iter().any(|c| c.contains("Select(0)")));
    }

    #[tokio::test]
    async fn zero_atteint_les_multiples_de_dix() {
        // 20 stations : +10 +10 puis 0 = 20 — le décalage 20 doit rester permis.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_avec_compte(Some(20)));
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Select(0)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(20)")));
    }

    #[tokio::test]
    async fn plus10_reboucle_apres_la_derniere_dizaine() {
        // 23 stations : décalages utiles 10 et 20 ; le troisième appui revient
        // à zéro et éteint l'incrustation, comme la fenêtre web.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_avec_compte(Some(23)));
        for _ in 0..3 {
            core.handle_command(Command::Plus10).await.unwrap();
        }
        assert!(core.overlay_deadline().is_none());
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn une_autre_commande_abandonne_le_decalage() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn abandonner_le_decalage_efface_aussi_son_incrustation() {
        // `VolumeUp` masque le défaut (il écrit son propre overlay juste
        // après) : `PlayPause` n'écrit aucun overlay, donc rien ne doit
        // effacer le `+NN` à sa place si ce n'est le garde d'abandon
        // lui-même. Sans le correctif, l'incrustation restait à l'écran
        // jusqu'à son échéance alors que le décalage était déjà abandonné.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(core.overlay_deadline().is_some(), "l'incrustation +10 doit être affichée");
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(
            core.overlay_deadline().is_none(),
            "l'incrustation +NN doit disparaître avec le décalage abandonné"
        );
    }

    #[tokio::test]
    async fn lecheance_de_lincrustation_oublie_le_decalage() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        core.expire_overlay();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn sans_compte_connu_le_decalage_sature_sans_reboucler() {
        // Pas de compte déclaré : on ne sait pas où est la fin, donc pas de
        // rebouclage — saturation à 240.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        for _ in 0..30 {
            core.handle_command(Command::Plus10).await.unwrap();
        }
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(243)")));
    }

    #[tokio::test]
    async fn set_settings_persiste() {
        let (mut core, _pc, _sc, _rx, dir) = setup();
        core.set_settings(crate::state::Settings {
            volume_repeat_initial_ms: 800,
            volume_repeat_interval_ms: 250,
            startup_power: StartupPower::Previous,
            ..Default::default()
        });
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.settings.volume_repeat_initial_ms, 800);
        assert_eq!(st.settings.startup_power, StartupPower::Previous);
    }

    #[tokio::test]
    async fn les_touches_de_deplacement_agissent_sur_un_contenu_fini() {
        let (mut core, calls, _, _, _dir) = setup();
        // Contenu fini : bascule de `radio` (source active par défaut) vers `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekBackward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        let journal = calls.lock().unwrap().clone();
        assert!(journal.contains(&"seek_relative 10".to_string()), "{journal:?}");
        assert!(journal.contains(&"seek_relative -10".to_string()), "{journal:?}");
        assert!(journal.contains(&"seek_absolute 198".to_string()), "{journal:?}");
    }

    /// Sur un direct, la touche ne fait rien — comme une touche non liée. Pas
    /// de message, pas de trame : le contenu n'est pas parcourable, et le dire
    /// n'apprendrait rien à qui vient d'appuyer.
    #[tokio::test]
    async fn les_touches_de_deplacement_sont_ignorees_sur_un_flux() {
        let (mut core, calls, _, _, _dir) = setup();
        // Flux : `radio` est déjà la source active, `PlayPause` la fait jouer.
        core.handle_command(Command::PlayPause).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        assert!(
            calls.lock().unwrap().iter().all(|c| !c.starts_with("seek_")),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn le_pas_de_deplacement_suit_le_reglage() {
        let (mut core, calls, _, _, _dir) = setup();
        // `set_settings` existe déjà (elle sert la route `PUT /api/settings`).
        core.set_settings(crate::state::Settings { seek_step_s: 30, ..Default::default() });
        // Contenu fini : bascule de `radio` vers `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"seek_relative 30".to_string()));
    }

    #[tokio::test]
    async fn un_message_ephemere_desarme_un_decalage_en_cours() {
        // Le message éphémère d'une source (« présélection vide ») emprunte
        // le même emplacement d'overlay que le cumul +NN et le lui vole :
        // sans désarmer le décalage ici, l'appui suivant sur un chiffre
        // composerait encore l'ancien décalage alors que l'écran ne montre
        // plus +NN mais le message de la source.
        let (mut core, _pc, source_calls, mut etat_rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(matches!(etat_rx.borrow_and_update().overlay, Some(Overlay::Tens { .. })));

        let mut ephemere = update_nu();
        ephemere.transient = true;
        ephemere.status = Some("empty preset".into());
        core.handle_source_update("radio", ephemere);

        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(
            source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")),
            "sans décalage armé, Select(3) doit demander la présélection 3"
        );
        assert!(
            !source_calls.lock().unwrap().iter().any(|c| c.contains("Select(13)")),
            "le décalage abandonné par le message éphémère ne doit pas être appliqué"
        );
    }

    #[tokio::test]
    async fn overlay_volume_et_decalage_ont_des_echeances_independantes() {
        // Le test qui compte (brief) : avec deux durées différentes,
        // l'incrustation volume suit `overlay_ms` et celle du cumul suit
        // `tens_window_ms`. C'est l'assertion qui échouerait si quelqu'un
        // recouplait les deux durées derrière un seul champ. Échéances
        // comparées à `Instant::now()`, pas de sommeil.
        //
        // Les durées sont **délibérément énormes** au regard de ce que fait le
        // test. Avec `overlay_ms: 1000` et un pivot à 2000 ms, l'assertion
        // exigeait implicitement que `handle_command` rende la main en moins
        // d'une seconde : une hypothèse d'exécution rapide, donc un flake en
        // puissance dès que la machine est chargée par les autres binaires de
        // test. Le pivot à 300 s entre 60 s et 600 s prouve exactement la même
        // propriété, en laissant quatre minutes de marge à une commande qui
        // prend des microsecondes.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(crate::state::Settings {
            overlay_ms: 60_000,
            tens_window_ms: 600_000,
            ..Default::default()
        });

        let avant = Instant::now();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let echeance_volume = core.overlay_deadline().unwrap();
        assert!(
            echeance_volume < avant + Duration::from_millis(300_000),
            "l'incrustation volume doit suivre overlay_ms (60 s), pas tens_window_ms"
        );

        core.handle_command(Command::Plus10).await.unwrap();
        let echeance_decalage = core.overlay_deadline().unwrap();
        assert!(
            echeance_decalage > avant + Duration::from_millis(300_000),
            "l'incrustation du cumul doit suivre tens_window_ms (600 s), pas overlay_ms"
        );
    }

    #[tokio::test]
    async fn demarrage_en_veille_applique_le_volume_sans_reveiller_la_source() {
        let (mut core, player_calls, source_calls, mut etat_rx, _d) = setup();
        core.start_in_standby().await.unwrap();
        // mpv is configured (volume applied) so waking later starts right...
        // (FakePlayer::set_volume records "vol {v}", see FakePlayer above.)
        assert!(player_calls.lock().unwrap().iter().any(|c| c.starts_with("vol ")));
        // ...but the source was NOT woken, and the display shows standby.
        assert!(!source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")), "pas de Wake en veille");
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));
        assert!(core.etat_lecteur().standby);
        // Power then wakes normally.
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.etat_lecteur().standby);
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")));
    }

    /// Les trois valeurs de `startup_power`, sur le seul critere observable :
    /// la source est-elle reveillee ? `Previous` est teste dans ses deux sens,
    /// sinon un `Previous` traite comme `On` passerait la moitie du test.
    #[tokio::test]
    async fn le_demarrage_suit_le_reglage_de_mise_sous_tension() {
        async fn reveille(startup_power: StartupPower, veille_persistee: bool) -> bool {
            let persisted = PersistedState {
                standby: veille_persistee,
                settings: crate::state::Settings { startup_power, ..Default::default() },
                ..Default::default()
            };
            let (mut core, _pc, source_calls, _rx, _d) = setup_persiste(persisted);
            core.demarrage().await.unwrap();
            // Le verrou est relache par cette liaison, pas garde jusqu'a la
            // fin du bloc : sinon `source_calls` est libere avant lui.
            let a_reveille = source_calls.lock().unwrap().iter().any(|c| c.contains("Wake"));
            a_reveille
        }

        assert!(reveille(StartupPower::On, true).await, "« allume » ignore la veille sur disque");
        assert!(!reveille(StartupPower::Standby, false).await, "« veille » ne reveille jamais");
        assert!(reveille(StartupPower::Previous, false).await, "etait allume : on rallume");
        assert!(!reveille(StartupPower::Previous, true).await, "etait en veille : on y reste");
    }

    /// La veille sur disque doit decrire l'appareil, pas une intention : c'est
    /// tout ce que `StartupPower::Previous` a pour se decider au prochain
    /// demarrage. Les deux sens de la bascule et les deux branches du
    /// demarrage l'ecrivent.
    #[tokio::test]
    async fn la_veille_est_persistee_a_chaque_bascule() {
        let (mut core, _pc, _sc, _rx, dir) = setup();
        let sur_disque = || crate::state::load(&dir.path().join("state.json")).standby;

        core.handle_command(Command::Power).await.unwrap(); // veille
        assert!(sur_disque(), "la mise en veille s'ecrit");
        core.handle_command(Command::Power).await.unwrap(); // reveil
        assert!(!sur_disque(), "le reveil aussi");

        // Et un demarrage remet le fichier d'accord avec ce qu'il a fait,
        // dans les deux sens : sans cela, « etat precedent » choisi plus tard
        // ressusciterait une veille que l'appareil a quittee depuis longtemps.
        core.start_in_standby().await.unwrap();
        assert!(sur_disque());
        core.demarrage().await.unwrap(); // reglage par defaut : « allume »
        assert!(!sur_disque());
    }

    #[tokio::test]
    async fn volume_deadline_ne_survit_pas_a_la_veille() {
        // A deadline armed before standby must not let a held key step the
        // volume after waking: it has to re-press first.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65, arms the deadline
        core.handle_command(Command::Power).await.unwrap();    // standby, clears it
        core.handle_command(Command::Power).await.unwrap();    // wake
        // L'absence d'echeance est affirmee directement, au lieu d'etre deduite
        // d'un sommeil de 40 ms : c'est elle qu'on teste, et une assertion sur
        // l'etat ne depend d'aucune horloge.
        assert!(core.volume_deadline.is_none(), "la veille doit avoir efface l'echeance");
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65, "pas de deadline restante : le held ne fait rien");
    }

    #[tokio::test]
    async fn la_position_de_mpv_est_publiee_sur_un_contenu_fini() {
        // La source active de `setup()` est `radio` (`PersistedState::default`) :
        // `SourceCycle` bascule vers `cd`, qui répond `play("cdda://").finite()` —
        // un contenu fini.
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.regle_progression(Some(87.4), Some(254.0));
        core.rafraichit_position().await;
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, Some(87), "tronquée, jamais arrondie au-dessus");
        assert_eq!(etat.morceau.duration_s, Some(254));
        assert!(etat.seekable, "un disque se parcourt");
        // 87.6 et non 87.4 : au-dessus de la demi-seconde, une troncature et un
        // arrondi ne donnent plus le même entier, et le test distingue enfin
        // les deux implémentations.
        core.regle_progression(Some(87.6), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
    }

    /// Sur un flux, `time-pos` compte depuis le début de la connexion et n'a
    /// aucun rapport avec le morceau : il est lu et jeté. Sans cette garde, la
    /// radio afficherait un compteur d'écoute croissant à la place de la
    /// position dans le morceau.
    #[tokio::test]
    async fn la_position_de_mpv_est_ecartee_sur_un_flux() {
        let (mut core, _, _, _, _dir) = setup();
        // La source active est déjà `radio` : `PlayPause` sans rien qui joue
        // lui redemande d'activer, et la factice répond `play("http://fip")`
        // sans `finite`.
        core.handle_command(Command::PlayPause).await.unwrap();
        core.regle_progression(Some(1234.0), Some(0.0));
        core.rafraichit_position().await;
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, None);
        assert!(!etat.seekable, "un direct ne se rembobine pas");
    }

    /// Régression : `rafraichit_position` n'effaçait que `duree_mesuree_s`
    /// dans la branche flux, laissant `position_s` figé sur la dernière
    /// valeur mesurée pour un disque. `lecture` repasse à `true` aussitôt
    /// qu'à `false` lors d'un `SourceCycle` (le cœur réactive la nouvelle
    /// source dans la foulée), donc le garde-fou `!self.lecture` ne se
    /// déclenche jamais entre les deux et la position du disque survivait,
    /// affichée indéfiniment sous le flux qui a pris sa place.
    #[tokio::test]
    async fn une_position_de_disque_ne_survit_pas_au_passage_a_un_flux() {
        let (mut core, _, _, _, _dir) = setup();
        // Fait jouer le cd, mesure une position.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.regle_progression(Some(87.0), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        // Retour vers la radio : un flux, sans rapport avec la position du disque.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, None, "la position du disque ne doit pas survivre au flux");
    }

    #[tokio::test]
    async fn l_arret_oublie_la_position() {
        let (mut core, _, _, _, _dir) = setup();
        // Bascule vers `cd`, contenu fini : voir le test ci-dessus.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.regle_progression(Some(87.0), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        core.handle_command(Command::Stop).await.unwrap();
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, None, "plus rien ne joue, plus rien à situer");
        assert_eq!(etat.morceau.duration_s, None);
        assert!(!etat.seekable);
    }

    /// La durée mesurée par mpv l'emporte sur celle qu'un plugin annonce : le
    /// disque réel prime sur ce qu'une base en ligne en dit.
    #[tokio::test]
    async fn la_duree_de_mpv_l_emporte_sur_celle_d_un_plugin() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["musicbrainz".into()]);
        // Bascule vers `cd`, contenu fini : sans quoi `rafraichit_position`
        // écarterait la mesure de mpv comme s'il s'agissait d'un flux.
        core.handle_command(Command::SourceCycle).await.unwrap();
        let id = serde_json::json!({"disc": "abc", "track": 2});
        core.handle_source_update("cd", joue(id.clone()));
        core.handle_enrichment(
            "musicbrainz",
            Enrichment {
                identity: id,
                title: Some("So What".into()),
                duration_s: Some(999),
                ..Default::default()
            },
        );
        core.regle_progression(Some(10.0), Some(545.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().morceau.duration_s, Some(545));
    }

    /// Entre deux interrogations du direct — plusieurs dizaines de secondes
    /// chez Radio France — c'est le cœur qui fait avancer la barre, depuis
    /// l'ancre posée à la réception.
    #[tokio::test]
    async fn l_ancre_d_un_enrichissement_avance_toute_seule() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Un **flux** : c'est le seul contexte où l'ancre parle (sur un
        // contenu fini, mpv a la parole). `radio` est déjà la source active.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(254),
                position_s: Some(87),
                ..Default::default()
            },
        );
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        core.avance_ancre_pour_test(std::time::Duration::from_secs(3));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(90));
    }

    /// Un morceau qui finit avant que la station ne l'annonce ne doit pas
    /// afficher « 4:31 / 4:14 ».
    #[tokio::test]
    async fn la_position_annoncee_est_plafonnee_par_la_duree() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(100),
                position_s: Some(98),
                ..Default::default()
            },
        );
        core.avance_ancre_pour_test(std::time::Duration::from_secs(30));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(100));
    }

    /// L'ancre du morceau précédent ne doit pas continuer d'avancer sous le
    /// titre du suivant.
    #[tokio::test]
    async fn un_changement_d_identite_efface_l_ancre() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let un = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(un.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment { identity: un, title: Some("A".into()), position_s: Some(50), ..Default::default() },
        );
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(50));
        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        // Avant meme le rafraichissement : la position du morceau precedent
        // ne doit pas survivre sous le titre du suivant (defaut corrige).
        assert_eq!(core.etat_lecteur().position_s, None, "position perimee sous le titre suivant");
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, None);
    }

    /// Régression : un plugin retenu en réserve qui répond (titre corrigé,
    /// pochette trouvée plus tard) ne doit pas réancrer la position sur la
    /// valeur — inchangée — du gagnant, faute de quoi la barre reculerait
    /// brutalement de tout ce qu'elle avait avancé depuis la précédente
    /// annonce du gagnant.
    #[tokio::test]
    async fn un_plugin_en_reserve_ne_fait_pas_reculer_la_position() {
        let (mut core, _np_rx, _etat_rx, _dir) =
            setup_metadonnees(vec!["radiofrance".into(), "ouifm".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id.clone(),
                title: Some("Bikwix".into()),
                position_s: Some(87),
                ..Default::default()
            },
        );
        core.avance_ancre_pour_test(std::time::Duration::from_secs(30));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(117));
        // `ouifm` répond, mais n'est pas le gagnant : rien de neuf sur
        // l'avancement.
        core.handle_enrichment(
            "ouifm",
            Enrichment { identity: id, title: Some("Autre titre".into()), ..Default::default() },
        );
        core.rafraichit_position().await;
        assert_eq!(
            core.etat_lecteur().position_s,
            Some(117),
            "un plugin en reserve ne doit pas faire reculer la position"
        );
    }

    // -- État partiel (`known`) et pochette : tâche 5 -----------------------

    #[tokio::test]
    async fn une_pochette_de_source_mal_formee_ne_touche_pas_a_celle_qui_tient() {
        // `CoverRef::validee` est la règle de forme de `ritornello-proto`, et
        // elle ne s'appliquait qu'à un des deux canaux d'entrée (celui des
        // greffons). Une référence refusée vaut « rien de neuf » — jamais
        // « plus de pochette » : c'est la convention du champ, et effacer sur
        // une trame mal formée retirerait l'image valide déjà déclarée.
        let (mut core, _np_rx, _etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let bonne = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let id = serde_json::json!({"kind": "file", "path": "/a.flac"});

        let mut update = joue(id);
        update.cover = Some(bonne.clone());
        core.handle_source_update("radio", update.clone());
        assert!(core.metadonnees.known().cover);

        // Chemin relatif : refusé par la forme. Rien ne doit bouger.
        update.identity = None;
        update.cover = Some(ritornello_proto::CoverRef::Path { path: "relatif/folder.jpg".into() });
        core.handle_source_update("radio", update.clone());
        assert_eq!(
            core.metadonnees.cover_retenue().map(|(r, _)| r),
            Some(bonne),
            "une reference mal formee ne doit ni s'installer ni effacer celle qui tient"
        );

        // Et une URL en clair vers une IP littérale non plus, l'autre moitié
        // de ce que `validee` refuse.
        update.cover =
            Some(ritornello_proto::CoverRef::Url { url: "http://192.168.1.1/a.jpg".into() });
        core.handle_source_update("radio", update);
        assert_eq!(core.metadonnees.cover_retenue().map(|(_, o)| o), Some("radio".to_string()));
    }

    /// Un contributeur qui vient de se câbler à chaud, ou qui répond
    /// lentement, doit voir ce qui est déjà connu — sinon il ne peut ni
    /// compléter ce qui manque, ni s'abstenir sur ce qui est déjà rempli.
    #[tokio::test]
    async fn le_now_playing_emis_porte_letat_partiel() {
        let (mut core, mut np_rx, _etat_rx, _tmp) = core_de_test();
        core.set_identity(Some(serde_json::json!({"kind": "stream", "url": "u"})));
        // `handle_icy_title` exige un flux effectivement attendu (voir sa
        // garde) : sans cette ligne, le titre serait ignoré en silence et ce
        // test n'éprouverait rien.
        core.expecting_stream = true;
        core.handle_icy_title("OUI FM".into());
        core.publie_etat();
        // Un contributeur doit voir ce qui est deja connu, sinon il ne peut ni
        // completer ni s'abstenir.
        let np = np_rx.borrow_and_update().clone();
        assert_eq!(np.known.title.as_deref(), Some("OUI FM"));
        assert!(!np.known.cover);
    }

    #[tokio::test]
    async fn une_pochette_arrivee_devient_une_url_locale_dans_letat() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        // La recuperation est detachee : on l'attend explicitement dans le test
        // plutot que de dormir, pour ne pas fabriquer un flake.
        let cle = crate::cover::cle(&r);
        let p = crate::cover::recupere(&r).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(etat.morceau.cover_href.as_deref(), Some(&format!("/api/cover/{cle}")[..]));
        assert_eq!(etat.morceau.cover_origin.as_deref(), Some("files"));
    }

    #[tokio::test]
    async fn une_recuperation_echouee_libere_les_contributeurs_du_dessous() {
        // La jonction que la revue a trouvée : `known.cover` était vrai dès
        // qu'une référence était *retenue*, et `cover_retenue` continuait de
        // préférer cette référence après l'échec de sa récupération. Un motif
        // d'URL de station qui a rouillé faisait donc taire `musicbrainz`
        // définitivement — cas que la conception anticipe explicitement.
        let (mut core, mut np_rx, _etat_rx, _tmp) = setup_metadonnees(vec![
            "radiofrance".into(),
            "musicbrainz".into(),
        ]);
        let id = serde_json::json!({"url": "https://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        let morte =
            ritornello_proto::CoverRef::Url { url: "https://api.radiofrance.fr/rouille".into() };
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                cover: Some(morte.clone()),
                ..Default::default()
            },
        );
        assert!(np_rx.borrow_and_update().known.cover, "une reference est tenue, on ne sait pas encore");

        // Ce que la tâche détachée rapporte quand la récupération n'a rien
        // rendu : `succes == false`.
        core.pochette_arrivee(crate::cover::cle(&morte), false).await;
        let np = np_rx.borrow_and_update().clone();
        assert!(!np.known.cover, "une promesse non tenue doit rendre la parole aux autres");
        // Et le texte que ce même greffon fournit n'a pas bougé : c'est bien
        // ce qui permet à `musicbrainz` de chercher sur cet artiste et cet
        // album, comme la documentation le promet.
        assert_eq!(np.known.title.as_deref(), Some("So What"));
        assert_eq!(np.known.artist.as_deref(), Some("Miles Davis"));
    }

    #[tokio::test]
    async fn un_echec_arrive_apres_un_changement_de_morceau_nest_pas_inscrit() {
        // Le registre des échecs vaut pour le morceau où ils ont eu lieu. Un
        // échec en retard, arrivé après le changement d'identité, ne doit donc
        // pas y entrer : il y noircirait une clé jamais essayée pour le
        // morceau courant, et écarterait cette image alors qu'elle pourrait
        // parfaitement répondre.
        let (mut core, _np_rx, _etat_rx, _tmp) = setup_metadonnees(vec!["musicbrainz".into()]);
        let une = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(une.clone()));
        let image = ritornello_proto::CoverRef::Url {
            url: "https://coverartarchive.org/release/x/front-500".into(),
        };
        core.handle_enrichment(
            "musicbrainz",
            Enrichment {
                identity: une,
                title: Some("T".into()),
                cover: Some(image.clone()),
                ..Default::default()
            },
        );

        // Morceau suivant, puis l'échec du précédent qui arrive enfin.
        let deux = serde_json::json!({"url": "deux"});
        core.handle_source_update("radio", joue(deux.clone()));
        core.pochette_arrivee(crate::cover::cle(&image), false).await;

        // Le même greffon propose la même image pour ce morceau-ci : jamais
        // essayée ici, elle doit être retenue.
        core.handle_enrichment(
            "musicbrainz",
            Enrichment { identity: deux, title: Some("T2".into()), cover: Some(image), ..Default::default() },
        );
        assert!(
            core.metadonnees.known().cover,
            "un echec perime ne doit pas condamner la reference du morceau suivant"
        );
    }

    /// Le risque signalé par la revue de la tâche 3 : deux `Arc<CoverCache>`
    /// distincts compileraient et laisseraient passer tous les autres tests
    /// de ce module, mais la pochette que le cœur vient de déposer ne serait
    /// jamais lisible par la vraie route HTTP. Ce test passe donc par
    /// `status::router` et une vraie requête, avec exactement le même `Arc`
    /// que celui exposé par `app_covers()`.
    #[tokio::test]
    async fn la_route_http_sert_ce_que_le_coeur_vient_de_deposer() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let (mut core, _np_rx, _etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let cle = crate::cover::cle(&r);

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        let p = crate::cover::recupere(&r).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        // Le seul champ qui compte pour cette preuve : le reste de l'`AppState`
        // vient du montage de test générique, jamais consulté par cette route.
        let app = crate::status::router(crate::status::AppState {
            covers: core.app_covers().clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get(format!("/api/cover/{cle}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "la route doit lire dans le meme cache que celui rempli par le coeur"
        );
    }

    /// La pochette d'un morceau déjà remplacé ne doit jamais s'installer sur
    /// le suivant : la vérification de péremption se fait à l'arrivée, pas au
    /// lancement — même garde-fou que l'écho d'identité des enrichissements.
    #[tokio::test]
    async fn une_pochette_perimee_ne_s_installe_pas_sur_le_morceau_suivant() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let ancienne = tmp.path().join("ancienne.jpg");
        std::fs::write(&ancienne, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r_ancienne = ritornello_proto::CoverRef::Path { path: ancienne.to_string_lossy().into_owned() };
        let cle_ancienne = crate::cover::cle(&r_ancienne);

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r_ancienne.clone()), "files");

        // Le morceau change avant que la récupération de l'ancienne pochette
        // n'ait eu le temps d'arriver, et le nouveau déclare sa **propre**
        // pochette (une référence différente) : la cible que `cover_retenue`
        // désigne change avec l'identité, sans jamais redevenir `None` — c'est
        // le comparaison de clé de `pochette_arrivee`, pas seulement l'absence
        // de cible, qui doit rejeter la réponse tardive.
        let nouvelle = tmp.path().join("nouvelle.jpg");
        std::fs::write(&nouvelle, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r_nouvelle = ritornello_proto::CoverRef::Path { path: nouvelle.to_string_lossy().into_owned() };
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/b.flac"})));
        core.set_cover_de_source(Some(r_nouvelle), "files");
        etat_rx.borrow_and_update();

        // La réponse tardive de l'ANCIENNE pochette arrive quand même.
        let p = crate::cover::recupere(&r_ancienne).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle_ancienne.clone(), p).await;
        core.pochette_arrivee(cle_ancienne, true).await;

        assert!(
            !etat_rx.has_changed().unwrap_or(false),
            "la pochette perimee ne doit rien publier sur le morceau suivant"
        );
        assert_eq!(
            core.etat_lecteur().morceau.cover_href, None,
            "la pochette du morceau precedent ne doit pas s'installer sur le suivant"
        );
    }

    /// Repro exacte du défaut critique relevé en revue (tâche 5) : le
    /// marqueur en vol doit se libérer même quand l'arrivée ne publie rien
    /// (morceau déjà remplacé), sans quoi revenir plus tard sur le même
    /// dossier — donc la même clé, un `folder.jpg` est partagé par toutes
    /// les pistes d'un album — ne relançait plus jamais rien : `lance_pochette`
    /// voyait la clé perpétuellement « en vol » et abandonnait en silence.
    #[tokio::test]
    async fn le_marqueur_en_vol_se_libere_meme_quand_larrivee_ne_publie_rien() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let cle = crate::cover::cle(&r);

        // 1. Un morceau d'album déclare la pochette K : `lance_pochette` arme
        // le marqueur. La vraie tâche détachée tourne aussi en tâche de fond,
        // mais rien ci-dessous n'attend son issue — comme les autres tests de
        // ce module, celui-ci simule lui-même l'arrivée plutôt que de dormir.
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        assert_eq!(core.pochette_en_vol.as_deref(), Some(cle.as_str()));

        // 2. Le morceau change avant que la réponse n'arrive : plus rien
        // n'est retenu, mais le marqueur, lui, ne bouge pas tout seul — c'est
        // `pochette_arrivee` qui a la charge de le libérer, à l'arrivée.
        core.set_identity(Some(serde_json::json!({"kind": "stream", "url": "u"})));
        assert_eq!(core.pochette_en_vol.as_deref(), Some(cle.as_str()));
        // Le changement d'identité publie déjà de son côté (titre effacé) :
        // on consomme cette trame pour que l'assertion suivante ne juge que
        // ce que `pochette_arrivee` publie, ou non, par elle-même.
        etat_rx.borrow_and_update();

        // 3. La réponse arrive quand même, en succès (les octets sont bien en
        // main, seulement plus rien à montrer avec). Avant le correctif,
        // cette méthode retournait ici sans jamais toucher au marqueur.
        core.pochette_arrivee(cle.clone(), true).await;
        assert_eq!(core.pochette_en_vol, None, "le marqueur doit se liberer meme sans rien publier");
        assert!(
            !etat_rx.has_changed().unwrap_or(false),
            "rien n'est retenu : cette arrivee ne doit rien publier"
        );

        // 4. Le même dossier — donc la même clé — redevient la cible. Sans le
        // correctif, `lance_pochette` restait bloquée à jamais sur cette clé
        // et cet album n'affichait plus jamais de pochette avant redémarrage.
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        assert_eq!(
            core.pochette_en_vol.as_deref(),
            Some(cle.as_str()),
            "une nouvelle recuperation doit pouvoir repartir pour la meme cle"
        );
        let p = crate::cover::recupere(&r).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(
            etat.morceau.cover_href.as_deref(),
            Some(&format!("/api/cover/{cle}")[..]),
            "revenir sur la meme cle doit a nouveau pouvoir publier une pochette"
        );
    }

    /// Une trame de couverture n'est traitée que si elle vient de la Source
    /// **active** — même garde que le reste de la trame (identité, statut,
    /// présélection). Régression relevée en revue : le câblage précédent
    /// appelait `set_cover_de_source` en dehors de `handle_source_update`,
    /// sans repasser par sa garde de tête, si bien qu'une Source inactive
    /// pouvait faire apparaître sa pochette sur le morceau que joue la
    /// Source active.
    #[tokio::test]
    async fn une_pochette_dune_source_inactive_nest_pas_retenue() {
        let (mut core, _np_rx, etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };

        // `cd` n'est pas la source active (`radio` l'est, par défaut).
        core.handle_source_update(
            "cd",
            SourceUpdate {
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: Some(r),
            },
        );
        assert_eq!(core.pochette_en_vol, None, "une source inactive ne doit declencher aucune recuperation");
        assert!(!etat_rx.has_changed().unwrap_or(false));
    }

    // -- Pochette embarquée, lue par le cœur : tâche 6 ----------------------

    /// Fabrique un mp3 réel avec une pochette embarquée, via ffmpeg — même
    /// principe que `player::mpv::tests::mp3_avec_pochette`, dupliqué ici
    /// faute d'un moyen simple de partager un utilitaire de test entre
    /// modules. Rend `None` si ffmpeg est absent : le test se saute plutôt
    /// que d'échouer, ce n'est pas une dépendance du cœur.
    ///
    /// **L'image doit rester différente de celle de `player::mpv::tests`, et ce
    /// n'est pas cosmétique.** Depuis que le fichier temporaire est nommé
    /// d'après le *contenu* de l'image, deux fixtures portant la même image
    /// visent le même chemin dans le `temp_dir()` **partagé** par tous les tests
    /// de ce binaire — qui tournent en parallèle. Les tests d'ici traversent en
    /// plus `CoverCache`, dont l'éviction **supprime** ces fichiers : la
    /// collision s'est manifestée comme un échec intermittent chez le voisin,
    /// qui lisait un fichier effacé ou réécrit sous lui. Les deux fixtures
    /// partageaient `color=c=red:s=16x16`.
    fn mp3_avec_pochette_de_test(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let image = dir.join("cover.jpg");
        let sortie = dir.join("avec_pochette.mp3");
        let ok = std::process::Command::new("ffmpeg")
            // Verte et 32×32 : voir la doc ci-dessus, elle **ne doit pas**
            // coïncider avec celle de `player::mpv::tests`.
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "color=c=green:s=32x32:d=1"])
            .args(["-frames:v", "1"])
            .arg(&image)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            && std::process::Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg("sine=frequency=440:duration=1")
                .arg("-i")
                .arg(&image)
                .args(["-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-c:v", "copy"])
                .args(["-id3v2_version", "3"])
                .args(["-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"])
                .arg(&sortie)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        ok.then_some(sortie)
    }

    /// Le chemin annoncé par mpv (`Event::Path`) arme une extraction
    /// **détachée** : `handle_event` rend la main aussitôt, sans que rien ne
    /// soit encore connu — la suite (`set_cover_tags` → `true`,
    /// `lance_pochette`, `publie_etat`) n'a lieu qu'à l'arrivée du résultat
    /// sur le canal.
    ///
    /// Le vrai canal est vidé ici, plutôt que rejoué à la main comme le fait
    /// `pochette_arrivee` ailleurs dans ce fichier : relire les tags une
    /// seconde fois pour reconstituer le `CoverRef` attendu écrirait en
    /// concurrence avec la tâche détachée sur le **même** fichier temporaire
    /// (défaut trouvé à l'usage, voir `core_de_test_avec_extraction`). La
    /// tâche détachée doit rester l'unique écrivaine.
    #[tokio::test]
    async fn le_chemin_mpv_declenche_lextraction_et_larmement_de_la_recuperation() {
        let (mut core, mut etat_rx, mut extraction_rx, tmp) = core_de_test_avec_extraction();
        let Some(f) = mp3_avec_pochette_de_test(tmp.path()) else {
            eprintln!("ffmpeg absent : test saute");
            return;
        };
        let chemin = f.to_string_lossy().into_owned();

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": chemin})));
        core.lecture = true;
        etat_rx.borrow_and_update();

        assert_eq!(
            core.handle_event(Event::Path(chemin.clone())).await,
            EventOutcome::Nothing,
            "un chemin ne prouve rien de la vivacite du flux"
        );

        // C'est ICI, et seulement ici, que se vérifie que l'extraction est
        // réellement détachée (ruling 1 de la revue de cette tâche) — sur un
        // vrai mp3 à pochette embarquée, pas sur un chemin inexistant qui
        // échouerait de toute façon aussi vite en synchrone qu'en détaché et
        // ne prouverait donc rien. `#[tokio::test]` tourne sur un runtime
        // **mono-fil** (`current_thread`), et le bras `Event::Path` de
        // `handle_event` ne contient aucun `.await` avant de rendre la main :
        // si `handle_path` exécutait encore `pochette_embarquee` en
        // synchrone (régression qui supprimerait le `tokio::spawn` ou l'appel
        // à `Sante::borne`), `known().cover` serait déjà vrai à cet instant
        // précis, dans le même sondage (poll) que l'`.await` ci-dessus — il
        // n'existe aucun univers d'exécution, rapide ou lent, où une
        // extraction synchrone laisserait cette assertion passer. Ne pas
        // affaiblir ni retirer cette ligne sans la remplacer par une preuve
        // équivalente.
        assert!(!core.metadonnees.known().cover, "l'extraction doit etre detachee, jamais synchrone");
        assert!(!etat_rx.has_changed().unwrap_or(false));

        // Attend le vrai résultat sur le vrai canal — pas d'horloge ici,
        // c'est un rendez-vous asynchrone réel sur la tâche que `handle_path`
        // a détachée.
        let (chemin_recu, r) =
            extraction_rx.recv().await.expect("le canal d'extraction doit livrer un resultat");
        assert_eq!(chemin_recu, chemin);
        let r = r.expect("l'extraction a du reussir sur ce fichier de test");
        core.extraction_arrivee(chemin_recu, Some(r.clone())).await;

        assert!(core.metadonnees.known().cover);
        let (retenue, origine) = core.metadonnees.cover_retenue().expect("une pochette doit etre retenue");
        assert_eq!(origine, crate::metadata::ORIGINE_TAGS);
        assert_eq!(retenue, r);
        assert!(etat_rx.has_changed().unwrap(), "set_cover_tags a renvoye vrai : une trame doit sortir");

        // Rejoue la fin de la récupération détachée à la main, comme les
        // autres tests de ce module : la clé armée par `lance_pochette` doit
        // être celle du fichier temporaire écrit par l'extraction.
        let cle = crate::cover::cle(&r);
        assert_eq!(core.pochette_en_vol.as_deref(), Some(cle.as_str()));
        let p = crate::cover::recupere(&r).await.expect("le fichier temporaire doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(etat.morceau.cover_href.as_deref(), Some(&format!("/api/cover/{cle}")[..]));
        assert_eq!(etat.morceau.cover_origin.as_deref(), Some(crate::metadata::ORIGINE_TAGS));
    }

    /// Le cœur complète, il n'écrase pas : une pochette déjà tenue (ici celle
    /// d'une Source, la plus prioritaire) empêche l'extraction, même quand
    /// mpv annonce un fichier qui, lui, porte une pochette embarquée valide.
    #[tokio::test]
    async fn une_pochette_deja_connue_empeche_toute_extraction() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let Some(f) = mp3_avec_pochette_de_test(tmp.path()) else {
            eprintln!("ffmpeg absent : test saute");
            return;
        };
        let folder = tmp.path().join("folder.jpg");
        std::fs::write(&folder, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: folder.to_string_lossy().into_owned() };

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.lecture = true;
        core.set_cover_de_source(Some(r.clone()), "files");
        etat_rx.borrow_and_update();

        core.handle_event(Event::Path(f.to_string_lossy().into_owned())).await;

        assert!(
            !etat_rx.has_changed().unwrap(),
            "aucune extraction tentee, donc aucune trame supplementaire"
        );
        let (retenue, origine) = core.metadonnees.cover_retenue().unwrap();
        assert_eq!(origine, "files", "le folder.jpg de la Source garde la preseance");
        assert_eq!(retenue, r);
    }

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

    /// Pack français livré dans le dépôt (invariant : mêmes clés que l'anglais embarqué).
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/core/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::i18n::EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }
}
