use crate::metadata::{Metadonnees, PlayerState};
use crate::player::Player;
use crate::state::{self, PersistedState};
use crate::types::Event;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::SourceUpdate;
use ritornello_proto::{
    Command, Enrichment, IdentityUpdate, InputMessage, NowPlaying, Overlay, SourceAction,
    SourceReq,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, RwLock};

/// Anglais embarqué du cœur (base toujours présente).
pub const EN: &str = include_str!("locales/en.toml");

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
    pub fn new(player: P, cablage: Cablage) -> Self {
        let Cablage { sources, persisted, state_path, catalog, locales_root, metadata } = cablage;
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
        Self {
            player,
            sources,
            source_order,
            active_source,
            // Reborné à la lecture : `state.json` peut avoir été édité à la
            // main, et un `volume: 255` partirait tel quel à mpv au réveil.
            volume: persisted.volume.min(100),
            muted: false,
            standby: false,
            expecting_stream: false,
            lecture: false,
            retry_count: 0,
            audio_device: persisted.audio_device.clone(),
            overlay: None,
            preset: None,
            preset_name: None,
            source_status: None,
            standby_status,
            preset_count: None,
            can_eject: false,
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
            settings: persisted.settings.clone(),
            volume_deadline: None,
            position_s: None,
            duree_mesuree_s: None,
            ancre_position: None,
        }
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
        let action = self.active().request(SourceReq::Wake).await?;
        self.apply(action).await?;
        // L'IHM doit connaître le volume et la source dès le premier affichage,
        // sans attendre qu'on touche à quelque chose.
        self.publie_etat();
        Ok(())
    }

    /// Rejoue le contenu courant de la source active (`Activate` demande à la
    /// source de redonner l'URI en cours, pas de passer au contenu suivant).
    pub async fn retry_stream(&mut self) -> Result<()> {
        if !self.standby && self.expecting_stream {
            let action = self.active().request(SourceReq::Activate).await?;
            self.apply(action).await?;
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
    pub fn handle_source_update(&mut self, name: &str, update: SourceUpdate) {
        if self.standby || name != self.active_source {
            return;
        }
        // `status` est réaffirmé par chaque trame permanente : absent vaut
        // effacé — convention **inverse** de celle de `preset`, et la seule
        // qui permette d'effacer un statut (« PAS DE DISQUE » doit pouvoir
        // disparaître à l'insertion d'un disque). Une trame éphémère, elle, ne
        // touche pas au statut mémorisé : son mot va dans l'incrustation
        // ci-dessous, pas ici.
        if !update.transient {
            self.source_status = update.status.clone();
        }
        if update.transient {
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
            if let Some(mot) = update.status.clone() {
                let echeance = Instant::now() + Duration::from_millis(self.settings.overlay_ms.into());
                self.overlay = Some((
                    Overlay::Message { text: mot, remaining_ms: self.settings.overlay_ms },
                    echeance,
                ));
            }
        }
        if let Some(identity) = update.identity {
            let valeur = match identity {
                IdentityUpdate::Playing(v) => Some(v),
                IdentityUpdate::Nothing => None,
            };
            self.set_identity(valeur);
        }
        // Après l'identité : `set_identity(None)` efface la sélection, et une
        // trame qui porterait « rien ne joue » **et** une sélection (ça
        // n'arrive pas, mais rien ne l'interdit) doit laisser gagner la
        // déclaration explicite.
        if let Some(p) = update.preset {
            self.preset = Some(p);
        }
        if let Some(n) = update.preset_name {
            self.preset_name = Some(n);
        }
        if let Some(c) = update.preset_count {
            self.preset_count = Some(c);
        }
        if let Some(e) = update.can_eject {
            self.can_eject = e;
        }
        // Toujours publier : la sélection courante fait partie de l'état
        // diffusé, et cet appel couvre la trame qui ne change ni identité ni
        // métadonnées (les autres chemins publient déjà, et le canal
        // déduplique). `etat_lecteur` porte toujours l'incrustation active
        // aux côtés du reste : une trame source arrivant pendant une
        // incrustation met donc à jour source_status/preset/preset_name sans
        // rien changer de ce que l'afficheur montre tant qu'elle dure.
        self.publie_etat();
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
        self.publie_etat();
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

    /// One volume step (±5), applied to mpv, persisted, shown as an overlay.
    /// Shared by fresh presses and held repeats; only the caller decides how
    /// to re-arm `volume_deadline`.
    async fn step_volume(&mut self, up: bool) -> Result<()> {
        let v = self.volume as i16 + if up { 5 } else { -5 };
        self.volume = v.clamp(0, 100) as u8;
        self.player.set_volume(self.volume).await?;
        self.persist();
        self.show_overlay().await;
        Ok(())
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
        self.settings = s;
        self.persist();
    }

    /// Startup in standby (`settings.start_in_standby`): mpv is configured
    /// (volume, audio device) so a later wake starts right, but the active
    /// source is not woken and the display shows the standby status.
    ///
    /// `standby_status` is not resolved here: it already is, since
    /// construction (see its doc) — no catalogue read on this path.
    pub async fn start_in_standby(&mut self) -> Result<()> {
        self.standby = true;
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
                let action = self.active().request(SourceReq::Select(n)).await?;
                self.apply(action).await?;
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
                let action = self.active().request(SourceReq::Next).await?;
                self.apply(action).await?;
            }
            Command::Prev => {
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Prev).await?;
                self.apply(action).await?;
            }
            Command::Eject => {
                let action = self.active().request(SourceReq::Eject).await?;
                self.apply(action).await?;
            }
            Command::VolumeUp | Command::VolumeDown => {
                self.step_volume(cmd == Command::VolumeUp).await?;
                self.volume_deadline = Some(
                    Instant::now() + Duration::from_millis(self.settings.volume_repeat_initial_ms.into()),
                );
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
                self.show_overlay().await;
            }
            Command::PlayPause => {
                if self.lecture {
                    self.player.toggle_pause().await?;
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
                    let action = self.active().request(SourceReq::Activate).await?;
                    self.apply(action).await?;
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
                if let Err(e) = self.active().request(SourceReq::Stop).await {
                    tracing::debug!("stop notification to source: {e}");
                }
            }
            Command::Power => {
                self.standby = !self.standby;
                if self.standby {
                    let _ = self.active().request(SourceReq::Deactivate).await;
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
                // Changer de source, c'est toujours changer de ce qui joue —
                // et c'est le cœur qui arrête, sans dépendre des réponses des
                // plugins. Avant, l'action renvoyée par `Deactivate` (le
                // `Stop` du plugin radio) était ignorée, et l'arrêt reposait
                // sur le `Play` de l'`Activate` suivant — que le cd sans
                // disque ne renvoie pas (`Noop`) : l'ancien flux continuait
                // de jouer sous un affichage qui annonçait la nouvelle
                // source, titres ICY compris.
                self.expecting_stream = false;
                self.lecture = false;
                self.player.stop().await?;
                // L'ancienne source est prévenue en best-effort : son arrêt
                // est déjà fait, elle n'a plus qu'à recaler son propre état.
                if let Err(e) = self.active().request(SourceReq::Deactivate).await {
                    tracing::debug!("deactivate: {e}");
                }
                let idx = self.source_order.iter().position(|n| n == &self.active_source).unwrap_or(0);
                let next_idx = (idx + 1) % self.source_order.len().max(1);
                if let Some(next_name) = self.source_order.get(next_idx).cloned() {
                    self.active_source = next_name;
                }
                // On l'acte ici sans attendre que la nouvelle Source le
                // déclare : sinon une Source qui omettrait de le faire
                // laisserait l'identité de l'autre en place, et les plugins
                // `metadata` continueraient d'enrichir le morceau précédent.
                self.set_identity(None);
                // Le compte de présélections et le statut annoncés par
                // l'ancienne Source ne veulent rien dire pour la nouvelle : les
                // garder afficherait une fenêtre de numéros qui ne correspond à
                // aucune présélection réelle, ou un statut (« PAS DE DISQUE »)
                // sous le nom d'une source qui n'a encore rien dit — tant que
                // la nouvelle Source n'a pas parlé (ce qui peut ne jamais
                // arriver : une présélection vide déclare une trame éphémère,
                // qui ne touche pas au statut mémorisé).
                self.preset_count = None;
                self.source_status = None;
                // Idem pour l'éjection : la capacité décrit la Source qui
                // s'en va. Sans cet effacement, quitter le cd pour la radio
                // laissait la touche Eject active jusqu'à la première trame
                // de la radio — et pour de bon si elle restait muette.
                self.can_eject = false;
                self.retry_count = 0;
                // Persister **avant** `Activate` : si la nouvelle source ne
                // répond pas (timeout de 5 s du SDK), l'état mémoire, l'état
                // sur disque et l'affichage disent déjà tous la même chose —
                // nouvelle source, rien ne joue. Sans cela, l'échec laissait
                // la bascule à moitié faite : « cd » à l'écran, « radio »
                // dans state.json.
                self.persist();
                let action = self.active().request(SourceReq::Activate).await?;
                self.apply(action).await?;
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
                    if let Err(e) = self.active().request(SourceReq::PlayerTrack(n)).await {
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
                    if let Err(e) = self.active().request(SourceReq::Stop).await {
                        tracing::debug!("stop notification to source: {e}");
                    }
                }
            }
        }
        EventOutcome::Nothing
    }

    fn active(&self) -> Arc<dyn Source> {
        self.sources
            .get(&self.active_source)
            .cloned()
            .unwrap_or_else(|| panic!("unknown active source: {}", self.active_source))
    }

    /// Nom de la source actuellement active (pour la page de statut vivante).
    pub fn active_source(&self) -> &str {
        &self.active_source
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
        let nouveau = Catalog::load("core", &locale, &self.locales_root, EN);
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
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None }).0,
            etat: watch::channel(PlayerState::default()).0,
        }
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
        }
    }

    fn setup() -> Montage {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let core = Core::new(
            player,
            Cablage {
                sources,
                persisted: PersistedState::default(),
                state_path: dir.path().join("state.json"),
                catalog,
                locales_root: root,
                metadata: MetadataCablage {
                    plugins: vec![],
                    now_playing: watch::channel(NowPlaying { source: String::new(), identity: None }).0,
                    etat: etat_tx,
                },
            },
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
        let (np_tx, np_rx) = watch::channel(NowPlaying { source: "radio".into(), identity: None });
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let core = Core::new(
            FakePlayer::default(),
            Cablage {
                sources,
                persisted: PersistedState::default(),
                state_path: dir.path().join("state.json"),
                catalog,
                locales_root: root,
                metadata: MetadataCablage { plugins, now_playing: np_tx, etat: etat_tx },
            },
        );
        (core, np_rx, etat_rx, dir)
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

    #[test]
    fn en_embarque_du_coeur_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(super::EN).unwrap().is_empty());
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
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            theme: None,
            mode: None,
            settings: crate::state::Settings::default(),
        };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]) });
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
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "fr", &root, crate::core::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None }).0,
            etat: etat_tx,
        };
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata });
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
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None }).0,
            etat: etat_tx,
        };
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata });
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
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]) });
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
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]) });
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
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None }).0,
            etat: etat_tx,
        };
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata });
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
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]) });
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
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 60 -> 65
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65, "une repetition avant le delai initial ne fait rien");
    }

    #[tokio::test]
    async fn volume_maintenu_repete_apres_le_delai_puis_a_lintervalle() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70, "premiere repetition apres le delai initial");
        // Immediately after: the interval has not elapsed yet.
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
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
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
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
            start_in_standby: true,
            ..Default::default()
        });
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.settings.volume_repeat_initial_ms, 800);
        assert!(st.settings.start_in_standby);
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
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(crate::state::Settings { overlay_ms: 1000, tens_window_ms: 8000, ..Default::default() });

        let avant = Instant::now();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let echeance_volume = core.overlay_deadline().unwrap();
        assert!(
            echeance_volume < avant + Duration::from_millis(2000),
            "l'incrustation volume doit suivre overlay_ms (1000 ms), pas tens_window_ms"
        );

        core.handle_command(Command::Plus10).await.unwrap();
        let echeance_decalage = core.overlay_deadline().unwrap();
        assert!(
            echeance_decalage > avant + Duration::from_millis(2000),
            "l'incrustation du cumul doit suivre tens_window_ms (8000 ms), pas overlay_ms"
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
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
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

    /// Pack français livré dans le dépôt (invariant : mêmes clés que l'anglais embarqué).
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/core/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(super::EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }
}
