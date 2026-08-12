use anyhow::{Context, Result};
use ritornello_proto::{
    Enrichment, IdentityUpdate, NowPlaying, SourceAction, SourceMessage, SourceReq, SourceRequest,
    View,
};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// Issue d'une requête adressée à une Source : l'action que le cœur doit
/// appliquer au lecteur, éventuellement une vue à afficher, éventuellement une
/// correction de l'identité de ce qui joue.
pub struct SourceOutcome {
    pub action: SourceAction,
    pub view: Option<View>,
    /// Laissé à `None`, l'identité courante du cœur est conservée. Une Source
    /// qui sait ce qu'elle vient de mettre en lecture doit la renseigner :
    /// sans elle, aucun plugin `metadata` n'apprend le changement, et un
    /// enrichissement en vol sur le morceau précédent resterait affiché.
    pub identity: Option<IdentityUpdate>,
    /// `line2` de la vue est un remplissage que le cœur peut remplacer par une
    /// métadonnée (voir `SourceMessage::line2_replaceable`).
    pub line2_replaceable: bool,
    /// La vue est un message éphémère (voir `SourceMessage::transient`).
    pub transient: bool,
    /// Touche 1-9 correspondant à ce qui joue (voir `SourceMessage::preset`).
    pub preset: Option<u8>,
}

impl SourceOutcome {
    /// Issue portant seulement une action (ni vue, ni identité).
    pub fn new(action: SourceAction) -> Self {
        Self {
            action,
            view: None,
            identity: None,
            line2_replaceable: false,
            transient: false,
            preset: None,
        }
    }

    pub fn with_view(mut self, view: View) -> Self {
        self.view = Some(view);
        self
    }

    /// Déclare que la `line2` de la vue n'est qu'un remplissage : le cœur peut
    /// y écrire une métadonnée s'il en connaît une, et la Source récupère sa
    /// propre ligne dès qu'il n'en connaît plus.
    pub fn line2_replaceable(mut self) -> Self {
        self.line2_replaceable = true;
        self
    }

    /// Déclare la vue comme un message **éphémère** : le cœur l'affiche quelques
    /// secondes, puis fait reparaître la vue permanente précédente. À employer
    /// pour signaler un incident sans détruire l'affichage de ce qui joue.
    pub fn transient(mut self) -> Self {
        self.transient = true;
        self
    }

    /// Déclare la touche 1-9 de la télécommande à laquelle correspond ce qui
    /// joue : la présélection pour une radio, la piste pour un cd. C'est ce
    /// qui permet à l'IHM de mettre la touche active en évidence. Le cœur
    /// l'oublie de lui-même quand plus rien ne joue.
    pub fn preset(mut self, n: u8) -> Self {
        self.preset = Some(n);
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
    pub view: Option<View>,
    pub identity: Option<IdentityUpdate>,
    /// Voir `SourceOutcome::line2_replaceable`.
    pub line2_replaceable: bool,
    /// Voir `SourceMessage::transient`.
    pub transient: bool,
    /// Voir `SourceOutcome::preset`.
    pub preset: Option<u8>,
}

impl Notification {
    pub fn view(view: View) -> Self {
        Self {
            view: Some(view),
            identity: None,
            line2_replaceable: false,
            transient: false,
            preset: None,
        }
    }

    pub fn line2_replaceable(mut self) -> Self {
        self.line2_replaceable = true;
        self
    }

    /// Voir `SourceOutcome::preset`.
    pub fn preset(mut self, n: u8) -> Self {
        self.preset = Some(n);
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
}

#[async_trait::async_trait]
pub trait SourcePlugin: Send + 'static {
    async fn activate(&mut self) -> SourceOutcome;
    async fn deactivate(&mut self) -> SourceOutcome;
    async fn select(&mut self, n: u8) -> SourceOutcome;
    async fn next(&mut self) -> SourceOutcome;
    async fn prev(&mut self) -> SourceOutcome;
    async fn eject(&mut self) -> SourceOutcome;

    /// Réveil (boot / sortie de veille). Par défaut, se comporte comme
    /// `activate()` (jouer) — adapté à la radio et à toute source simple.
    /// Un plugin qui ne doit pas jouer tout seul au réveil (cd) surcharge.
    async fn wake(&mut self) -> SourceOutcome {
        self.activate().await
    }

    /// Le cœur a arrêté la lecture sans consulter la Source (touche Stop).
    ///
    /// Implémentation par défaut : déclarer que plus rien ne joue, ce qui est
    /// vrai pour toute Source, et ne rien afficher de nouveau. Une Source qui
    /// tient un état de lecture propre (le cd) surcharge pour le remettre à
    /// jour ; les autres compilent inchangées.
    async fn stop(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop).plays_nothing()
    }

    /// Le lecteur est passé de lui-même à la piste d'index `n`.
    ///
    /// Implémentation par défaut : rien — une radio n'a pas de pistes. Une Source
    /// qui suit un index (le cd) surcharge pour se recaler et rendre une vue et
    /// une identité à jour.
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

/// Lie `socket_path`, accepte une connexion (le cœur), puis traite les
/// requêtes et les notifications spontanées jusqu'à fermeture de la
/// connexion.
pub async fn run_source_plugin(mut plugin: impl SourcePlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("liaison de {}", socket_path.display()))?;
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
                        tracing::warn!("ligne source invalide ignoree: {e}");
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
                    view: outcome.view,
                    identity: outcome.identity,
                    line2_replaceable: outcome.line2_replaceable,
                    transient: outcome.transient,
                    preset: outcome.preset,
                };
                write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
            }
            notification = plugin.poll_notification(), if notifications_ouvertes => {
                match notification {
                    Some(n) => {
                        let msg = SourceMessage {
                            id: None,
                            action: None,
                            view: n.view,
                            identity: n.identity,
                            line2_replaceable: n.line2_replaceable,
                            transient: n.transient,
                            preset: n.preset,
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
                        tracing::warn!("plus de notifications spontanees (tache interne terminee)");
                        notifications_ouvertes = false;
                    }
                }
            }
        }
    }
}

#[async_trait::async_trait]
pub trait DisplayPlugin: Send + 'static {
    async fn show(&mut self, view: View) -> Result<()>;
}

/// Lie `socket_path`, accepte une connexion (le cœur), puis affiche chaque
/// vue reçue jusqu'à fermeture de la connexion. Protocole à sens unique :
/// aucune réponse n'est attendue.
pub async fn run_display_plugin(mut plugin: impl DisplayPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("liaison de {}", socket_path.display()))?;
    let (stream, _) = listener.accept().await?;
    let (read, _write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let view: View = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("vue invalide ignoree: {e}");
                continue;
            }
        };
        plugin.show(view).await?;
    }
    Ok(())
}

use ritornello_proto::InputMessage;

#[async_trait::async_trait]
pub trait InputPlugin: Send + 'static {
    async fn next_command(&mut self) -> Result<InputMessage>;
}

/// Lie `socket_path`, accepte une connexion (le cœur), puis relaie chaque
/// `InputMessage` produit par le plugin. `held: false` n'est pas sérialisé
/// (voir `InputMessage`), donc les octets sur le fil restent inchangés pour
/// les commandes non maintenues — un cœur d'avant Tâche 1 déserialiserait la
/// trame sans rien y voir de nouveau.
pub async fn run_input_plugin(mut plugin: impl InputPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    let (stream, _) = listener.accept().await?;
    let (_read, mut write) = stream.into_split();
    loop {
        let msg = plugin.next_command().await?;
        write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
    }
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

/// Lie `socket_path`, accepte une connexion (le cœur), puis relaie dans les
/// deux sens jusqu'à fermeture : chaque ligne reçue est un `NowPlaying`, chaque
/// enrichissement produit part sur le fil. Aucune corrélation par `id` : les
/// deux sens sont indépendants.
pub async fn run_metadata_plugin(mut plugin: impl MetadataPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("liaison de {}", socket_path.display()))?;
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                match serde_json::from_str::<NowPlaying>(&line) {
                    Ok(np) => plugin.now_playing(np).await,
                    Err(e) => tracing::warn!("ligne metadata invalide ignoree: {e}"),
                }
            }
            enrichment = plugin.next_enrichment() => {
                let ligne = format!("{}\n", serde_json::to_string(&enrichment)?);
                write.write_all(ligne.as_bytes()).await?;
            }
        }
    }
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

/// Lie `socket_path`, accepte une connexion (le cœur), puis traite les
/// requêtes admin (requête/réponse corrélée par `id`) jusqu'à fermeture.
pub async fn run_admin_plugin(mut plugin: impl AdminPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("liaison de {}", socket_path.display()))?;
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let req: AdminRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("requete admin invalide ignoree: {e}");
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
    use ritornello_proto::{SourceAction, View};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    struct EchoSource;

    #[async_trait::async_trait]
    impl SourcePlugin for EchoSource {
        async fn activate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Play { uri: "http://fip".into() })
                .with_view(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() })
                .plays(serde_json::json!({"kind": "stream", "url": "http://fip"}))
        }
        async fn deactivate(&mut self) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Stop).plays_nothing()
        }
        async fn select(&mut self, n: u8) -> SourceOutcome {
            SourceOutcome::new(SourceAction::Play { uri: format!("http://station-{n}") })
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
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://fip".into() }));
        assert_eq!(msg.view.unwrap().line2, "FIP");
        assert_eq!(
            msg.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream", "url": "http://fip"})))
        );

        write.write_all(b"{\"id\":2,\"req\":\"Select\",\"arg\":3}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(2));
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://station-3".into() }));
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
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://fip".into() }));
    }

    #[tokio::test]
    async fn wake_surcharge_est_dispatche() {
        struct WakingSource;
        #[async_trait::async_trait]
        impl SourcePlugin for WakingSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Play { uri: "http://activate".into() }) }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Noop) }
            async fn wake(&mut self) -> SourceOutcome { SourceOutcome::new(SourceAction::Play { uri: "http://wake".into() }) }
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
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://wake".into() }));
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
        assert!(msg.view.is_none());
        assert_eq!(vu.lock().unwrap().as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn une_notification_spontanee_porte_vue_et_identite() {
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
                Some(
                    Notification::view(View { line1: "CD 3/12".into(), line2: String::new(), line3: String::new() })
                        .plays(serde_json::json!({"kind": "disc", "track": 2})),
                )
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
        assert_eq!(msg.view.unwrap().line1, "CD 3/12");
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
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://fip".into() }));
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use ritornello_proto::View;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct RecordingDisplay {
        views: Arc<Mutex<Vec<View>>>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for RecordingDisplay {
        async fn show(&mut self, view: View) -> Result<()> {
            self.views.lock().unwrap().push(view);
            Ok(())
        }
    }

    #[tokio::test]
    async fn recoit_les_vues_en_ligne() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let plugin = RecordingDisplay::default();
        let views = plugin.views.clone();
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
        let v = View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() };
        write.write_all(format!("{}\n", serde_json::to_string(&v).unwrap()).as_bytes()).await.unwrap();

        for _ in 0..50 {
            if !views.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(views.lock().unwrap().as_slice(), &[v]);
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
