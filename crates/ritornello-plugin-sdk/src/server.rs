use anyhow::{Context, Result};
use ritornello_proto::{
    Enrichment, IdentityUpdate, NowPlaying, PlayerState, SourceAction, SourceMessage, SourceReq,
    SourceRequest,
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
/// Chaque ligne est un `PlayerState` complet, pas une vue déjà composée : la
/// mise en page appartient au plugin (voir `ritornello-plugin-console::display`).
pub async fn serve_display(listener: UnixListener, mut plugin: impl DisplayPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, _write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let state: PlayerState = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("invalid player state ignored: {e}");
                continue;
            }
        };
        plugin.show(state).await?;
    }
    Ok(())
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

#[async_trait::async_trait]
pub trait AdminPlugin: Send + 'static {
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

/// Accepte la connexion du cœur, puis traite les requêtes admin
/// (requête/réponse corrélée par `id`) jusqu'à fermeture.
pub async fn serve_admin(listener: UnixListener, mut plugin: impl AdminPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let req: AdminRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("invalid admin request ignored: {e}");
                continue;
            }
        };
        let result = match req.req {
            AdminReq::GetAsset(path) => match plugin.asset(&path) {
                Some((mime, body)) => AdminResult::Asset { mime, body: Some(body) },
                None => AdminResult::Asset { mime: "text/plain".to_string(), body: None },
            },
            AdminReq::GetCatalog => AdminResult::Catalog(plugin.catalog()),
            AdminReq::GetData => AdminResult::Data(plugin.get_data().await),
            AdminReq::SetData(data) => match plugin.set_data(data).await {
                Ok(()) => AdminResult::Set { ok: true, error: None },
                Err(msg) => AdminResult::Set { ok: false, error: Some(msg) },
            },
        };
        let resp = AdminResponse { id: req.id, result };
        write.write_all(format!("{}\n", serde_json::to_string(&resp)?).as_bytes()).await?;
    }
    Ok(())
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
            if data.get("bad").is_some() {
                return Err("refus".into());
            }
            self.data = data;
            Ok(())
        }
    }

    #[tokio::test]
    async fn getasset_getdata_setdata_getcatalog_dialogue() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let socket_srv = socket.clone();
        tokio::spawn(async move {
            run_admin_plugin(FakeAdmin { data: serde_json::json!({"n": 1}) }, &socket_srv)
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

        write.write_all(b"{\"id\":2,\"req\":\"Select\",\"arg\":3}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(2));
        assert_eq!(msg.action, Some(SourceAction::play("http://station-3")));
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
        let etat = PlayerState::default();
        w.write_all(format!("{}\n", serde_json::to_string(&etat).unwrap()).as_bytes())
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
        write.write_all(format!("{}\n", serde_json::to_string(&e).unwrap()).as_bytes()).await.unwrap();

        for _ in 0..50 {
            if !etats.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(etats.lock().unwrap().as_slice(), &[e]);
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
