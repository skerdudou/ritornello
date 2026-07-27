use crate::metadata::{self, Metadonnees, PlayerState};
use crate::player::Player;
use crate::state::{self, PersistedState};
use crate::types::Event;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::SourceUpdate;
use ritornello_proto::{Command, Enrichment, IdentityUpdate, NowPlaying, SourceAction, SourceReq, View};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, RwLock};

/// Anglais embarqué du cœur (base toujours présente).
pub const EN: &str = include_str!("locales/en.toml");

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Durée d'affichage de l'overlay volume/muet apres la derniere pression.
const OVERLAY: Duration = Duration::from_secs(2);

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
    /// Vues composées, vers les plugins Display.
    pub view_tx: watch::Sender<View>,
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
    /// État du lecteur, vers la SPA (route `GET /api/player`) : source, volume,
    /// muet, veille, et le morceau quand on le connaît. Canal distinct de
    /// `view_tx` : la SPA reçoit du structuré, les afficheurs reçoivent des
    /// lignes déjà composées, chacun son chemin.
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
    retry_count: u32,
    audio_device: Option<String>,
    view: View,
    /// La Source a déclaré la `line2` de `view` remplaçable par une métadonnée
    /// (voir `metadata::composer`). Mémorisé avec la vue, puisque c'est d'elle
    /// que la déclaration parle.
    view_line2_replaceable: bool,
    /// Overlay temporaire (volume/muet) : vue à afficher + échéance. Tant
    /// qu'il est actif, `push_view` l'affiche à la place de `view`, mais
    /// `view` continue d'être tenue à jour par `handle_source_update` pour
    /// réapparaître dès l'expiration.
    overlay: Option<(View, Instant)>,
    /// Touche 1-9 correspondant à ce qui joue, déclarée par la Source active
    /// (voir `SourceMessage::preset`). Oubliée dès que plus rien ne joue —
    /// c'est `set_identity(None)` qui fait foi, comme pour l'ardoise des
    /// métadonnées.
    preset: Option<u8>,
    state_path: PathBuf,
    view_tx: watch::Sender<View>,
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
}

impl<P: Player> Core<P> {
    pub fn new(player: P, cablage: Cablage) -> Self {
        let Cablage { sources, persisted, state_path, view_tx, catalog, locales_root, metadata } =
            cablage;
        let mut source_order: Vec<String> = sources.keys().cloned().collect();
        source_order.sort();
        let active_source = if sources.contains_key(&persisted.active_source) {
            persisted.active_source.clone()
        } else {
            source_order.first().cloned().unwrap_or_default()
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
            retry_count: 0,
            audio_device: persisted.audio_device.clone(),
            view: View::default(),
            view_line2_replaceable: false,
            overlay: None,
            preset: None,
            state_path,
            view_tx,
            catalog,
            locale: persisted.locale.clone(),
            locales_root,
            theme: persisted.theme.clone(),
            mode: persisted.mode.clone(),
            metadonnees: Metadonnees::new(metadata.plugins),
            now_playing_tx: metadata.now_playing,
            etat_tx: metadata.etat,
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
                        tracing::warn!("SetLocale vers {name}: {e}");
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

    /// Applique ce qu'une Source rapporte : sa vue, et/ou l'identité de ce
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
        // La vue **avant** l'identité : `set_identity` rafraîchit l'affichage,
        // et l'ordre inverse le ferait composer l'ancienne vue avec l'ardoise
        // déjà vidée. Les deux venant de la même trame, aucun instant
        // observable ne les voit se contredire.
        if let Some(view) = update.view {
            if update.transient {
                // Message éphémère (« présélection vide ») : il emprunte
                // l'emplacement et l'échéance de l'incrustation volume/muet,
                // donc `self.view` — la vue permanente — est conservée et
                // reparaît d'elle-même. Sans cela, le message restait à l'écran
                // indéfiniment alors que la lecture continuait sur la station
                // précédente : l'affichage décrivait durablement un état qui
                // n'existait plus.
                self.overlay = Some((view, Instant::now() + OVERLAY));
            } else {
                self.view = view;
                self.view_line2_replaceable = update.line2_replaceable;
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
        // Toujours pousser : `push_view` donne la priorité à l'incrustation si
        // elle est active, et le canal écarte une vue identique — une vue source
        // arrivant pendant une incrustation ne la perturbe donc pas, tout en
        // étant mémorisée pour reparaître.
        self.push_view();
        // La sélection courante fait partie de l'état diffusé : publier ici
        // couvre la trame qui ne change ni identité ni métadonnées (les autres
        // chemins publient déjà, et le canal déduplique).
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
        }
        if !self.metadonnees.set_identity(identity) {
            return;
        }
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
        self.publie_etat();
        // L'ardoise a changé, donc l'affichage doit suivre — comme le font
        // `handle_icy_title` et `handle_enrichment`. Sans ce rafraîchissement,
        // `Command::Stop` laissait le titre du morceau arrêté **figé sur
        // l'afficheur** jusqu'à la prochaine action de l'utilisateur, alors que
        // la SPA, elle, se vidait correctement.
        if self.overlay.is_none() {
            self.push_view();
        }
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
        if self.overlay.is_none() {
            self.push_view();
        }
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
                tracing::debug!("metadonnees affichees: {gagnant} (reponse de {plugin} gardee en reserve)");
            }
            Some(gagnant) => tracing::debug!("metadonnees affichees: {gagnant}"),
            None => {}
        }
        self.publie_etat();
        if self.overlay.is_none() {
            self.push_view();
        }
    }

    /// Diffuse l'état structuré du morceau vers la SPA.
    fn publie_etat(&self) {
        let etat = self.etat_lecteur();
        // Même déduplication que `push_view`, et pour la même raison : cet état
        // est publié généreusement (à la fin de chaque commande, en plus des
        // chemins de métadonnées), et sans cette garde chaque navigateur
        // connecté recevrait une trame SSE identique à la précédente.
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
            morceau: self.metadonnees.etat(),
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

    async fn appliquer_commande(&mut self, cmd: Command) -> Result<()> {
        match cmd {
            Command::Select(n) => {
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
                let v = self.volume as i16 + if cmd == Command::VolumeUp { 5 } else { -5 };
                self.volume = v.clamp(0, 100) as u8;
                self.player.set_volume(self.volume).await?;
                self.persist();
                self.show_overlay().await;
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
                self.show_overlay().await;
            }
            Command::PlayPause => self.player.toggle_pause().await?,
            Command::Stop => {
                self.expecting_stream = false;
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
                    tracing::debug!("notification d'arret a la source: {e}");
                }
            }
            Command::Power => {
                self.standby = !self.standby;
                if self.standby {
                    let _ = self.active().request(SourceReq::Deactivate).await;
                    self.player.stop().await?;
                    self.expecting_stream = false;
                    // Même raison qu'au-dessus : la réponse de la Source à
                    // `Deactivate` est ignorée, et la vue de veille qui suit
                    // passerait outre le garde-fou de `handle_source_update`.
                    self.set_identity(None);
                    // L'incrustation volume/muet ne survit pas à la mise en
                    // veille : elle garde la priorité dans `push_view`, et
                    // « VOLUME 65 % » restait à l'écran jusqu'à 2 s après
                    // l'extinction avant que la vue de veille n'apparaisse.
                    self.overlay = None;
                    self.view = self.standby_view().await;
                    self.push_view();
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
                        tracing::debug!("notification de piste a la source: {e}");
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
                if !self.standby {
                    if let Err(e) = self.active().request(SourceReq::Stop).await {
                        tracing::debug!("notification d'arret a la source: {e}");
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
            .unwrap_or_else(|| panic!("source active inconnue: {}", self.active_source))
    }

    /// Nom de la source actuellement active (pour la page de statut vivante).
    pub fn active_source(&self) -> &str {
        &self.active_source
    }

    async fn apply(&mut self, action: SourceAction) -> Result<()> {
        match action {
            SourceAction::Noop => {}
            SourceAction::Play { uri } => {
                // La machinerie de relance (`expecting_stream` puis
                // `PlaybackIdle` → retry) n'existe que pour les flux réseau :
                // un disque qui se termine est une fin normale, pas une
                // panne. Marquer `cdda://` « flux attendu » faisait
                // redémarrer le disque en boucle : fin du disque → mpv idle
                // → relance ~2 s → `Activate` → `Play cdda://` → piste 1.
                self.expecting_stream = !uri.starts_with("cdda://");
                self.player.play(&uri).await?;
            }
            SourceAction::Stop => {
                self.expecting_stream = false;
                self.player.stop().await?;
            }
            SourceAction::PlayerNext => self.player.next().await?,
            SourceAction::PlayerPrev => self.player.prev().await?,
        }
        Ok(())
    }

    pub async fn set_audio_device(&mut self, device: String) -> Result<()> {
        self.player.set_audio_device(&device).await?;
        self.audio_device = Some(device);
        self.persist();
        Ok(())
    }

    /// Change la langue courante : reconstruit le catalogue partagé du cœur
    /// (lu par la page de statut), persiste l'état, et pousse `SetLocale` à
    /// chaque plugin Source connecté (best-effort).
    ///
    /// Appelée depuis la boucle `select!` de `main` sur réception du canal
    /// `locale_rx`, lui-même alimenté par la route `PUT /api/locale`.
    pub async fn set_locale(&mut self, locale: String) -> Result<()> {
        self.locale = Some(locale.clone());
        *self.catalog.write().await = Catalog::load("core", &locale, &self.locales_root, EN);
        self.persist();
        for name in self.source_order.clone() {
            if let Some(src) = self.sources.get(&name) {
                if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                    tracing::warn!("SetLocale vers {name}: {e}");
                }
            }
        }
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
        };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistance impossible: {e}");
        }
    }

    /// Pousse la vue à afficher aux plugins Display.
    ///
    /// La composition avec les métadonnées se fait **ici**, à l'affichage, et
    /// non dans `self.view` : l'arrivée d'un enrichissement doit pouvoir
    /// rafraîchir la ligne sans qu'on ait à redemander sa vue à la Source, et
    /// réciproquement une nouvelle vue de la Source ne doit pas effacer les
    /// métadonnées déjà connues.
    ///
    /// L'overlay volume/muet, lui, remplace tout : il est éphémère et n'a pas à
    /// porter le titre du morceau.
    fn push_view(&self) {
        let view = match &self.overlay {
            Some((v, _)) => v.clone(),
            None => metadata::composer(
                &self.view,
                &self.metadonnees.etat(),
                self.view_line2_replaceable,
            ),
        };
        // Rien envoyé si rien ne change à l'écran. Plusieurs chemins peuvent
        // désormais rafraîchir l'affichage pour un même événement (une trame de
        // Source porte à la fois une vue et une identité) ; sans cette garde,
        // chaque afficheur recevrait deux fois la même ligne, et le plugin
        // console la réimprimerait.
        self.view_tx.send_if_modified(|courante| {
            if *courante == view {
                false
            } else {
                *courante = view;
                true
            }
        });
    }

    async fn standby_view(&self) -> View {
        let cat = self.catalog.read().await;
        View { line1: cat.get("standby").to_string(), line2: String::new(), line3: String::new() }
    }

    /// Affiche (ou prolonge) l'overlay temporaire volume/muet : ligne 1 le
    /// libellé "volume", ligne 2 le pourcentage courant ou le message
    /// "muet" selon `self.muted`. Chaque appel repousse l'échéance de
    /// `OVERLAY` (une pression de plus garde l'overlay visible).
    async fn show_overlay(&mut self) {
        let line2 = if self.muted {
            let cat = self.catalog.read().await;
            cat.get("muted").to_string()
        } else {
            format!("{} %", self.volume)
        };
        let line1 = self.catalog.read().await.get("volume_label").to_string();
        self.overlay = Some((View { line1, line2, line3: String::new() }, Instant::now() + OVERLAY));
        self.push_view();
    }

    /// Échéance de l'overlay actif, s'il y en a un (à lire dans `main` avant
    /// le `select!`, à l'image de `retry_at`, pour bâtir la temporisation).
    pub fn overlay_deadline(&self) -> Option<Instant> {
        self.overlay.as_ref().map(|(_, deadline)| *deadline)
    }

    /// Efface l'overlay expiré et laisse réapparaître la vue de la source,
    /// mémorisée entre-temps par `handle_source_update`.
    pub fn expire_overlay(&mut self) {
        self.overlay = None;
        self.push_view();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct FakePlayer {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::player::Player for FakePlayer {
        async fn play(&self, uri: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("play {uri}"));
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
                ("radio", SourceReq::Activate) => SourceAction::Play { uri: "http://fip".into() },
                ("radio", SourceReq::Select(3)) => SourceAction::Play { uri: "http://inter".into() },
                ("radio", SourceReq::Select(_)) => SourceAction::Noop,
                ("cd", SourceReq::Activate) => SourceAction::Play { uri: "cdda://".into() },
                (_, SourceReq::Eject) if self.name == "cd" => SourceAction::Stop,
                ("radio", SourceReq::Wake) => SourceAction::Play { uri: "http://fip".into() },
                ("cd", SourceReq::Wake) => SourceAction::Noop,
                _ => SourceAction::Noop,
            })
        }
    }

    /// Alias pour le montage de test (clippy::type_complexity) : cœur factice,
    /// journaux d'appels du lecteur et des sources, récepteur de vue, répertoire temporaire.
    type Montage = (Core<FakePlayer>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>, watch::Receiver<View>, tempfile::TempDir);

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

    /// Mise à jour ne portant qu'une vue, dont la `line2` est la ligne propre de
    /// la Source (non remplaçable) — le cas de la radio.
    fn vue(v: View) -> SourceUpdate {
        SourceUpdate { view: Some(v), identity: None, line2_replaceable: false, transient: false, preset: None }
    }

    /// Mise à jour dont la `line2` est un remplissage remplaçable — le cas du cd.
    fn vue_remplacable(v: View) -> SourceUpdate {
        SourceUpdate { view: Some(v), identity: None, line2_replaceable: true, transient: false, preset: None }
    }

    /// Mise à jour ne portant qu'une identité.
    fn joue(identity: serde_json::Value) -> SourceUpdate {
        SourceUpdate {
            view: None,
            identity: Some(IdentityUpdate::Playing(identity)),
            line2_replaceable: false,
            transient: false,
            preset: None,
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
        let (tx, rx) = watch::channel(View::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let core = Core::new(
            player,
            Cablage {
                sources,
                persisted: PersistedState::default(),
                state_path: dir.path().join("state.json"),
                view_tx: tx,
                catalog,
                locales_root: root,
                metadata: cablage_muet(vec![]),
            },
        );
        (core, player_calls, source_calls, rx, dir)
    }

    /// Montage observant les deux canaux de métadonnées : ce qui descend vers
    /// les plugins, et l'état structuré qui monte vers la SPA.
    ///
    /// `plugins` porte l'ordre de déclaration, donc la priorité d'arbitrage.
    #[allow(clippy::type_complexity)]
    fn setup_metadonnees(
        plugins: Vec<String>,
    ) -> (
        Core<FakePlayer>,
        watch::Receiver<View>,
        watch::Receiver<NowPlaying>,
        watch::Receiver<PlayerState>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls }));
        let (view_tx, view_rx) = watch::channel(View::default());
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
                view_tx,
                catalog,
                locales_root: root,
                metadata: MetadataCablage { plugins, now_playing: np_tx, etat: etat_tx },
            },
        );
        (core, view_rx, np_rx, etat_rx, dir)
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
    async fn veille_affiche_un_message_dedie_et_ignore_les_vues_pendant_ce_temps() {
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(rx.borrow_and_update().line1, "STANDBY");
        core.handle_source_update("radio", vue(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }));
        assert_eq!(rx.borrow().line1, "STANDBY"); // toujours en veille, la vue source est ignoree
        core.handle_command(Command::Power).await.unwrap();
        core.handle_source_update("radio", vue(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }));
        assert_eq!(rx.borrow_and_update().line1, "RADIO  P1"); // le reveil laisse la source reprendre l'affichage
    }

    #[tokio::test]
    async fn resume_applique_la_sortie_audio_persistee() {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (tx, _rx) = watch::channel(View::default());
        let persisted = PersistedState {
            active_source: "radio".into(),
            volume: 60,
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            theme: None,
            mode: None,
        };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), view_tx: tx, catalog, locales_root: root, metadata: cablage_muet(vec![]) });
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device bluealsa:DEV=XX".to_string()));
    }

    #[tokio::test]
    async fn set_audio_device_applique_et_persiste() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device("hw:CARD=Headphones".into()).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device hw:CARD=Headphones".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device.as_deref(), Some("hw:CARD=Headphones"));
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
    async fn vue_dune_source_inactive_est_ignoree() {
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_source_update("cd", vue(View { line1: "CD".into(), line2: "".into(), line3: "".into() }));
        assert!(rx.borrow().line1.is_empty()); // la vue de "cd" (inactive) n'a pas ete appliquee
        core.handle_source_update("radio", vue(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }));
        assert!(rx.borrow_and_update().line1.contains("RADIO"));
    }

    #[tokio::test]
    async fn standby_view_est_traduit_par_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (tx, mut rx) = watch::channel(View::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "fr", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), view_tx: tx, catalog, locales_root: root, metadata: cablage_muet(vec![]) });
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(rx.borrow_and_update().line1, "VEILLE");
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
        let (tx, _rx) = watch::channel(View::default());
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), view_tx: tx, catalog, locales_root: root, metadata: cablage_muet(vec![]) });
        core.resume().await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
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
        let (tx, _rx) = watch::channel(View::default());
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), view_tx: tx, catalog, locales_root: root, metadata: cablage_muet(vec![]) });
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
        let (tx, _rx) = watch::channel(View::default());
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None }).0,
            etat: etat_tx,
        };
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), view_tx: tx, catalog, locales_root: root, metadata });
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
        let (tx, _rx) = watch::channel(View::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), view_tx: tx, catalog, locales_root: root, metadata: cablage_muet(vec![]) });
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
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        rx.borrow_and_update();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let v = rx.borrow_and_update().clone();
        assert_eq!(v.line1, "VOLUME");
        assert_eq!(v.line2, "65 %"); // PersistedState::default().volume == 60, VolumeUp += 5
        assert!(v.line3.is_empty());
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    async fn mute_affiche_loverlay_muet() {
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        rx.borrow_and_update();
        core.handle_command(Command::Mute).await.unwrap();
        let v = rx.borrow_and_update().clone();
        assert_eq!(v.line1, "VOLUME");
        assert_eq!(v.line2, "MUTED");
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    async fn vue_source_pendant_overlay_est_memorisee_puis_affichee_a_expiration() {
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let overlay_view = rx.borrow_and_update().clone();
        assert_eq!(overlay_view.line1, "VOLUME");

        // La vue source arrive pendant l'overlay : elle est memorisee mais pas affichee.
        core.handle_source_update("radio", vue(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }));
        assert_eq!(rx.borrow().clone(), overlay_view); // aucun nouveau push, l'overlay reste affiche

        // A l'expiration, la vue source memorisee reapparait.
        core.expire_overlay();
        assert_eq!(rx.borrow_and_update().line1, "RADIO  P1");
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
        // dans `push_view`, donc « VOLUME 65 % » restait affiché jusqu'à 2 s
        // après l'extinction avant que la vue de veille n'apparaisse.
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        assert_eq!(rx.borrow_and_update().line1, "VOLUME");
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(rx.borrow_and_update().line1, "STANDBY");
        assert!(core.overlay_deadline().is_none());
    }

    /// Vue de base de la radio : ligne 3 libre, c'est là que les métadonnées
    /// viendront se poser.
    fn vue_radio() -> View {
        View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: String::new() }
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
        // La touche 1-9 mise en évidence sur la télécommande de l'IHM désigne
        // **ce qui joue** : elle suit la déclaration de la Source, et
        // disparaît à l'arrêt plutôt que de rester sur la dernière pression.
        let (mut core, _vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(etat_rx.borrow().preset, None);
    }

    #[tokio::test]
    async fn changer_de_source_oublie_la_selection_de_lancienne() {
        // La présélection 2 de la radio ne veut rien dire pour le cd : la
        // laisser en évidence après la bascule désignerait une touche au
        // hasard.
        let (mut core, _vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(etat_rx.borrow().preset, None);
    }

    #[tokio::test]
    async fn lidentite_declaree_par_la_source_est_annoncee_aux_plugins() {
        let (mut core, _vue_rx, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
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
        let (mut core, _vue_rx, np_rx, _etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("cd", joue(serde_json::json!({"kind": "disc"})));
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn licy_se_pose_sur_la_ligne3_et_est_diffuse_a_la_spa() {
        let (mut core, mut vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        // `resume` met la radio en lecture : sans quoi le cœur écarte à raison
        // tout titre ICY, rien ne jouant.
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_source_update("radio", vue(vue_radio()));
        assert_eq!(vue_rx.borrow_and_update().line3, "");

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let v = vue_rx.borrow_and_update().clone();
        assert_eq!(v.line3, "Mandrillus Sphynx - Bikwix");
        assert_eq!(v.line2, "FIP", "le nom de station reste intouche");
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.morceau.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(etat.morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn un_enrichissement_de_plugin_ecrase_licy() {
        let (mut core, mut vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_source_update("radio", vue(vue_radio()));
        // Texte de remplissage réellement émis par OUI FM sur son flux principal.
        core.handle_event(Event::IcyTitle("Now Playing info goes here".into())).await;
        // Sans ce contrôle, la suite du test passerait aussi bien si l'ICY
        // n'était jamais entré : ce n'est pas « l'enrichissement gagne » qu'on
        // vérifierait, mais « l'ICY est absent ».
        assert_eq!(vue_rx.borrow_and_update().line3, "Now Playing info goes here");
        core.handle_enrichment("ouifm", enrichissement(id, "Shaka Ponk", "Wanna Get Free"));
        assert_eq!(vue_rx.borrow_and_update().line3, "Shaka Ponk — Wanna Get Free");
        assert_eq!(core.etat_lecteur().morceau.origin.as_deref(), Some("ouifm"));
    }

    #[tokio::test]
    async fn un_enrichissement_perime_ne_touche_pas_laffichage() {
        let (mut core, mut vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        core.handle_source_update("radio", vue(vue_radio()));
        vue_rx.borrow_and_update();
        core.handle_enrichment(
            "ouifm",
            enrichissement(serde_json::json!({"url": "un"}), "Ancien", "Morceau"),
        );
        assert_eq!(vue_rx.borrow().line3, "", "la reponse en retard ne doit rien afficher");
        assert!(core.etat_lecteur().morceau.est_vide());
    }

    #[tokio::test]
    async fn changer_de_morceau_efface_immediatement_le_precedent() {
        // Le morceau précédent ne doit pas rester à l'écran pendant qu'on
        // attend le suivant : c'est un comportement, pas un détail.
        let (mut core, mut vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(vue_rx.borrow_and_update().line3, "Miles Davis — So What");

        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        assert_eq!(vue_rx.borrow_and_update().line3, "", "l'ardoise doit etre nette aussitot");
    }

    #[tokio::test]
    async fn larret_demande_par_la_telecommande_efface_le_titre_de_lafficheur() {
        // Défaut trouvé en revue : `set_identity` ne rafraîchissait pas
        // l'affichage. La SPA se vidait (canal d'état), mais l'afficheur
        // physique gardait le titre du morceau arrêté jusqu'à la prochaine
        // action de l'utilisateur — toute la nuit sur un appareil qu'on arrête
        // le soir. L'ancien test n'assertionnait que le canal `now_playing` :
        // il passait aussi bien contre le code faux.
        let (mut core, mut vue_rx, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(vue_rx.borrow_and_update().line3, "Miles Davis — So What");

        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None, "les plugins doivent cesser leur travail");
        assert_eq!(vue_rx.borrow_and_update().line3, "", "le titre ne doit pas rester affiche");
    }

    #[tokio::test]
    async fn letat_du_lecteur_diffuse_volume_muet_veille_et_source() {
        // Le volume n'est expose par aucune route : sa place est ce canal
        // pousse, avec le reste de ce qui est volatil. Une branche de
        // `handle_command` qui oublierait de publier laisserait l'IHM afficher
        // un etat perime sans que rien ne le signale — d'ou la publication a la
        // sortie de **toute** commande, et d'ou ce test qui les parcourt.
        let (mut core, _vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
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
        let (mut core, _vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        assert_eq!(etat_rx.borrow().source, "");
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(etat_rx.borrow().source, "cd");
    }

    #[tokio::test]
    async fn le_morceau_est_aplati_dans_le_json_de_letat() {
        // L'IHM recoit un objet plat : un seul encart, pas deux niveaux a
        // distinguer.
        let (mut core, _vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
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
    async fn un_titre_icy_arrivant_en_veille_natteint_pas_lafficheur() {
        // Chemin réel : `Command::Power` attend la réponse de la Source à
        // `Deactivate` (jusqu'à 5 s) pendant que mpv joue encore. Un titre émis
        // dans cet intervalle arrive après que la vue de veille a été poussée —
        // et rien ne se produisant plus en veille, il y resterait des semaines.
        let (mut core, mut vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(vue_rx.borrow_and_update().line1, "STANDBY");

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let v = vue_rx.borrow().clone();
        assert_eq!(v.line1, "STANDBY");
        assert_eq!(v.line3, "", "aucun titre ne doit se coller sur la vue de veille");
    }

    #[tokio::test]
    async fn la_veille_bloque_licy_meme_avec_une_identite_vivante() {
        // Deux gardes couvrent ce chemin, et celle-ci n'est pas redondante : la
        // mise en veille efface normalement l'identité, mais `Command::Power`
        // peut rendre la main sur l'erreur de `player.stop()` **avant** de le
        // faire, laissant la veille active avec une identité vivante. L'état est
        // donc posé directement ici pour éprouver la garde de veille seule.
        let (mut core, mut vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap(); // pose `expecting_stream` (la radio joue)
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_source_update("radio", vue(vue_radio()));
        vue_rx.borrow_and_update();
        // Veille posée directement : c'est l'état atteint quand `Command::Power`
        // rend la main sur l'erreur de `player.stop()`, donc avec une lecture
        // encore attendue. La garde de veille est alors la seule à agir.
        core.standby = true;
        assert!(core.expecting_stream, "sans quoi ce test n'eprouverait pas la garde de veille");

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        assert_eq!(vue_rx.borrow().line3, "", "rien ne doit atteindre l'afficheur en veille");
        assert_eq!(etat_rx.borrow().morceau.title, None);
    }

    #[tokio::test]
    async fn licy_saffiche_meme_si_la_source_ne_declare_aucune_identite() {
        // Régression rencontrée en essai réel : la couche ICY était
        // conditionnée à la déclaration d'identité de la Source, donc muette
        // face à un plugin qui ne la déclare pas — et muette **en silence**,
        // sans une ligne de journal. C'est pourtant la seule couche censée
        // fonctionner sans aucun plugin `metadata`.
        let (mut core, mut vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        // Aucune identité n'est jamais déclarée : seule la vue arrive.
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_event(Event::IcyTitle("Made Up - TAHITI 80".into())).await;
        assert_eq!(vue_rx.borrow_and_update().line3, "Made Up - TAHITI 80");
        assert_eq!(etat_rx.borrow().morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn un_titre_icy_arrivant_apres_un_arret_est_ignore() {
        let (mut core, mut vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_command(Command::Stop).await.unwrap();
        vue_rx.borrow_and_update();

        core.handle_event(Event::IcyTitle("un titre en retard".into())).await;
        assert_eq!(vue_rx.borrow().line3, "");
        assert_eq!(etat_rx.borrow().morceau.title, None, "la SPA ne doit pas annoncer de morceau");
    }

    #[tokio::test]
    async fn la_mise_en_veille_oublie_lidentite() {
        let (mut core, _vue_rx, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn changer_de_source_oublie_lidentite_precedente() {
        let (mut core, _vue_rx, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::SourceCycle).await.unwrap();
        let np = np_rx.borrow().clone();
        assert_eq!(np.identity, None);
        assert_eq!(np.source, "cd", "l'annonce porte la nouvelle source active");
    }

    #[tokio::test]
    async fn un_message_ephemere_seffece_et_rend_la_vue_precedente() {
        // Cas reel : selectionner une preselection vide. Rien n'est lance, la
        // station precedente joue toujours — le message doit donc passer, puis
        // ceder la place, sans que la vue permanente ni les metadonnees bougent.
        let (mut core, mut vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(vue_rx.borrow_and_update().line2, "FIP");

        let message = View { line1: "RADIO  P4".into(), line2: "empty preset".into(), line3: String::new() };
        core.handle_source_update(
            "radio",
            SourceUpdate { view: Some(message), identity: None, line2_replaceable: false, transient: true, preset: None },
        );
        let affiche = vue_rx.borrow_and_update().clone();
        assert_eq!(affiche.line2, "empty preset", "le message doit s'afficher");
        assert!(core.overlay_deadline().is_some(), "et porter une echeance");

        core.expire_overlay();
        let apres = vue_rx.borrow_and_update().clone();
        assert_eq!(apres.line2, "FIP", "la station qui joue doit reparaitre");
        assert_eq!(apres.line3, "Miles Davis — So What", "les metadonnees aussi");
    }

    #[tokio::test]
    async fn un_enrichissement_pendant_loverlay_ne_le_remplace_pas() {
        let (mut core, mut vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_command(Command::VolumeUp).await.unwrap();
        let overlay = vue_rx.borrow_and_update().clone();
        assert_eq!(overlay.line1, "VOLUME");

        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(vue_rx.borrow().clone(), overlay, "l'overlay volume reste seul a l'ecran");
        // ... et le titre réapparaît composé dès l'expiration.
        core.expire_overlay();
        assert_eq!(vue_rx.borrow_and_update().line3, "Miles Davis — So What");
    }

    #[tokio::test]
    async fn un_plugin_metadata_declare_mais_muet_neclipse_pas_licy() {
        // Un plugin déclaré qui ne répond jamais (processus mort, socket muette)
        // ne doit pas priver l'appareil de la couche de base : le titre annoncé
        // par le flux doit continuer de s'afficher, attribué à `icy`.
        let (mut core, mut vue_rx, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["mort".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_source_update("radio", vue(vue_radio()));
        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let v = vue_rx.borrow_and_update().clone();
        assert_eq!(v.line1, "RADIO  P1");
        assert_eq!(v.line2, "FIP");
        assert_eq!(v.line3, "Mandrillus Sphynx - Bikwix");
        assert_eq!(etat_rx.borrow().morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn lalbum_ne_se_pose_que_sur_une_ligne_declaree_remplacable() {
        // Bout en bout : la déclaration du plugin traverse le canal de mise à
        // jour et gouverne la composition.
        let (mut core, mut vue_rx, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["mb".into()]);
        let id = serde_json::json!({"kind": "disc", "track": 0});
        core.handle_source_update("radio", joue(id.clone()));
        let etiquette = View { line1: "CD 1/3".into(), line2: "audio CD".into(), line3: String::new() };
        core.handle_source_update("radio", vue_remplacable(etiquette.clone()));
        assert_eq!(vue_rx.borrow_and_update().line2, "audio CD", "sans album, l'etiquette reste");

        core.handle_enrichment(
            "mb",
            Enrichment {
                identity: id.clone(),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                album: Some("Kind of Blue".into()),
                duration_s: None,
            },
        );
        let v = vue_rx.borrow_and_update().clone();
        assert_eq!(v.line2, "Kind of Blue");
        assert_eq!(v.line3, "Miles Davis — So What");

        // La même vue, non déclarée remplaçable : l'album ne s'y pose plus.
        core.handle_source_update("radio", vue(etiquette));
        assert_eq!(vue_rx.borrow_and_update().line2, "audio CD");
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
