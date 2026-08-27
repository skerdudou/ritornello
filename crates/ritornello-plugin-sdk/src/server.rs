use anyhow::{Context, Result};
use ritornello_proto::{
    Catalogue, Cover, DisplayFrame, Enrichment, IdentityUpdate, NowPlaying, PlayerState, Preset,
    SourceAction, SourceMessage, SourceReq, SourceRequest,
};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Issue d'une requête adressée à une Source : l'action que le cœur doit
/// appliquer au lecteur, éventuellement une correction de l'identité de ce qui
/// joue.
pub struct SourceOutcome {
    pub action: SourceAction,
    /// Laissé à `None`, l'identité courante du cœur est conservée. Une Source
    /// qui sait ce qu'elle vient de mettre en lecture doit la renseigner :
    /// sans elle, aucun plugin `metadata` n'apprend le changement, et un
    /// enrichissement en vol sur le morceau précédent resterait affiché.
    pub identity: Option<IdentityUpdate>,
    /// Le statut est un message éphémère (voir `SourceMessage::transient`).
    pub transient: bool,
    /// Touche numérotée correspondant à ce qui joue (voir `SourceMessage::preset`).
    pub preset: Option<u8>,
    /// See `SourceMessage::preset_count`.
    pub preset_count: Option<u8>,
    /// See `SourceMessage::preset_name`.
    pub preset_name: Option<String>,
    /// See `SourceMessage::status`.
    pub status: Option<String>,
    /// See `SourceMessage::presets`.
    pub presets: Option<Vec<Preset>>,
}

impl SourceOutcome {
    /// Issue portant seulement une action (ni statut, ni identité).
    pub fn new(action: SourceAction) -> Self {
        Self {
            action,
            identity: None,
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: None,
            presets: None,
        }
    }

    /// Déclare le statut comme un message **éphémère** : le cœur l'affiche
    /// quelques secondes, puis fait reparaître le statut permanent précédent.
    /// À employer pour signaler un incident sans détruire l'affichage de ce
    /// qui joue.
    pub fn transient(mut self) -> Self {
        self.transient = true;
        self
    }

    /// Déclare la touche numérotée de la télécommande à laquelle correspond ce qui
    /// joue : la présélection pour une radio, la piste pour un cd. C'est ce
    /// qui permet à l'IHM de mettre la touche active en évidence. Le cœur
    /// l'oublie de lui-même quand plus rien ne joue.
    pub fn preset(mut self, n: u8) -> Self {
        self.preset = Some(n);
        self
    }

    /// Declare how many numbered presets exist after this frame (stations,
    /// tracks). See `SourceMessage::preset_count` for the exact semantics.
    pub fn preset_count(mut self, n: u8) -> Self {
        self.preset_count = Some(n);
        self
    }

    /// Déclare le nom lisible de la présélection portée par `preset` (voir
    /// `SourceMessage::preset_name`). Le plugin radio s'en sert avec le nom
    /// configuré de la station.
    pub fn preset_name(mut self, nom: impl Into<String>) -> Self {
        self.preset_name = Some(nom.into());
        self
    }

    /// Declares the source's own state word (see `SourceMessage::status`).
    pub fn status(mut self, mot: impl Into<String>) -> Self {
        self.status = Some(mot.into());
        self
    }

    /// Declares the source's named presets (see `SourceMessage::presets`).
    ///
    /// **An empty list normalizes to absence**, and that is deliberate: "this
    /// source has no names" and "this frame says nothing about names" are the
    /// same statement, so only one of the two writings may travel, and it is
    /// absence. A caller cannot get this wrong, which is why nothing here asks
    /// them to check first — the older wording did ask ("call this with a
    /// non-empty list"), `Notification::presets` never did, and a source
    /// following the docs literally would have relayed an empty list from the
    /// spontaneous path. Deriving the property beats documenting it twice.
    pub fn presets(mut self, presets: Vec<Preset>) -> Self {
        self.presets = if presets.is_empty() { None } else { Some(presets) };
        self
    }

    /// Déclare l'identité **opaque** de ce qui joue désormais.
    pub fn plays(mut self, identity: serde_json::Value) -> Self {
        self.identity = Some(IdentityUpdate::Playing(identity));
        self
    }

    /// Déclare que plus rien ne joue.
    pub fn plays_nothing(mut self) -> Self {
        self.identity = Some(IdentityUpdate::Nothing);
        self
    }
}

/// Notification spontanée d'une Source : changement de piste, arrivée différée
/// d'une TOC, insertion d'un disque.
///
/// Volontairement sans action : le cœur décide seul de ce qui se met en
/// lecture. Une Source qui pourrait déclencher un `Play` de sa propre
/// initiative rendrait la lecture imprévisible depuis la télécommande.
#[derive(Default)]
pub struct Notification {
    pub identity: Option<IdentityUpdate>,
    /// Voir `SourceMessage::transient`.
    pub transient: bool,
    /// Voir `SourceOutcome::preset`.
    pub preset: Option<u8>,
    /// See `SourceMessage::preset_count`.
    pub preset_count: Option<u8>,
    /// See `SourceMessage::preset_name`.
    pub preset_name: Option<String>,
    /// See `SourceMessage::status`.
    pub status: Option<String>,
    /// See `SourceMessage::presets`.
    pub presets: Option<Vec<Preset>>,
    /// Voir `SourceMessage::cover`.
    pub cover: Option<ritornello_proto::CoverRef>,
}

impl Notification {
    pub fn new() -> Self {
        Self::default()
    }

    /// Voir `SourceOutcome::preset`.
    pub fn preset(mut self, n: u8) -> Self {
        self.preset = Some(n);
        self
    }

    /// See `SourceMessage::preset_count`.
    pub fn preset_count(mut self, n: u8) -> Self {
        self.preset_count = Some(n);
        self
    }

    /// Voir `SourceOutcome::preset_name`.
    pub fn preset_name(mut self, nom: impl Into<String>) -> Self {
        self.preset_name = Some(nom.into());
        self
    }

    /// Declares the source's own state word (see `SourceMessage::status`).
    pub fn status(mut self, mot: impl Into<String>) -> Self {
        self.status = Some(mot.into());
        self
    }

    /// See `SourceOutcome::presets`. C'est ce qui permet à une Source de
    /// **republier** son catalogue sans qu'on le lui redemande — renommer une
    /// station depuis sa page d'admin se propage ainsi.
    ///
    /// Une liste vide y devient une absence, exactement comme sur
    /// `SourceOutcome` : ce constructeur-ci n'avait ni garde ni mise en garde,
    /// et c'était le trou — une Source suivant la documentation à la lettre
    /// relayait une liste vide par le chemin spontané.
    pub fn presets(mut self, presets: Vec<Preset>) -> Self {
        self.presets = if presets.is_empty() { None } else { Some(presets) };
        self
    }

    pub fn plays(mut self, identity: serde_json::Value) -> Self {
        self.identity = Some(IdentityUpdate::Playing(identity));
        self
    }

    pub fn plays_nothing(mut self) -> Self {
        self.identity = Some(IdentityUpdate::Nothing);
        self
    }

    /// Voir `SourceMessage::cover`.
    pub fn cover(mut self, c: ritornello_proto::CoverRef) -> Self {
        self.cover = Some(c);
        self
    }
}

#[async_trait::async_trait]
pub trait SourcePlugin: Send + 'static {
    async fn activate(&mut self) -> SourceOutcome;
    async fn deactivate(&mut self) -> SourceOutcome;
    async fn select(&mut self, n: u8) -> SourceOutcome;
    async fn next(&mut self) -> SourceOutcome;
    async fn prev(&mut self) -> SourceOutcome;
    async fn eject(&mut self) -> SourceOutcome;

    /// Cette Source a-t-elle quelque chose à éjecter ?
    ///
    /// Une **capacité de la Source**, pas de ce qu'elle a chargé : un tiroir
    /// vide s'ouvre quand même, donc le cd répond vrai sans disque. Le sdk
    /// l'estampille sur chaque trame, le cœur la relaie dans `PlayerState`, et
    /// la télécommande web grise sa touche Eject là où elle ne mène nulle
    /// part — au lieu d'émettre une commande que `eject()` jette en silence.
    ///
    /// Défaut **faux** : ne pas savoir, c'est n'offrir rien. C'est ce qui rend
    /// la capacité juste sans toucher aux plugins qui n'éjectent rien (radio,
    /// fichiers, entrée générique) : ils compilent inchangés et leur touche
    /// devient grise.
    fn can_eject(&self) -> bool {
        false
    }

    /// Réveil (boot / sortie de veille). Par défaut, se comporte comme
    /// `activate()` (jouer) — adapté à la radio et à toute source simple.
    /// Un plugin qui ne doit pas jouer tout seul au réveil (cd) surcharge.
    async fn wake(&mut self) -> SourceOutcome {
        self.activate().await
    }

    /// Le cœur a arrêté la lecture sans consulter la Source (touche Stop).
    ///
    /// Implémentation par défaut : déclarer que plus rien ne joue, ce qui est
    /// vrai pour toute Source. Sans statut, cette trame **efface** le statut
    /// mémorisé côté cœur (une trame permanente sans statut vaut effacement,
    /// voir `SourceMessage::status`) — ce qui est correct ici, une Source sans
    /// statut permanent n'ayant rien à perdre. Une Source qui en déclare un à
    /// chaque trame (le cd) doit surcharger et repasser par sa propre logique
    /// de statut, sous peine de le voir disparaître à l'arrêt ; une Source qui
    /// tient par ailleurs un état de lecture propre (toujours le cd) surcharge
    /// aussi pour le remettre à jour. Les autres compilent inchangées.
    async fn stop(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop).plays_nothing()
    }

    /// Le lecteur est passé de lui-même à la piste d'index `n`.
    ///
    /// Implémentation par défaut : rien — une radio n'a pas de pistes. Une Source
    /// qui suit un index (le cd) surcharge pour se recaler et rendre une identité
    /// (et, via son propre statut, un état) à jour.
    async fn player_track(&mut self, _n: i64) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop)
    }

    /// Change la langue courante du plugin. Implémentation par défaut : no-op —
    /// un plugin sans texte propre (console, mce) n'a rien à faire, et cd/radio
    /// compilent inchangés tant qu'ils n'ont pas surchargé cette méthode.
    async fn set_locale(&mut self, _locale: String) {}

    /// Les présélections nommées, si cette source sait les énumérer. Défaut : la
    /// liste vide, qui veut dire « je n'ai que des numéros ». Le cd est dans ce
    /// cas par nature — une piste n'a pas de nom sans base de données — et les
    /// fichiers y restent pour l'instant : leur liste **est** la file d'attente,
    /// pas un jeu de présélections.
    ///
    /// La liste peut être **creuse** (stations 1, 5, 99) : `Preset::index` est
    /// l'indice que `Select` attend, jamais un rang.
    async fn list_presets(&mut self) -> Vec<Preset> {
        Vec::new()
    }

    /// Notification spontanée (ex. changement de piste, arrivée différée d'une
    /// TOC). Par défaut ne se termine jamais : un plugin sans notification
    /// spontanée (Radio) n'a rien à écrire de plus.
    ///
    /// Deux points de contrat, dictés par le `select!` du harnais :
    ///
    /// - **`None` est terminal** : il signifie « plus jamais de notification »
    ///   (la tâche interne qui les produisait est morte), et le harnais cesse
    ///   d'appeler cette méthode — les requêtes du cœur restent servies. Un
    ///   `None` re-pollé en boucle aurait tourné à 100 % CPU sans autre
    ///   symptôme que la chauffe.
    /// - **Annulable sans perte** : le futur est abandonné dès qu'une requête
    ///   du cœur arrive (même exigence, et même raison, que
    ///   `MetadataPlugin::next_enrichment`). Tout état durable doit vivre dans
    ///   le plugin, pas dans les variables locales du futur — deux `await`
    ///   successifs dont le second serait interrompu perdraient le premier.
    async fn poll_notification(&mut self) -> Option<Notification> {
        std::future::pending().await
    }
}

/// Lie le socket d'une Source, sans servir encore.
///
/// Séparé de `serve_source` pour que le `Runtime` puisse lier **tous** ses
/// sockets avant de s'annoncer : c'est cet ordre qui fait de l'annonce une
/// barrière de disponibilité.
pub fn bind_source(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepte la connexion du cœur, puis traite les requêtes et les
/// notifications spontanées jusqu'à fermeture de la connexion.
pub async fn serve_source(listener: UnixListener, mut plugin: impl SourcePlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    // Vrai tant que `poll_notification` n'a pas rendu `None` — qui est
    // terminal (voir le trait) et désarme le bras correspondant du `select!`.
    let mut notifications_ouvertes = true;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let req: SourceRequest = match serde_json::from_str(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("invalid source line ignored: {e}");
                        continue;
                    }
                };
                let outcome = match req.req {
                    SourceReq::Activate => plugin.activate().await,
                    SourceReq::Wake => plugin.wake().await,
                    SourceReq::Deactivate => plugin.deactivate().await,
                    SourceReq::Select(n) => plugin.select(n).await,
                    SourceReq::Next => plugin.next().await,
                    SourceReq::Prev => plugin.prev().await,
                    SourceReq::Eject => plugin.eject().await,
                    SourceReq::Stop => plugin.stop().await,
                    SourceReq::PlayerTrack(n) => plugin.player_track(n).await,
                    SourceReq::SetLocale(locale) => {
                        plugin.set_locale(locale).await;
                        SourceOutcome::new(SourceAction::Noop)
                    }
                    // Même précédent que `SetLocale` : une méthode qui ne rend
                    // pas de `SourceOutcome`. Le `Noop` n'est pas décoratif —
                    // c'est lui qui dénoue le `oneshot` du `SourceClient`, qui
                    // exige `(Some(id), Some(action))`. Sans action, l'appelant
                    // attendrait les 5 s du délai puis échouerait, alors que la
                    // liste est déjà là, à côté.
                    SourceReq::ListPresets => {
                        // Plus de garde ici : `SourceOutcome::presets` normalise
                        // lui-même une liste vide en absence, pour tous ses
                        // appelants et non seulement pour ce bras (voir sa doc).
                        // Le corps par défaut de `list_presets` rend `Vec::new()`,
                        // donc une source qui n'énumère pas produit bien une trame
                        // inerte — sans que ce chemin ait à y penser.
                        SourceOutcome::new(SourceAction::Noop)
                            .presets(plugin.list_presets().await)
                    }
                };
                let msg = SourceMessage {
                    id: Some(req.id),
                    action: Some(outcome.action),
                    identity: outcome.identity,
                    transient: outcome.transient,
                    preset: outcome.preset,
                    preset_count: outcome.preset_count,
                    preset_name: outcome.preset_name,
                    status: outcome.status,
                    // Estampillé ici, une seule fois, plutôt que par un appel
                    // de constructeur sur chacun des dix chemins de
                    // déclaration d'un plugin : une capacité oubliée sur un
                    // seul chemin donnerait un bouton qui clignote entre
                    // actif et grisé au fil des trames.
                    can_eject: Some(plugin.can_eject()),
                    presets: outcome.presets,
                    // Une réponse à une requête (Activate, Select…) ne porte
                    // jamais de pochette : `SourceOutcome` ne le déclare pas,
                    // seule la notification spontanée le fait (voir plus bas).
                    cover: None,
                };
                write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
            }
            notification = plugin.poll_notification(), if notifications_ouvertes => {
                match notification {
                    Some(n) => {
                        let msg = SourceMessage {
                            id: None,
                            action: None,
                            identity: n.identity,
                            transient: n.transient,
                            preset: n.preset,
                            preset_count: n.preset_count,
                            preset_name: n.preset_name,
                            status: n.status,
                            can_eject: Some(plugin.can_eject()),
                            presets: n.presets,
                            cover: n.cover,
                        };
                        write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
                    }
                    // `None` est terminal (voir le trait) : désarmer le bras,
                    // sans quoi il serait re-pollé immédiatement et la boucle
                    // tournerait à vide — 100 % CPU pendant que les requêtes
                    // continuent d'être servies, la panne la plus discrète qui
                    // soit. Le cas est réel : le plugin cd rend `None` si sa
                    // tâche de veille du lecteur meurt.
                    None => {
                        tracing::warn!("no more spontaneous notifications (internal task ended)");
                        notifications_ouvertes = false;
                    }
                }
            }
        }
    }
}

/// Enveloppe historique : lie puis sert. Conservée pour les appels directs et
/// pour les tests de protocole, qui ne doivent pas bouger.
pub async fn run_source_plugin(plugin: impl SourcePlugin, socket_path: &Path) -> Result<()> {
    serve_source(bind_source(socket_path)?, plugin).await
}

#[async_trait::async_trait]
pub trait DisplayPlugin: Send + 'static {
    async fn show(&mut self, state: PlayerState) -> Result<()>;

    /// Le catalogue des sources et de leurs présélections nommées.
    ///
    /// Défaut : **ignoré** — un afficheur de vingt colonnes n'en a que faire, et
    /// c'est ce corps par défaut qui fait de chaque nouveau genre de trame une
    /// addition non cassante (voir `DisplayFrame`, fait pour grandir).
    async fn catalogue(&mut self, _c: Catalogue) -> Result<()> {
        Ok(())
    }

    /// Cet afficheur veut-il recevoir les octets des pochettes ?
    ///
    /// **Défaut : non.** Une pochette pèse jusqu'à
    /// `ritornello_proto::COVER_MAX_BYTES`, et un afficheur de vingt colonnes
    /// n'en a que faire : le cœur ne doit pas lui pousser des mégaoctets qu'il
    /// jetterait. Un afficheur qui en veut redéfinit cette méthode, et c'est
    /// **cette valeur-là** qui devient le drapeau de l'annonce — voir
    /// `Runtime::display`. L'annonce est dérivée, jamais demandée : elle ne
    /// peut donc pas mentir sur ce que le greffon fera des octets reçus.
    ///
    /// Lue une seule fois, au moment de l'enregistrement : le drapeau part sur
    /// le socket d'enregistrement, et le cœur ne le relit jamais. Un afficheur
    /// dont l'envie changerait en cours de route n'a donc rien à en attendre —
    /// et n'en a pas besoin : `cover` peut simplement ignorer.
    fn wants_covers(&self) -> bool {
        false
    }

    /// Les octets de la pochette de ce qui joue.
    ///
    /// Défaut : **ignoré** — comme `catalogue` ci-dessus, et pour la même
    /// raison. Reçue seulement si `wants_covers` rend vrai ; le corps par
    /// défaut couvre un afficheur qui aurait demandé sans traiter, ce que le
    /// cœur n'a aucun moyen de distinguer.
    async fn cover(&mut self, _c: Cover) -> Result<()> {
        Ok(())
    }
}

/// Lie le socket d'un afficheur, sans servir encore.
///
/// Séparé de `serve_display` pour que le `Runtime` puisse lier **tous** ses
/// sockets avant de s'annoncer : c'est cet ordre qui fait de l'annonce une
/// barrière de disponibilité.
pub fn bind_display(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepte la connexion du cœur, puis affiche chaque état reçu jusqu'à
/// fermeture. Protocole à sens unique : aucune réponse n'est attendue.
///
/// Chaque ligne est une `DisplayFrame` : un `PlayerState` complet — pas une
/// vue déjà composée, la mise en page appartient au plugin (voir
/// `ritornello-plugin-console::display`) —, un catalogue de sources, ou les
/// octets d'une pochette. Cette dernière n'arrive **que** si le plugin a
/// redéfini `wants_covers` : c'est le cœur qui ne l'envoie pas, pas ce SDK qui
/// la filtre — un afficheur de vingt colonnes ne doit pas recevoir des
/// mégaoctets sur son socket pour les jeter à l'arrivée.
///
/// Une trame d'un genre que ce SDK ne connaît pas est traitée comme une ligne
/// illisible : `warn` puis `continue`, la connexion survit. C'est la politique
/// qui rend l'ajout d'un genre de trame non cassant dans les deux sens — et une
/// ligne au-delà de `LIGNE_MAX` est traitée exactement pareil (voir
/// `lit_ligne_bornee`), pour que la politique reste unique.
pub async fn serve_display(listener: UnixListener, mut plugin: impl DisplayPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, _write) = stream.into_split();
    let mut lecteur = BufReader::new(read);
    let mut tampon = Vec::new();
    loop {
        match lit_ligne_bornee(&mut lecteur, &mut tampon, LIGNE_MAX).await? {
            LigneLue::Fin => return Ok(()),
            LigneLue::TropLongue(vus) => {
                tracing::warn!("display frame ignored: line over {LIGNE_MAX} bytes ({vus} seen)");
                continue;
            }
            LigneLue::Ligne => {}
        }
        let frame: DisplayFrame = match serde_json::from_slice(&tampon) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("invalid display frame ignored: {e}");
                continue;
            }
        };
        match frame {
            DisplayFrame::State(state) => plugin.show(state).await?,
            DisplayFrame::Catalogue(c) => plugin.catalogue(c).await?,
            DisplayFrame::Cover(c) => plugin.cover(c).await?,
        }
    }
}

/// Plafond d'une ligne de ce protocole, en octets.
///
/// **Une acceptation rouverte, pas un oubli.** Quand ce transport a été écrit,
/// lire une ligne sans borne avait été accepté sur ce raisonnement : le cœur est
/// le seul écrivain de ce socket, et borner le lecteur changerait la politique de
/// ligne illisible que la conception a figée. Le plafond de pochette était alors
/// de 2 Mio. Il est passé à 20 Mio sans que cette acceptation soit relue, et à
/// cette valeur-là les deux moitiés du raisonnement ne tiennent plus :
///
/// * Le plafond de `COVER_MAX_BYTES` est contrôlé **au décodage**, c'est-à-dire
///   après que la ligne entière est résidente. Sa doc dit que « le producteur ne
///   matérialise jamais au-delà » — vrai côté cœur, faux côté lecteur, qui n'avait
///   aucune borne du tout. Pas 27 Mio : *ce que l'écrivain veut bien envoyer*. Une
///   ligne sans saut de ligne faisait croître le `Vec` jusqu'à l'OOM, sur un
///   appareil à 1 Gio, dans un processus de greffon qui pèse normalement quelques
///   mégaoctets. « Le cœur est le seul écrivain » parle de *confiance* ; ça ne
///   borne pas un `Vec`, et un cœur qui déraille reste un cœur.
/// * La politique de ligne illisible, elle, ne change pas : une ligne trop longue
///   est drainée jusqu'à son saut de ligne puis traitée comme une ligne
///   illisible — `warn`, `continue`, la connexion survit —, exactement comme une
///   trame mal formée ou d'un genre inconnu. C'est ce qui rend le refus sans
///   conséquence : une trame de pochette est **autonome**, en sauter une ne perd
///   qu'une image.
///
/// La valeur est celle de la plus grande ligne **légitime** : les 4/3 de
/// `COVER_MAX_BYTES` en base64, plus une marge d'enveloppe JSON (les clés, le
/// `href`, le type MIME). Le contrôle de `COVER_MAX_BYTES` au décodage reste donc
/// le seul juge des lignes de taille plausible — une image tout juste au-dessus du
/// plafond est refusée par lui, avec son message, comme avant. Cette borne-ci ne
/// voit que la démesure.
const LIGNE_MAX: usize = ritornello_proto::COVER_MAX_BYTES / 3 * 4 + 4 + 4096;

/// Issue d'une lecture de ligne bornée.
enum LigneLue {
    /// Une ligne complète est dans le tampon.
    Ligne,
    /// La ligne dépassait `LIGNE_MAX` : rien n'est dans le tampon, et le reste
    /// de la ligne a été **consommé** jusqu'à son saut de ligne — sans quoi le
    /// tour de boucle suivant relirait son milieu comme si c'était une trame.
    /// Porte le nombre d'octets vus, pour que le journal dise l'ampleur.
    TropLongue(usize),
    /// Fin de flux : le pair a fermé.
    Fin,
}

/// Lit une ligne dans `tampon`, sans jamais y accumuler plus de `plafond`
/// octets.
///
/// Écrite à la main plutôt qu'avec `BufReader::lines()` ou `read_until` : les
/// deux accumulent sans borne. `fill_buf`/`consume` permet de recopier ce qui
/// est utile et de **jeter au fil de l'eau** ce qui dépasse, si bien que le pic
/// résident est `plafond` plus le tampon interne du `BufReader`, quelle que
/// soit la longueur de ce que l'écrivain envoie.
///
/// Le saut de ligne n'est pas recopié, comme `lines()` ne le recopiait pas. Une
/// dernière ligne sans saut de ligne final est rendue quand même (`Ligne`), puis
/// la fermeture est vue au tour suivant : même comportement que `lines()`.
///
/// `plafond` est un **paramètre** et non `LIGNE_MAX` lu directement, pour que les
/// tests éprouvent le drainage et la resynchronisation sur quelques dizaines
/// d'octets. Les fabriquer à la vraie valeur coûterait 28 Mio par test, et le
/// seul effet de cette dépense serait de charger la machine — la logique testée
/// est la même à 16 octets qu'à 28 Mio, et c'est elle qui peut casser, pas la
/// constante.
async fn lit_ligne_bornee<R: tokio::io::AsyncBufRead + Unpin>(
    lecteur: &mut R,
    tampon: &mut Vec<u8>,
    plafond: usize,
) -> std::io::Result<LigneLue> {
    use tokio::io::AsyncBufReadExt as _;
    tampon.clear();
    let mut vus = 0usize;
    let mut trop_longue = false;
    loop {
        // Le contenu disponible est recopié **puis** consommé dans le même tour :
        // l'emprunt sur `lecteur` doit finir avant l'appel à `consume`, d'où le
        // bloc.
        let (fini, consomme) = {
            let dispo = lecteur.fill_buf().await?;
            if dispo.is_empty() {
                // Fin de flux. Une ligne non terminée déjà commencée est rendue,
                // une ligne trop longue reste un refus.
                if trop_longue {
                    return Ok(LigneLue::TropLongue(vus));
                }
                return Ok(if vus == 0 { LigneLue::Fin } else { LigneLue::Ligne });
            }
            match dispo.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if !trop_longue {
                        tampon.extend_from_slice(&dispo[..i]);
                        // Contrôlé sur cette branche aussi. Le tampon interne du
                        // `BufReader` (8 Kio) ne peut pas rendre d'un coup une
                        // ligne de plus de `plafond`, mais faire dépendre la
                        // borne de cette taille-là serait la faire dépendre d'un
                        // détail d'implémentation.
                        if tampon.len() > plafond {
                            trop_longue = true;
                            tampon.clear();
                            tampon.shrink_to_fit();
                        }
                    }
                    vus += i;
                    (true, i + 1)
                }
                None => {
                    vus += dispo.len();
                    if !trop_longue {
                        tampon.extend_from_slice(dispo);
                        if tampon.len() > plafond {
                            // Bascule irréversible pour cette ligne : le tampon
                            // est rendu tout de suite plutôt que gardé jusqu'au
                            // saut de ligne, et la suite est lue pour être jetée.
                            trop_longue = true;
                            tampon.clear();
                            tampon.shrink_to_fit();
                        }
                    }
                    (false, dispo.len())
                }
            }
        };
        lecteur.consume(consomme);
        if fini {
            return Ok(if trop_longue { LigneLue::TropLongue(vus) } else { LigneLue::Ligne });
        }
    }
}

/// Enveloppe historique : lie puis sert. Conservée pour les appels directs et
/// pour les tests de protocole, qui ne doivent pas bouger.
pub async fn run_display_plugin(plugin: impl DisplayPlugin, socket_path: &Path) -> Result<()> {
    serve_display(bind_display(socket_path)?, plugin).await
}

use ritornello_proto::InputMessage;

#[async_trait::async_trait]
pub trait InputPlugin: Send + 'static {
    async fn next_command(&mut self) -> Result<InputMessage>;
}

/// Lie le socket d'une entrée, sans servir encore.
///
/// Séparé de `serve_input` pour que le `Runtime` puisse lier **tous** ses
/// sockets avant de s'annoncer : c'est cet ordre qui fait de l'annonce une
/// barrière de disponibilité.
pub fn bind_input(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepte la connexion du cœur, puis relaie chaque `InputMessage` produit par
/// le plugin. `held: false` n'est pas sérialisé (voir `InputMessage`), donc
/// les octets sur le fil restent inchangés pour les commandes non maintenues
/// — un cœur d'avant Tâche 1 déserialiserait la trame sans rien y voir de
/// nouveau.
pub async fn serve_input(listener: UnixListener, mut plugin: impl InputPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (_read, mut write) = stream.into_split();
    loop {
        let msg = plugin.next_command().await?;
        write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
    }
}

/// Enveloppe historique : lie puis sert. Conservée pour les appels directs et
/// pour les tests de protocole, qui ne doivent pas bouger.
pub async fn run_input_plugin(plugin: impl InputPlugin, socket_path: &Path) -> Result<()> {
    serve_input(bind_input(socket_path)?, plugin).await
}

#[async_trait::async_trait]
pub trait MetadataPlugin: Send + 'static {
    /// Ce qui joue a changé. Le plugin décide seul s'il sait faire quelque
    /// chose de cette identité ; s'il ne la reconnaît pas, il se tait.
    async fn now_playing(&mut self, np: NowPlaying);

    /// Prochain enrichissement disponible. Ne se termine jamais s'il n'y a
    /// rien à dire (même convention que `poll_notification`).
    ///
    /// **Doit être annulable sans perte** : ce futur est abandonné dès qu'un
    /// `NowPlaying` arrive, donc tout état durable (connexion HTTP ouverte,
    /// file d'attente, cache) doit vivre dans le plugin, jamais dans les
    /// variables locales du futur.
    async fn next_enrichment(&mut self) -> Enrichment;
}

/// Lie le socket d'un plugin de métadonnées, sans servir encore.
///
/// Séparé de `serve_metadata` pour que le `Runtime` puisse lier **tous** ses
/// sockets avant de s'annoncer : c'est cet ordre qui fait de l'annonce une
/// barrière de disponibilité.
pub fn bind_metadata(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepte la connexion du cœur, puis relaie dans les deux sens jusqu'à
/// fermeture : chaque ligne reçue est un `NowPlaying`, chaque enrichissement
/// produit part sur le fil. Aucune corrélation par `id` : les deux sens sont
/// indépendants.
pub async fn serve_metadata(listener: UnixListener, mut plugin: impl MetadataPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                match serde_json::from_str::<NowPlaying>(&line) {
                    Ok(np) => plugin.now_playing(np).await,
                    Err(e) => tracing::warn!("invalid metadata line ignored: {e}"),
                }
            }
            enrichment = plugin.next_enrichment() => {
                let ligne = format!("{}\n", serde_json::to_string(&enrichment)?);
                write.write_all(ligne.as_bytes()).await?;
            }
        }
    }
}

/// Enveloppe historique : lie puis sert. Conservée pour les appels directs et
/// pour les tests de protocole, qui ne doivent pas bouger.
pub async fn run_metadata_plugin(plugin: impl MetadataPlugin, socket_path: &Path) -> Result<()> {
    serve_metadata(bind_metadata(socket_path)?, plugin).await
}

use ritornello_proto::{AdminReq, AdminRequest, AdminResponse, AdminResult};
use std::collections::HashMap;

#[async_trait::async_trait]
pub trait AdminPlugin: Send + Sync + 'static {
    /// Actif d'IHM : `(mime, corps)`, ou `None` si le chemin est inconnu.
    /// Typiquement `ui.js` et `ui.css`, embarqués par `include_str!`.
    fn asset(&self, path: &str) -> Option<(String, String)>;
    /// Catalogue i18n du plugin dans la langue courante, à plat.
    fn catalog(&self) -> serde_json::Value;
    async fn get_data(&self) -> serde_json::Value;
    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>;
}

/// Lie le socket d'un plugin admin, sans servir encore.
///
/// Séparé de `serve_admin` pour que le `Runtime` puisse lier **tous** ses
/// sockets avant de s'annoncer : c'est cet ordre qui fait de l'annonce une
/// barrière de disponibilité.
pub fn bind_admin(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepte la connexion du cœur, puis traite les requêtes admin **en
/// parallèle** : une tâche par requête, un seul écrivain sur la socket.
///
/// Historiquement sériel (lire, attendre, écrire, relire), ce qui faisait
/// qu'un `set_data` qui montait un partage réseau endormi retenait `ui.js`,
/// un simple `include_str!`, jusqu'au plafond du cœur — la page d'admin
/// « disparaissait ». Les réponses partent maintenant dans l'ordre où elles
/// aboutissent ; c'est l'`id` qui les corrèle, pas l'ordre.
///
/// Le greffon est derrière un `RwLock` : `asset`, `catalog`, `get_data`
/// lisent en parallèle, `set_data` est exclusif — il l'est légitimement, c'est
/// une écriture. Le budget (`deadline_ms`) couvre l'**attente du verrou** aussi
/// bien que le traitement : un `GetCatalog` coincé derrière un `set_data` de
/// 60 s répond `Expired` à son échéance au lieu de se taire.
///
/// `Ping` ne prend aucun verrou : c'est ce qui permet au cœur de distinguer
/// « occupé » de « mort ». Les **actifs** n'en prennent pas non plus une fois
/// vus : un bundle est immuable pour la durée de vie du processus, donc il est
/// mis en cache ici, et les deux noms conventionnels (`ui.js`, `ui.css`) sont
/// chargés avant la première requête. Sans cela, le `RwLock` étant équitable
/// (FIFO), un `GetAsset` arrivé après un `set_data` en file attendrait derrière
/// lui — exactement l'incident que ce découplage veut clore.
///
/// Ce que le budget **n'absorbe pas** : `tokio::time::timeout` abandonne le
/// futur au prochain point d'`await`, donc un `set_data` interrompu relâche le
/// verrou — mais une IO bloquante dans un `spawn_blocking` court jusqu'au bout.
/// Les greffons qui touchent un chemin réseau gardent donc l'obligation
/// d'exécuter hors fil et sous disjoncteur (voir `plugin-files/src/sante.rs`).
pub async fn serve_admin(listener: UnixListener, plugin: impl AdminPlugin) -> Result<()> {
    // Les actifs conventionnels sont lus **avant** d'accepter : le verrou est
    // forcément libre, et le cœur les demandera dès la première page.
    let actifs: std::sync::Arc<std::sync::Mutex<HashMap<String, (String, String)>>> = Default::default();
    for nom in ["ui.js", "ui.css"] {
        if let Some(a) = plugin.asset(nom) {
            actifs.lock().unwrap().insert(nom.to_string(), a);
        }
    }
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let plugin = std::sync::Arc::new(tokio::sync::RwLock::new(plugin));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AdminResponse>(64);

    // L'unique écrivain : sérialise les trames sortantes sans sérialiser les
    // traitements.
    let ecrivain = tokio::spawn(async move {
        while let Some(resp) = rx.recv().await {
            let ligne = match serde_json::to_string(&resp) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("admin response not serializable: {e}");
                    continue;
                }
            };
            if write.write_all(format!("{ligne}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let req: AdminRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("invalid admin request ignored: {e}");
                continue;
            }
        };
        let plugin = plugin.clone();
        let actifs = actifs.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let id = req.id;
            let budget = req.deadline_ms.map(std::time::Duration::from_millis);
            let travail = traite_admin(plugin, actifs, req.req);
            let result = match budget {
                Some(d) => match tokio::time::timeout(d, travail).await {
                    Ok(r) => r,
                    Err(_) => {
                        tracing::warn!("admin request {id} exceeded its {} ms budget", d.as_millis());
                        AdminResult::Expired
                    }
                },
                None => travail.await,
            };
            // Le destinataire a pu partir (cœur déconnecté) : rien à faire.
            let _ = tx.send(AdminResponse { id, result }).await;
        });
    }
    drop(tx);
    let _ = ecrivain.await;
    Ok(())
}

async fn traite_admin<P: AdminPlugin>(
    plugin: std::sync::Arc<tokio::sync::RwLock<P>>,
    actifs: std::sync::Arc<std::sync::Mutex<HashMap<String, (String, String)>>>,
    req: AdminReq,
) -> AdminResult {
    match req {
        AdminReq::Ping => AdminResult::Pong,
        AdminReq::GetAsset(path) => {
            let connu = actifs.lock().unwrap().get(&path).cloned();
            let trouve = match connu {
                Some(a) => Some(a),
                None => {
                    let lu = plugin.read().await.asset(&path);
                    if let Some(a) = &lu {
                        actifs.lock().unwrap().insert(path.clone(), a.clone());
                    }
                    lu
                }
            };
            match trouve {
                Some((mime, body)) => AdminResult::Asset { mime, body: Some(body) },
                None => AdminResult::Asset { mime: "text/plain".to_string(), body: None },
            }
        }
        AdminReq::GetCatalog => AdminResult::Catalog(plugin.read().await.catalog()),
        AdminReq::GetData => AdminResult::Data(plugin.read().await.get_data().await),
        AdminReq::SetData(data) => match plugin.write().await.set_data(data).await {
            Ok(()) => AdminResult::Set { ok: true, error: None },
            Err(msg) => AdminResult::Set { ok: false, error: Some(msg) },
        },
    }
}

/// Enveloppe historique : lie puis sert. Conservée pour les appels directs et
/// pour les tests de protocole, qui ne doivent pas bouger.
pub async fn run_admin_plugin(plugin: impl AdminPlugin, socket_path: &Path) -> Result<()> {
    serve_admin(bind_admin(socket_path)?, plugin).await
}

#[cfg(test)]
mod admin_server_tests {
    use super::*;
    use ritornello_proto::{AdminResponse, AdminResult};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    struct FakeAdmin {
        data: serde_json::Value,
        /// Durée d'un `set_data` : simule le montage réseau qui n'aboutit pas.
        lenteur_set: std::time::Duration,
    }

    #[async_trait::async_trait]
    impl AdminPlugin for FakeAdmin {
        fn asset(&self, path: &str) -> Option<(String, String)> {
            match path {
                "ui.js" => Some(("text/javascript".into(), "export const contract = 1".into())),
                _ => None,
            }
        }
        fn catalog(&self) -> serde_json::Value {
            serde_json::json!({ "btn_save": "Enregistrer" })
        }
        async fn get_data(&self) -> serde_json::Value {
            self.data.clone()
        }
        async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
            tokio::time::sleep(self.lenteur_set).await;
            if data.get("bad").is_some() {
                return Err("refus".into());
            }
            self.data = data;
            Ok(())
        }
    }

    fn fake_lent(secs: u64) -> FakeAdmin {
        FakeAdmin { data: serde_json::json!({}), lenteur_set: std::time::Duration::from_secs(secs) }
    }

    async fn client_connecte(
        plugin: FakeAdmin,
    ) -> (BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        // Le socket doit survivre au test : le répertoire est abandonné.
        std::mem::forget(dir);
        let listener = bind_admin(&socket).unwrap();
        tokio::spawn(async move { serve_admin(listener, plugin).await.unwrap() });
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (r, w) = stream.into_split();
        (BufReader::new(r), w)
    }

    async fn ligne(r: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> AdminResponse {
        let mut s = String::new();
        r.read_line(&mut s).await.unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[tokio::test]
    async fn un_set_data_lent_ne_retient_pas_ui_js() {
        // L'incident du partage muet : la boucle admin était sérielle, donc un
        // seul appel système qui n'aboutit pas retenait `ui.js`, un simple
        // `include_str!`. Ici `set_data` dort 3 s ; l'actif doit revenir bien
        // avant, et **avant** la réponse du set.
        let (mut r, mut w) = client_connecte(fake_lent(3)).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"req\":\"GetAsset\",\"arg\":\"ui.js\"}\n").await.unwrap();
        let debut = std::time::Instant::now();
        let premiere = ligne(&mut r).await;
        assert_eq!(premiere.id, 2, "l'actif doit repondre avant le set lent");
        assert!(debut.elapsed() < std::time::Duration::from_secs(1), "{:?}", debut.elapsed());
        let seconde = ligne(&mut r).await;
        assert_eq!(seconde.id, 1);
        assert_eq!(seconde.result, AdminResult::Set { ok: true, error: None });
    }

    #[tokio::test]
    async fn le_budget_est_tenu_par_le_serveur() {
        // Le cœur accorde 200 ms ; le set en prend 3 s : le greffon le dit
        // lui-même (`Expired`) au lieu de laisser le client deviner.
        let (mut r, mut w) = client_connecte(fake_lent(3)).await;
        w.write_all(b"{\"id\":1,\"deadline_ms\":200,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        let debut = std::time::Instant::now();
        let rep = ligne(&mut r).await;
        assert_eq!(rep.result, AdminResult::Expired);
        assert!(debut.elapsed() < std::time::Duration::from_secs(2), "{:?}", debut.elapsed());
    }

    #[tokio::test]
    async fn ping_repond_pong_meme_pendant_un_set_data() {
        let (mut r, mut w) = client_connecte(fake_lent(3)).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"deadline_ms\":500,\"req\":\"Ping\"}\n").await.unwrap();
        let rep = ligne(&mut r).await;
        assert_eq!((rep.id, rep.result), (2, AdminResult::Pong));
    }

    #[tokio::test]
    async fn get_catalog_attend_le_verrou_dans_son_budget_puis_expire() {
        // Le catalogue lit l'état du greffon, donc attend la fin d'un
        // `set_data` en cours ; si le budget est plus court que ce set, c'est
        // `Expired`, pas un silence.
        let (mut r, mut w) = client_connecte(fake_lent(3)).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"deadline_ms\":300,\"req\":\"GetCatalog\"}\n").await.unwrap();
        let rep = ligne(&mut r).await;
        assert_eq!((rep.id, rep.result), (2, AdminResult::Expired));
    }

    #[tokio::test]
    async fn getasset_getdata_setdata_getcatalog_dialogue() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let socket_srv = socket.clone();
        tokio::spawn(async move {
            run_admin_plugin(
                FakeAdmin { data: serde_json::json!({"n": 1}), lenteur_set: std::time::Duration::ZERO },
                &socket_srv,
            )
                .await
                .unwrap();
        });

        let mut stream = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = stream.expect("connexion admin").into_split();
        let mut lines = BufReader::new(read).lines();

        write.write_all(b"{\"id\":1,\"req\":\"GetAsset\",\"arg\":\"ui.js\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Asset { body: Some(ref b), .. } if b.contains("contract")));

        write.write_all(b"{\"id\":2,\"req\":\"GetData\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Data(ref v) if v["n"] == 1));

        write.write_all(b"{\"id\":3,\"req\":\"SetData\",\"arg\":{\"bad\":true}}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Set { ok: false, .. }));

        write.write_all(b"{\"id\":4,\"req\":\"GetAsset\",\"arg\":\"inconnu.txt\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Asset { body: None, .. }));

        write.write_all(b"{\"id\":5,\"req\":\"GetCatalog\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Catalog(ref v) if v["btn_save"] == "Enregistrer"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::SourceAction;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    #[test]
    fn une_liste_vide_devient_une_absence_sur_les_deux_constructeurs() {
        // Les **deux**, dans le même test, parce que c'est leur divergence qui
        // était le défaut : `SourceOutcome::presets` se contentait de demander
        // « appelez-moi avec une liste non vide » et `Notification::presets` ne
        // disait rien du tout. Une Source suivant la documentation à la lettre
        // relayait donc une liste vide par le chemin spontané — une trame qui ne
        // déclare rien. La propriété est maintenant dérivée des deux côtés, et
        // c'est ce que ce test épingle.
        assert_eq!(
            SourceOutcome::new(SourceAction::Noop).presets(Vec::new()).presets,
            None,
            "SourceOutcome doit normaliser la liste vide en absence"
        );
        assert_eq!(
            Notification::new().presets(Vec::new()).presets,
            None,
            "Notification doit la normaliser de la meme facon"
        );
    }

    #[test]
    fn une_liste_non_vide_voyage_telle_quelle_sur_les_deux_constructeurs() {
        // Le pendant du test ci-dessus : la normalisation ne doit pas avaler ce
        // qu'une source déclare réellement.
        let liste = vec![Preset { index: 5, name: "FIP".into() }];
        assert_eq!(
            SourceOutcome::new(SourceAction::Noop).presets(liste.clone()).presets,
            Some(liste.clone())
        );
        assert_eq!(Notification::new().presets(liste.clone()).presets, Some(liste));
    }

    #[test]
    fn le_compte_du_builder_atterrit_dans_la_trame() {
        let o = SourceOutcome::new(SourceAction::Noop).preset_count(23);
        assert_eq!(o.preset_count, Some(23));
        let n = Notification::new().preset_count(0);
        assert_eq!(n.preset_count, Some(0));
    }

    #[test]
    fn le_nom_du_builder_atterrit_dans_la_trame() {
        let o = SourceOutcome::new(SourceAction::Noop).preset(4).preset_name("FIP");
        assert_eq!(o.preset, Some(4));
        assert_eq!(o.preset_name.as_deref(), Some("FIP"));
    }

    #[test]
    fn la_notification_porte_une_pochette_par_son_constructeur() {
        let n = Notification::new()
            .cover(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/cover.jpg".into() });
        assert_eq!(
            n.cover,
            Some(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/cover.jpg".into() })
        );
        // Les autres champs ne bougent pas : c'est le piege d'un builder.
        assert_eq!(n.preset, None);
        assert_eq!(n.status, None);
        assert!(!n.transient);
    }

    #[test]
    fn le_statut_du_builder_atterrit_dans_la_trame() {
        let o = SourceOutcome::new(SourceAction::Noop).status("PAS DE DISQUE");
        assert_eq!(o.status.as_deref(), Some("PAS DE DISQUE"));
        let n = Notification::new().status("FIP").preset_name("FIP");
        assert_eq!(n.status.as_deref(), Some("FIP"));
        assert_eq!(n.preset_name.as_deref(), Some("FIP"));
    }

    struct EchoSource;

    #[async_trait::async_trait]
    impl SourcePlugin for EchoSource {
        async fn activate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::play("http://fip"))
                .plays(serde_json::json!({"kind": "stream", "url": "http://fip"}))
        }
        async fn deactivate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Stop).plays_nothing()
        }
        async fn select(&mut self, n: u8) -> SourceOutcome {
            SourceOutcome::new(SourceAction::play(format!("http://station-{n}")))
        }
        async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
    }

    #[tokio::test]
    async fn dialogue_requete_reponse() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        // laisse le temps au serveur de lier le socket
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin");
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(msg.action, Some(SourceAction::play("http://fip")));
        assert_eq!(
            msg.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream", "url": "http://fip"})))
        );
        // L'estampille de capacité d'éjection, **dérivée** du plugin et non
        // déclarée par lui : `EchoSource` ne surcharge pas `can_eject`, donc la
        // valeur doit être `Some(false)` — présente, et fausse. C'est
        // `Some(_)` qui porte la propriété (voir le test dédié aux deux chemins
        // de trame) ; `false` prouve en plus qu'elle n'est pas câblée en dur.
        assert_eq!(
            msg.can_eject,
            Some(false),
            "la reponse correlee doit porter la capacite lue sur le plugin : {line}"
        );

        write.write_all(b"{\"id\":2,\"req\":\"Select\",\"arg\":3}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(2));
        assert_eq!(msg.action, Some(SourceAction::play("http://station-3")));
    }

    #[tokio::test]
    async fn can_eject_est_estampille_sur_les_deux_chemins_de_trame() {
        // **La ligne porteuse que rien n'épinglait.** `serve_source` écrit deux
        // sortes de trames — la réponse corrélée à une requête, et la
        // notification spontanée — et estampille `can_eject: Some(…)` sur
        // chacune. C'est l'un des deux mécanismes qui tiennent fermée une classe
        // de défaut apparue **trois fois** dans ce chantier : une trame relayée
        // qui ne déclare ni identité ni statut *efface* le statut mémorisé de la
        // source côté cœur, et c'est l'estampille qui garantit que le prédicat de
        // trame intéressante voit toujours quelque chose. Un chemin oublié, et
        // « PAS DE DISQUE » disparaîtrait de l'écran.
        //
        // Les **deux** chemins dans un seul test, parce que c'est la double
        // estampille qui est la propriété : la prouver sur un seul chemin
        // laisserait l'autre libre de régresser.
        //
        // La notification ne porte qu'une **pochette**, sans identité ni statut :
        // c'est la forme réelle d'une notification spontanée de production, celle
        // qui a justement besoin de l'estampille pour être relayée.
        struct SourceEjectable {
            annoncee: bool,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for SourceEjectable {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            fn can_eject(&self) -> bool {
                true
            }
            async fn poll_notification(&mut self) -> Option<Notification> {
                if self.annoncee {
                    // Une seule notification, puis plus jamais : `pending` et non
                    // `None`, qui serait terminal et désarmerait le bras.
                    std::future::pending().await
                } else {
                    self.annoncee = true;
                    Some(Notification::new().cover(ritornello_proto::CoverRef::Path {
                        path: "/mnt/nas/A/folder.jpg".into(),
                    }))
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(SourceEjectable { annoncee: false }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();

        // Les deux trames arrivent dans un ordre que l'ordre des bras du
        // `select!` ne garantit pas : on lit les deux et on les trie sur `id`,
        // plutôt que de supposer laquelle vient d'abord. Aucune marge de temps —
        // les deux doivent arriver, donc les deux sont attendues.
        let mut correlee = None;
        let mut spontanee = None;
        for _ in 0..2 {
            let line = lines.next_line().await.unwrap().expect("le plugin doit ecrire deux trames");
            let msg: SourceMessage = serde_json::from_str(&line).unwrap();
            if msg.id.is_some() {
                correlee = Some((msg, line));
            } else {
                spontanee = Some((msg, line));
            }
        }

        let (correlee, ligne_c) = correlee.expect("la reponse correlee a Activate");
        assert_eq!(correlee.id, Some(1));
        assert_eq!(
            correlee.can_eject,
            Some(true),
            "chemin 1 : la reponse correlee doit estampiller la capacite : {ligne_c}"
        );

        let (spontanee, ligne_s) = spontanee.expect("la notification spontanee");
        assert_eq!(
            spontanee.cover,
            Some(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/folder.jpg".into() }),
            "la notification testee doit bien etre celle qui ne porte qu'une pochette : {ligne_s}"
        );
        assert!(
            spontanee.identity.is_none() && spontanee.status.is_none(),
            "sans quoi la trame se qualifierait d'elle-meme et l'estampille ne serait plus \
             porteuse : {ligne_s}"
        );
        assert_eq!(
            spontanee.can_eject,
            Some(true),
            "chemin 2 : la notification spontanee doit estampiller la capacite aussi : {ligne_s}"
        );
    }

    /// Source dont le flux de notifications se tarit : premier appel `None`,
    /// puis compte les re-polls — il ne doit pas y en avoir.
    struct SourceTarie {
        polls: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait::async_trait]
    impl SourcePlugin for SourceTarie {
        async fn activate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Noop)
        }
        async fn deactivate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Noop)
        }
        async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
        async fn poll_notification(&mut self) -> Option<Notification> {
            let n = self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                None
            } else {
                std::future::pending().await
            }
        }
    }

    #[tokio::test]
    async fn un_none_de_poll_notification_est_terminal_et_nest_pas_re_polle() {
        // Régression (revue 2026-07-27) : `None` était ignoré et le bras
        // re-pollé immédiatement — boucle chaude à 100 % CPU pendant que les
        // requêtes continuaient d'être servies. Le cas est réel : le plugin cd
        // rend `None` si sa tâche de veille du lecteur meurt.
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        let polls_serveur = polls.clone();
        tokio::spawn(async move {
            run_source_plugin(SourceTarie { polls: polls_serveur }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        // Les requêtes restent servies après le tarissement…
        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        // …et le `None` n'a été lu qu'une fois : pas de re-poll. La pause
        // laisse à la boucle le temps de consommer le `None` (l'ordre des bras
        // d'un `select!` est aléatoire) — avec l'ancien code, le compteur
        // serait à 2 ici, le bras ayant été re-pollé aussitôt.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn wake_par_defaut_delegue_a_activate() {
        // EchoSource ne surcharge PAS wake() : doit se comporter comme activate().
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Wake\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.action, Some(SourceAction::play("http://fip")));
    }

    #[tokio::test]
    async fn wake_surcharge_est_dispatche() {
        struct WakingSource;
        #[async_trait::async_trait]
        impl SourcePlugin for WakingSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::play("http://activate")) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn wake(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::play("http://wake")) }
        }
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(WakingSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Wake\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        // wake() dispatché (http://wake), PAS activate() (http://activate).
        assert_eq!(msg.action, Some(SourceAction::play("http://wake")));
    }

    #[tokio::test]
    async fn set_locale_est_transmis_au_plugin_et_repond_noop() {
        use std::sync::{Arc, Mutex};
        struct RecordingLocale {
            vu: Arc<Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for RecordingLocale {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn set_locale(&mut self, locale: String) {
                *self.vu.lock().unwrap() = Some(locale);
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        let vu = Arc::new(Mutex::new(None));
        let vu_srv = vu.clone();
        tokio::spawn(async move {
            run_source_plugin(RecordingLocale { vu: vu_srv }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"SetLocale\",\"arg\":\"fr\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(msg.action, Some(SourceAction::Noop));
        assert_eq!(vu.lock().unwrap().as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn list_presets_repond_un_noop_correlable_et_la_liste_a_cote() {
        // Les deux moitiés de la propriété, dans une seule trame : le `Noop`
        // (sans lui, le `oneshot` du `SourceClient`, qui exige
        // `(Some(id), Some(action))`, attendrait les 5 s du délai) et la liste,
        // qui voyage à côté et non dans l'action.
        struct SourceNommante;
        #[async_trait::async_trait]
        impl SourcePlugin for SourceNommante {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn list_presets(&mut self) -> Vec<Preset> {
                vec![
                    Preset { index: 1, name: "FIP".into() },
                    Preset { index: 5, name: "France Info".into() },
                ]
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(SourceNommante, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"ListPresets\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(
            msg.action,
            Some(SourceAction::Noop),
            "sans action, la correlation ne se denoue pas et l'appelant attend 5 s: {line}"
        );
        assert_eq!(
            msg.presets.as_deref(),
            Some(
                &[
                    Preset { index: 1, name: "FIP".into() },
                    Preset { index: 5, name: "France Info".into() },
                ][..]
            ),
            "{line}"
        );
    }

    #[tokio::test]
    async fn une_source_qui_nenumere_pas_ne_declare_aucune_liste() {
        // `EchoSource` ne surcharge PAS `list_presets` : le corps par défaut
        // rend `Vec::new()`, et le bras doit le taire — « pas de noms » et
        // « rien dit » étant le même propos, une seule des deux écritures
        // voyage.
        //
        // Ce n'est pas cosmétique : un `"presets":[]` sur le fil passerait le
        // prédicat de trame intéressante du `SourceClient`, et une trame relayée
        // qui ne déclare ni identité ni statut **efface** le statut mémorisé du
        // cœur. Chaque source qui ne nomme rien blanchirait ainsi son
        // « PAS DE DISQUE » à la première énumération. La preuve de bout en
        // bout est côté client :
        // `une_source_qui_nenumere_pas_ne_reveille_pas_le_coeur`.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"ListPresets\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: SourceMessage = serde_json::from_str(&line).unwrap();
        // La corrélation se dénoue quand même : le `Noop` est là.
        assert_eq!(msg.action, Some(SourceAction::Noop));
        assert_eq!(msg.presets, None, "{line}");
        assert!(!line.contains("presets"), "rien de la liste ne doit voyager: {line}");
    }

    #[tokio::test]
    async fn une_notification_spontanee_peut_republier_les_preselections() {
        // Le chemin du renommage : la radio réenregistre sa configuration et
        // repousse son catalogue sans qu'on le lui redemande. La trame est
        // spontanée (aucun `id`) et ne porte aucune action.
        struct Renommante {
            emis: bool,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for Renommante {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn poll_notification(&mut self) -> Option<Notification> {
                if self.emis {
                    std::future::pending::<()>().await;
                }
                self.emis = true;
                Some(
                    Notification::new()
                        .presets(vec![Preset { index: 2, name: "Nova".into() }]),
                )
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(Renommante { emis: false }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, _write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, None, "une notification spontanee n'est pas correlee: {line}");
        assert_eq!(
            msg.presets.as_deref(),
            Some(&[Preset { index: 2, name: "Nova".into() }][..]),
            "{line}"
        );
    }

    #[tokio::test]
    async fn une_notification_spontanee_porte_lidentite() {
        // C'est le chemin du changement de piste d'un disque et de l'arrivée
        // différée d'une TOC : aucune requête du cœur, mais l'identité change.
        struct Spontanee {
            emis: bool,
        }
        #[async_trait::async_trait]
        impl SourcePlugin for Spontanee {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn poll_notification(&mut self) -> Option<Notification> {
                if self.emis {
                    std::future::pending::<()>().await;
                }
                self.emis = true;
                Some(Notification::new().plays(serde_json::json!({"kind": "disc", "track": 2})))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(Spontanee { emis: false }, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, _write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, None, "une notification n'est correlee a aucune requete");
        assert_eq!(msg.action, None, "une notification ne declenche jamais d'action");
        assert_eq!(
            msg.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({"kind": "disc", "track": 2})))
        );
    }

    #[tokio::test]
    async fn source_ignore_ligne_invalide_et_repond_a_la_suivante() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        // Ligne malformée : doit être ignorée (warn + continue), sans fermer la connexion.
        write.write_all(b"ceci n'est pas du json\n").await.unwrap();
        // Requête valide ensuite : réponse normale attendue.
        write.write_all(b"{\"id\":7,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(7));
        assert_eq!(msg.action, Some(SourceAction::play("http://fip")));
    }

    struct EnMemoire {
        recus: std::sync::Arc<std::sync::Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for EnMemoire {
        async fn show(&mut self, state: PlayerState) -> Result<()> {
            self.recus.lock().unwrap().push(state);
            Ok(())
        }
    }

    #[tokio::test]
    async fn bind_puis_serve_equivaut_a_run() {
        // La scission ne doit rien changer au comportement observable : un
        // socket lié par `bind_display` accepte une connexion AVANT que
        // `serve_display` ne tourne (c'est le backlog du noyau, et c'est ce
        // qui rend l'annonce du Runtime fiable).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let listener = bind_display(&socket).unwrap();

        // Personne ne sert encore : la connexion doit néanmoins aboutir.
        let stream = UnixStream::connect(&socket).await.expect("le backlog accepte avant accept()");

        let recus = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recus_plugin = recus.clone();
        tokio::spawn(async move {
            serve_display(listener, EnMemoire { recus: recus_plugin }).await.unwrap();
        });

        let (_r, mut w) = stream.into_split();
        let trame = DisplayFrame::State(PlayerState::default());
        w.write_all(format!("{}\n", serde_json::to_string(&trame).unwrap()).as_bytes())
            .await
            .unwrap();

        for _ in 0..100 {
            if recus.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("l'etat n'a pas atteint le plugin");
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use ritornello_proto::PlayerState;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct RecordingDisplay {
        etats: Arc<Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for RecordingDisplay {
        async fn show(&mut self, state: PlayerState) -> Result<()> {
            self.etats.lock().unwrap().push(state);
            Ok(())
        }
    }

    #[tokio::test]
    async fn recoit_letat_du_lecteur_en_ligne() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let plugin = RecordingDisplay::default();
        let etats = plugin.etats.clone();
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            let _ = run_display_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin display");
        use tokio::io::AsyncWriteExt;
        let mut write = stream;
        let e = PlayerState { source: "radio".into(), preset: Some(1), preset_name: Some("FIP".into()), ..Default::default() };
        let trame = DisplayFrame::State(e.clone());
        write.write_all(format!("{}\n", serde_json::to_string(&trame).unwrap()).as_bytes()).await.unwrap();

        for _ in 0..50 {
            if !etats.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(etats.lock().unwrap().as_slice(), &[e]);
    }

    #[tokio::test]
    async fn un_afficheur_qui_ignore_le_catalogue_recoit_quand_meme_les_etats() {
        // La propriété du corps par défaut : `RecordingDisplay` ne surcharge pas
        // `catalogue` — comme `console` et les trois autres bouchons, qui n'ont
        // pas été touchés — et une trame de catalogue ne doit ni le casser, ni
        // lui faire perdre la trame suivante.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        let etats = plugin.etats.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;

        // D'abord le catalogue, que ce plugin ignore…
        let cat = DisplayFrame::Catalogue(Catalogue {
            sources: vec![ritornello_proto::SourceCatalogue {
                name: "radio".into(),
                presets: vec![Preset { index: 1, name: "FIP".into() }],
            }],
        });
        w.write_all(format!("{}\n", serde_json::to_string(&cat).unwrap()).as_bytes())
            .await
            .unwrap();
        // …puis l'état, qui doit arriver.
        let e = PlayerState { source: "radio".into(), preset: Some(1), ..Default::default() };
        let etat = DisplayFrame::State(e.clone());
        w.write_all(format!("{}\n", serde_json::to_string(&etat).unwrap()).as_bytes())
            .await
            .unwrap();

        for _ in 0..100 {
            if !etats.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // Un seul reçu, et c'est bien l'état : le catalogue n'a pas été pris
        // pour un état vide, et n'a pas fait tomber la connexion.
        assert_eq!(
            etats.lock().unwrap().as_slice(),
            &[e],
            "l'etat doit passer malgre le catalogue, et le catalogue ne doit pas passer pour un etat"
        );
    }

    #[tokio::test]
    async fn un_afficheur_interesse_recoit_le_catalogue() {
        // Le pendant : le corps par défaut ne doit pas *avaler* le catalogue.
        // Sans le bras d'aiguillage de `serve_display`, ce plugin ne verrait
        // jamais rien.
        #[derive(Clone, Default)]
        struct Interesse {
            catalogues: Arc<Mutex<Vec<Catalogue>>>,
        }
        #[async_trait::async_trait]
        impl DisplayPlugin for Interesse {
            async fn show(&mut self, _state: PlayerState) -> Result<()> {
                Ok(())
            }
            async fn catalogue(&mut self, c: Catalogue) -> Result<()> {
                self.catalogues.lock().unwrap().push(c);
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = Interesse::default();
        let vus = plugin.catalogues.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        let attendu = Catalogue {
            sources: vec![ritornello_proto::SourceCatalogue {
                name: "radio".into(),
                presets: vec![Preset { index: 99, name: "Nova".into() }],
            }],
        };
        let trame = DisplayFrame::Catalogue(attendu.clone());
        w.write_all(format!("{}\n", serde_json::to_string(&trame).unwrap()).as_bytes())
            .await
            .unwrap();
        for _ in 0..100 {
            if !vus.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(vus.lock().unwrap().as_slice(), &[attendu]);
    }

    #[tokio::test]
    async fn une_trame_illisible_ne_ferme_pas_la_connexion() {
        // La politique de ligne illisible ne change pas avec l'enveloppe :
        // `warn` puis `continue`. Une trame d'un genre que ce SDK ne connaît pas
        // tombe dans le même cas — c'est ce qui rend l'ajout d'un genre non
        // cassant dans les deux sens. Une trame d'un genre **connu** dont la
        // charge utile est mal formée (le `cover` sans ses champs ci-dessous)
        // aussi : c'est le même chemin d'erreur de serde.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        let etats = plugin.etats.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        w.write_all(b"ceci n'est pas du json\n").await.unwrap();
        w.write_all(b"{\"frame\":\"cover\",\"data\":{\"url\":\"x\"}}\n").await.unwrap();
        w.write_all(b"{\"frame\":\"genre-inexistant\",\"data\":{}}\n").await.unwrap();
        let e = PlayerState { source: "cd".into(), ..Default::default() };
        let etat = DisplayFrame::State(e.clone());
        w.write_all(format!("{}\n", serde_json::to_string(&etat).unwrap()).as_bytes())
            .await
            .unwrap();
        for _ in 0..100 {
            if !etats.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(etats.lock().unwrap().as_slice(), &[e]);
    }

    // -- la trame de pochette ------------------------------------------------

    /// Un afficheur qui n'a rien redéfini : ni `wants_covers`, ni `cover`.
    /// C'est la console, et les trois autres bouchons de ce fichier.
    #[tokio::test]
    async fn un_afficheur_qui_ignore_les_pochettes_recoit_quand_meme_les_etats() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        assert!(!plugin.wants_covers(), "le corps par defaut doit refuser les octets");
        let etats = plugin.etats.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        // Une pochette valide, que le corps par défaut doit avaler sans bruit…
        let pochette = DisplayFrame::Cover(Cover {
            href: "/api/cover/1a2b".into(),
            mime: "image/jpeg".into(),
            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0],
        });
        w.write_all(format!("{}\n", serde_json::to_string(&pochette).unwrap()).as_bytes())
            .await
            .unwrap();
        // …puis l'état, qui doit arriver : la connexion a survécu.
        let e = PlayerState { source: "cd".into(), ..Default::default() };
        w.write_all(
            format!("{}\n", serde_json::to_string(&DisplayFrame::State(e.clone())).unwrap())
                .as_bytes(),
        )
        .await
        .unwrap();
        for _ in 0..100 {
            if !etats.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(etats.lock().unwrap().as_slice(), &[e]);
    }

    #[tokio::test]
    async fn un_afficheur_interesse_recoit_les_octets_de_la_pochette() {
        #[derive(Clone, Default)]
        struct Interesse {
            pochettes: Arc<Mutex<Vec<Cover>>>,
        }
        #[async_trait::async_trait]
        impl DisplayPlugin for Interesse {
            async fn show(&mut self, _state: PlayerState) -> Result<()> {
                Ok(())
            }
            fn wants_covers(&self) -> bool {
                true
            }
            async fn cover(&mut self, c: Cover) -> Result<()> {
                self.pochettes.lock().unwrap().push(c);
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = Interesse::default();
        assert!(plugin.wants_covers());
        let vues = plugin.pochettes.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        // Des octets qui ne sont pas du texte, `0x0A` compris : c'est ce que le
        // codage du fil doit rendre intact, et le saut de ligne est justement
        // le séparateur du protocole.
        let mut octets = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        octets.extend((0u16..=255).map(|b| b as u8));
        let attendue = Cover {
            href: "/api/cover/1a2b3c4d".into(),
            mime: "image/jpeg".into(),
            bytes: octets,
        };
        w.write_all(
            format!("{}\n", serde_json::to_string(&DisplayFrame::Cover(attendue.clone())).unwrap())
                .as_bytes(),
        )
        .await
        .unwrap();
        for _ in 0..100 {
            if !vues.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(vues.lock().unwrap().as_slice(), &[attendue]);
    }

    #[tokio::test]
    async fn une_pochette_au_dela_du_plafond_est_une_ligne_illisible_et_la_connexion_survit() {
        // Le plafond du transport vu du côté qui reçoit : un refus, traité par
        // la politique de ligne illisible — `warn` puis `continue` — et non une
        // allocation de la taille annoncée. La trame d'état qui suit prouve que
        // la connexion a survécu.
        //
        // La ligne est fabriquée à la main : le producteur, lui, ne peut pas
        // émettre cela (il ne matérialise jamais au-delà du plafond), donc
        // seule une ligne écrite ici met le refus sur le chemin.
        //
        // Ce test tient aussi, depuis que le lecteur est borné, la moitié
        // « la borne du lecteur ne préempte pas celle du décodage » : cette ligne
        // dépasse `COVER_MAX_BYTES` mais reste **sous** `LIGNE_MAX`, elle traverse
        // donc le lecteur et c'est bien le désérialiseur qui la refuse — la
        // politique de refus qu'attend le brief est intacte.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let plugin = RecordingDisplay::default();
        let etats = plugin.etats.clone();
        tokio::spawn(async move {
            serve_display(listener, plugin).await.unwrap();
        });
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (_r, mut w) = stream.into_split();
        use tokio::io::AsyncWriteExt;
        let trop = "A".repeat(ritornello_proto::COVER_MAX_BYTES / 3 * 4 + 8);
        w.write_all(
            format!(
                r#"{{"frame":"cover","data":{{"href":"/api/cover/x","mime":"image/jpeg","bytes":"{trop}"}}}}{}"#,
                "\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let e = PlayerState { source: "cd".into(), ..Default::default() };
        w.write_all(
            format!("{}\n", serde_json::to_string(&DisplayFrame::State(e.clone())).unwrap())
                .as_bytes(),
        )
        .await
        .unwrap();
        for _ in 0..100 {
            if !etats.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(etats.lock().unwrap().as_slice(), &[e]);
    }

    #[tokio::test]
    async fn une_ligne_au_dela_du_plafond_est_drainee_sans_desynchroniser_le_flux() {
        // **La borne du lecteur lui-meme**, distincte du plafond de pochette.
        // Celui-la est controle au decodage, donc *apres* que la ligne entiere est
        // residente ; `lines()` n'avait, lui, aucune borne du tout — une ligne
        // sans saut de ligne faisait croitre le tampon jusqu'ou l'ecrivain voulait
        // bien aller, sur un appareil a 1 Gio.
        //
        // Ce qu'un test peut prouver ici n'est pas la residence mais ce qui
        // casserait si le drainage etait mal ecrit : la ligne au-dela du plafond
        // est **consommee jusqu'a son saut de ligne**, et celle d'apres est lue
        // comme une ligne entiere, pas comme le milieu de la precedente. Un
        // `consume` mal compte desynchroniserait le flux pour toujours.
        //
        // Le plafond est passe en parametre : l'eprouver a la vraie valeur
        // couterait 28 Mio par test pour exactement la meme logique, et cette
        // depense-la n'aurait d'autre effet que de charger la machine.
        let entree: &[u8] = b"avant\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\napres\n";
        let mut lecteur = BufReader::new(entree);
        let mut tampon = Vec::new();

        assert!(matches!(
            lit_ligne_bornee(&mut lecteur, &mut tampon, 16).await.unwrap(),
            LigneLue::Ligne
        ));
        assert_eq!(tampon, b"avant", "une ligne sous le plafond passe intacte, sans son saut de ligne");

        match lit_ligne_bornee(&mut lecteur, &mut tampon, 16).await.unwrap() {
            LigneLue::TropLongue(vus) => {
                assert_eq!(vus, 40, "le journal doit pouvoir dire l'ampleur reellement vue");
                assert!(tampon.is_empty(), "et rien de la ligne refusee ne doit rester en memoire");
            }
            LigneLue::Ligne => panic!("la ligne de 40 octets devait etre refusee, pas rendue"),
            LigneLue::Fin => panic!("le flux ne devait pas etre epuise"),
        }

        assert!(matches!(
            lit_ligne_bornee(&mut lecteur, &mut tampon, 16).await.unwrap(),
            LigneLue::Ligne
        ));
        assert_eq!(
            tampon, b"apres",
            "la ligne suivante doit etre lue entiere : la resynchronisation est la propriete \
             que ce test tient"
        );

        assert!(matches!(
            lit_ligne_bornee(&mut lecteur, &mut tampon, 16).await.unwrap(),
            LigneLue::Fin
        ));
    }

    #[test]
    fn le_plafond_de_ligne_laisse_passer_la_plus_grande_pochette_legitime() {
        // La moitie de la propriete que le test ci-dessus ne couvre pas : cette
        // borne ne doit **jamais** prendre la place du refus de `COVER_MAX_BYTES`,
        // qui est celui qui porte le message et la politique figee par le brief.
        // Une image de exactement `COVER_MAX_BYTES` a le droit d'etre emise, donc
        // sa ligne doit passer le lecteur et n'etre jugee qu'au decodage.
        //
        // Verifie par l'arithmetique plutot qu'en fabriquant la ligne : la
        // fabriquer couterait 28 Mio pour prouver une inegalite entre deux
        // constantes.
        let base64 = ritornello_proto::COVER_MAX_BYTES.div_ceil(3) * 4;
        assert!(
            LIGNE_MAX >= base64 + 512,
            "LIGNE_MAX ({LIGNE_MAX}) doit depasser le base64 de la plus grande pochette \
             ({base64}) d'une marge couvrant l'enveloppe JSON"
        );
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    /// Plugin d'essai : mémorise ce qu'on lui annonce et renvoie un
    /// enrichissement en écho de la dernière identité reçue.
    struct EnEcho {
        recus: Arc<Mutex<Vec<NowPlaying>>>,
        a_dire: Option<Enrichment>,
    }

    #[async_trait::async_trait]
    impl MetadataPlugin for EnEcho {
        async fn now_playing(&mut self, np: NowPlaying) {
            self.recus.lock().unwrap().push(np.clone());
            self.a_dire = np.identity.map(|identity| Enrichment {
                identity,
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                ..Default::default()
            });
        }
        async fn next_enrichment(&mut self) -> Enrichment {
            match self.a_dire.take() {
                Some(e) => e,
                // Rien à dire : ne se termine jamais (le futur sera abandonné
                // par le `select!` du runner dès qu'un NowPlaying arrivera).
                None => std::future::pending().await,
            }
        }
    }

    async fn connecte(socket: &std::path::Path) -> UnixStream {
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(socket).await {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("connexion au plugin metadata impossible");
    }

    #[tokio::test]
    async fn dialogue_non_correle_dans_les_deux_sens() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let socket_srv = socket.clone();
        let recus = Arc::new(Mutex::new(Vec::new()));
        let recus_plugin = recus.clone();
        tokio::spawn(async move {
            run_metadata_plugin(EnEcho { recus: recus_plugin, a_dire: None }, &socket_srv).await.unwrap();
        });

        let (read, mut write) = connecte(&socket).await.into_split();
        let mut lines = BufReader::new(read).lines();

        let np = NowPlaying {
            source: "cd".into(),
            identity: Some(serde_json::json!({"kind": "disc", "track": 0})),
            ..Default::default()
        };
        write.write_all(format!("{}\n", serde_json::to_string(&np).unwrap()).as_bytes()).await.unwrap();

        // L'enrichissement arrive sans qu'on l'ait demandé, et sans `id`.
        let line = lines.next_line().await.unwrap().unwrap();
        let e: Enrichment = serde_json::from_str(&line).unwrap();
        assert_eq!(e.identity, serde_json::json!({"kind": "disc", "track": 0}));
        assert_eq!(e.title.as_deref(), Some("So What"));
        assert!(!line.contains("\"id\""), "aucune correlation par id: {line}");
        assert_eq!(recus.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn larret_est_transmis_au_plugin() {
        // `identity: null` est le signal qui fait cesser le travail du plugin
        // (fermer une connexion HTTP, oublier son cache).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let socket_srv = socket.clone();
        let recus = Arc::new(Mutex::new(Vec::new()));
        let recus_plugin = recus.clone();
        tokio::spawn(async move {
            run_metadata_plugin(EnEcho { recus: recus_plugin, a_dire: None }, &socket_srv).await.unwrap();
        });

        let mut write = connecte(&socket).await;
        write.write_all(b"{\"source\":\"radio\",\"identity\":null}\n").await.unwrap();
        for _ in 0..50 {
            if !recus.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let recus = recus.lock().unwrap();
        assert_eq!(recus.len(), 1);
        assert_eq!(recus[0].identity, None);
        assert_eq!(recus[0].source, "radio");
    }

    #[tokio::test]
    async fn ligne_invalide_ignoree_et_la_suivante_traitee() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let socket_srv = socket.clone();
        let recus = Arc::new(Mutex::new(Vec::new()));
        let recus_plugin = recus.clone();
        tokio::spawn(async move {
            run_metadata_plugin(EnEcho { recus: recus_plugin, a_dire: None }, &socket_srv).await.unwrap();
        });

        let (read, mut write) = connecte(&socket).await.into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"ceci n'est pas du json\n").await.unwrap();
        write.write_all(b"{\"source\":\"cd\",\"identity\":{\"k\":1}}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let e: Enrichment = serde_json::from_str(&line).unwrap();
        assert_eq!(e.identity, serde_json::json!({"k": 1}));
        assert_eq!(recus.lock().unwrap().len(), 1, "seule la trame valide compte");
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use ritornello_proto::Command;

    struct FixedCommands {
        remaining: Vec<InputMessage>,
    }

    #[async_trait::async_trait]
    impl InputPlugin for FixedCommands {
        async fn next_command(&mut self) -> anyhow::Result<InputMessage> {
            if self.remaining.is_empty() {
                std::future::pending::<()>().await;
            }
            Ok(self.remaining.remove(0))
        }
    }

    #[tokio::test]
    async fn commandes_envoyees_en_ligne() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let socket_for_server = socket.clone();
        let plugin = FixedCommands {
            remaining: vec![InputMessage::from(Command::Select(3)), InputMessage::from(Command::Stop)],
        };
        tokio::spawn(async move {
            let _ = run_input_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin input");
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stream).lines();

        let l1 = lines.next_line().await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<InputMessage>(&l1).unwrap(), InputMessage::from(Command::Select(3)));
        let l2 = lines.next_line().await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<InputMessage>(&l2).unwrap(), InputMessage::from(Command::Stop));
    }

    #[tokio::test]
    async fn un_message_maintenu_serialise_held_true_un_non_maintenu_omet_le_champ() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let socket_for_server = socket.clone();
        let plugin = FixedCommands {
            remaining: vec![
                InputMessage::from(Command::VolumeUp),
                InputMessage { cmd: Command::VolumeUp, held: true },
            ],
        };
        tokio::spawn(async move {
            let _ = run_input_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin input");
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stream).lines();

        let l1 = lines.next_line().await.unwrap().unwrap();
        assert!(!l1.contains("held"), "held:false ne doit pas apparaitre sur le fil: {l1}");
        let l2 = lines.next_line().await.unwrap().unwrap();
        assert!(l2.contains("\"held\":true"), "held:true doit apparaitre sur le fil: {l2}");
    }
}
