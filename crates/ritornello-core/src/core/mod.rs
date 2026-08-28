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

}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::sync::Mutex;

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

    #[test]
    fn en_embarque_du_coeur_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(crate::i18n::EN).unwrap().is_empty());
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
