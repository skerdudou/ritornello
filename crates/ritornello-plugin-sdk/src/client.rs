use anyhow::{bail, Context, Result};
use ritornello_proto::{
    AdminReq, AdminRequest, AdminResponse, AdminResult, Catalogue, CoverRef, DisplayFrame,
    Enrichment, IdentityUpdate, InputMessage, NowPlaying, PlayerState, Preset, SourceAction,
    SourceMessage, SourceReq, SourceRequest,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

/// Ce qu'une Source rapporte spontanément ou en marge d'une réponse : une
/// correction de l'identité de ce qui joue, un statut, une présélection.
///
/// Tous ces champs voyagent ensemble parce qu'ils sont produits ensemble par
/// le plugin, dans une seule trame : les séparer en plusieurs canaux ferait
/// exister des instants où l'état affiché et l'identité annoncée aux plugins
/// `metadata` se contredisent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceUpdate {
    pub identity: Option<IdentityUpdate>,
    /// Voir `SourceMessage::transient`.
    pub transient: bool,
    /// Voir `SourceMessage::preset`. Absent = rien déclaré, garder la valeur
    /// courante.
    pub preset: Option<u8>,
    /// See `SourceMessage::preset_count`.
    pub preset_count: Option<u8>,
    /// See `SourceMessage::preset_name`.
    pub preset_name: Option<String>,
    /// See `SourceMessage::status`.
    pub status: Option<String>,
    /// See `SourceMessage::can_eject`. Absent = rien déclaré, garder la valeur
    /// courante. N'entre volontairement **pas** dans le prédicat ci-dessous
    /// qui décide si une trame vaut d'être relayée : voir la doc du champ.
    pub can_eject: Option<bool>,
    /// See `SourceMessage::presets`. Entre volontairement **dans** le prédicat
    /// ci-dessous, à l'inverse de `can_eject` : c'est la seule voie par laquelle
    /// une liste atteint le cœur, la réponse corrélée à `ListPresets` n'étant
    /// qu'un `Noop`.
    ///
    /// **Danger, à l'attention du cœur.** Une trame ne portant que des
    /// présélections ne déclare **ni identité ni statut**, et une trame
    /// permanente sans statut vaut *effacement* du statut mémorisé
    /// (`Core::handle_source_update` : `if !update.transient { self.source_status
    /// = update.status.clone(); }`). C'est la raison exacte pour laquelle
    /// `can_eject` est resté **hors** du prédicat — réveiller des trames
    /// aujourd'hui jetées effacerait « PAS DE DISQUE » de l'écran — et cette
    /// clause-ci rompt l'invariant qui rendait ce choix sûr (« tout chemin d'une
    /// vraie source déclare une identité ou un statut »).
    ///
    /// Le cœur traite donc les présélections **et rend la main avant** le
    /// traitement du statut quand la trame ne déclare ni identité ni statut
    /// (`handle_source_update`, retour anticipé) : le prédicat y reprend
    /// l'invariant mot pour mot, et couvre du même coup le cas de
    /// `preset_count`, qui le rompait déjà en service. Deux
    /// atténuations existent déjà en amont, mais aucune ne suffit : le sdk
    /// n'émet jamais de liste vide (une source qui n'énumère pas reste donc
    /// inerte, voir le bras `ListPresets` de `serve_source`), et le catalogue
    /// est un fait sur une source, pas sur ce qui joue — il se lit donc avant le
    /// garde de source active. Une source qui **énumère** (la radio) atteint
    /// bien, elle, ce chemin.
    pub presets: Option<Vec<Preset>>,
    /// Voir `SourceMessage::cover`. **Absent = rien déclaré, garder la valeur
    /// courante** — même convention que `preset`/`preset_count` : une Source
    /// ne répète pas la pochette sur chaque trame de statut qui suit, et
    /// `Core::set_cover_de_source` ne doit donc être appelé que lorsque ce
    /// champ vaut `Some`, jamais à chaque trame relayée. Envoyée seule, en
    /// notification spontanée (`id: None`), sans identité ni statut : **entre**
    /// dans le prédicat ci-dessous, sans quoi cette trame-là serait jetée en
    /// silence — c'est précisément la forme sous laquelle une pochette arrive
    /// (voir la doc de `SourceMessage::cover`, qui explique pourquoi elle
    /// n'attend pas la réponse au `Play`).
    pub cover: Option<CoverRef>,
}

pub struct SourceClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<SourceAction>>>>,
    next_id: AtomicU64,
}

impl SourceClient {
    pub async fn connect(
        socket_path: &Path,
        name: String,
        update_tx: mpsc::Sender<(String, SourceUpdate)>,
    ) -> Result<Arc<Self>> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to {}", socket_path.display()))?;
        let (read, write) = stream.into_split();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self { writer: Mutex::new(write), pending: pending.clone(), next_id: AtomicU64::new(1) });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let msg = match serde_json::from_str::<SourceMessage>(&line) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("invalid source message ignored: {e}");
                        continue;
                    }
                };
                if let (Some(id), Some(action)) = (msg.id, msg.action.clone()) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(action);
                    }
                }
                if msg.identity.is_some()
                    || msg.preset.is_some()
                    || msg.preset_count.is_some()
                    || msg.preset_name.is_some()
                    || msg.status.is_some()
                    || msg.presets.is_some()
                    // Une pochette arrive seule, en notification spontanée
                    // (voir la doc de `SourceUpdate::cover`) : sans cette
                    // entrée, une trame qui ne porterait qu'elle serait jetée
                    // par ce garde avant même d'atteindre `SourceUpdate`.
                    || msg.cover.is_some()
                {
                    let porte_identite = msg.identity.is_some();
                    let update = SourceUpdate {
                        identity: msg.identity,
                        transient: msg.transient,
                        preset: msg.preset,
                        preset_count: msg.preset_count,
                        preset_name: msg.preset_name,
                        status: msg.status,
                        can_eject: msg.can_eject,
                        presets: msg.presets,
                        cover: msg.cover,
                    };
                    if update_tx.try_send((name.clone(), update)).is_err() {
                        // Un statut ou une présélection perdus sont réparés par
                        // la trame suivante, une **identité** perdue ne l'est
                        // jamais — la Source ne la réémet que sur changement,
                        // donc le cœur garde celle du morceau précédent et les
                        // plugins `metadata` continuent de l'enrichir, sans que
                        // le garde-fou de péremption y voie quoi que ce soit.
                        //
                        // Toujours `try_send` et non `send().await` : cette même
                        // tâche délivre les réponses corrélées aux requêtes du
                        // cœur. Attendre ici sur un canal plein retiendrait la
                        // réponse que le cœur attend, et le cœur ne draine le
                        // canal qu'en revenant à sa boucle — soit un blocage
                        // croisé jusqu'au timeout de 5 s de `request`. Perdre
                        // une trame en le signalant fort vaut mieux qu'une
                        // seconde d'appareil figé.
                        if porte_identite {
                            tracing::error!(
                                "identity update for {name} lost (channel full): display and metadata possibly stale until next change"
                            );
                        } else {
                            tracing::warn!("source update for {name} lost (channel full)");
                        }
                    }
                }
            }
            // Déconnexion : drainer les requêtes en vol. Dropper chaque Sender
            // fait résoudre le rx.await de request() en Err immédiatement.
            pending.lock().await.clear();
            tracing::warn!("source plugin connection closed");
        });
        Ok(client)
    }

    pub async fn request(&self, req: SourceReq) -> Result<SourceAction> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = SourceRequest { id, req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(action)) => Ok(action),
            Ok(Err(_)) => bail!("source plugin: response dropped"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("source plugin: request timeout")
            }
        }
    }
}

pub struct DisplayClient {
    writer: Mutex<OwnedWriteHalf>,
}

impl DisplayClient {
    pub async fn connect(socket_path: &Path) -> Result<Arc<Self>> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to {}", socket_path.display()))?;
        let (_read, write) = stream.into_split();
        Ok(Arc::new(Self { writer: Mutex::new(write) }))
    }

    /// Pousse un état. Sur le fil, c'est une `DisplayFrame::State` : l'ancienne
    /// charge utile inchangée, dans une enveloppe à étiquetage adjacent.
    pub async fn send(&self, state: &PlayerState) -> Result<()> {
        self.envoyer(&DisplayFrame::State(state.clone())).await
    }

    /// Pousse le catalogue des sources. Jumeau de `send`, sur son propre canal :
    /// élargir la charge utile de l'état aurait republié l'état à chaque
    /// changement de catalogue et l'inverse, ce que la déduplication par égalité
    /// du cœur ne rattrape pas — les deux valeurs changeraient ensemble par
    /// construction.
    pub async fn send_catalogue(&self, catalogue: &Catalogue) -> Result<()> {
        self.envoyer(&DisplayFrame::Catalogue(catalogue.clone())).await
    }

    async fn envoyer(&self, frame: &DisplayFrame) -> Result<()> {
        let ligne = format!("{}\n", serde_json::to_string(frame)?);
        let mut w = self.writer.lock().await;
        w.write_all(ligne.as_bytes()).await?;
        Ok(())
    }
}

/// Panne du dialogue d'admin avec un plugin, **typée** pour que le cœur puisse
/// la distinguer.
///
/// Une chaîne ne suffisait pas : le cœur aplatissait tout en « plugin
/// injoignable », si bien qu'un plugin mort et un plugin qui répond trop
/// lentement recevaient le même message — le premier appelle un redémarrage, le
/// second envoie regarder le réseau.
///
/// Les libellés restent en **anglais** : ils partent dans les journaux, comme
/// tous les messages de ce crate. Ce qui atteint l'écran vient du catalogue du
/// cœur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminIpcError {
    /// Le plafond de 5 s a été atteint : le plugin vit, mais répond trop tard.
    Timeout,
    /// Le socket est tombé, ou la requête a été drainée par une déconnexion.
    Closed,
}

impl std::fmt::Display for AdminIpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Formulations inchangées : elles sont déjà dans les journaux des
            // appareils en service, et les changer casserait toute recherche
            // portant dessus.
            Self::Timeout => write!(f, "admin plugin: request timeout"),
            Self::Closed => write!(f, "admin plugin: response dropped"),
        }
    }
}

impl std::error::Error for AdminIpcError {}

pub struct AdminClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AdminResult>>>>,
    next_id: AtomicU64,
}

impl AdminClient {
    pub async fn connect(socket_path: &Path) -> Result<Arc<Self>> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("connecting to {}", socket_path.display()))?;
        let (read, write) = stream.into_split();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AdminResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self {
            writer: Mutex::new(write),
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let resp = match serde_json::from_str::<AdminResponse>(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("invalid admin response ignored: {e}");
                        continue;
                    }
                };
                if let Some(tx) = pending.lock().await.remove(&resp.id) {
                    let _ = tx.send(resp.result);
                }
            }
            // Déconnexion : drainer les requêtes en vol (voir SourceClient).
            pending.lock().await.clear();
            tracing::warn!("admin plugin connection closed");
        });
        Ok(client)
    }

    async fn request(&self, req: AdminReq) -> Result<AdminResult> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = AdminRequest { id, req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(AdminIpcError::Closed.into()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(AdminIpcError::Timeout.into())
            }
        }
    }

    pub async fn get_asset(&self, path: &str) -> Result<Option<(String, String)>> {
        match self.request(AdminReq::GetAsset(path.to_string())).await? {
            AdminResult::Asset { mime, body } => Ok(body.map(|b| (mime, b))),
            autre => anyhow::bail!("unexpected response to GetAsset: {autre:?}"),
        }
    }

    pub async fn get_catalog(&self) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetCatalog).await? {
            AdminResult::Catalog(v) => Ok(v),
            autre => anyhow::bail!("unexpected response to GetCatalog: {autre:?}"),
        }
    }

    pub async fn get_data(&self) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetData).await? {
            AdminResult::Data(v) => Ok(v),
            other => bail!("unexpected admin response for GetData: {other:?}"),
        }
    }

    pub async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        match self.request(AdminReq::SetData(data)).await? {
            AdminResult::Set { ok: true, .. } => Ok(Ok(())),
            AdminResult::Set { ok: false, error } => Ok(Err(error.unwrap_or_default())),
            other => bail!("unexpected admin response for SetData: {other:?}"),
        }
    }
}

/// Se connecte à un plugin `metadata` et fait circuler les deux sens jusqu'à
/// fermeture : ce qui joue descend vers le plugin, ses enrichissements montent
/// vers le cœur, étiquetés de son nom (c'est le nom qui départage deux plugins,
/// selon l'ordre de déclaration dans `plugins.toml`).
///
/// Le sens descendant passe par un `watch` et non par un `mpsc` : seule la
/// dernière valeur compte, les intermédiaires n'ont aucune valeur, et surtout
/// **un plugin lent ne peut pas bloquer le cœur**. Si le cœur attendait
/// l'écriture sur cette socket depuis sa boucle principale, un plugin qui ne
/// lit plus (mais dont le processus vit toujours) remplirait le tampon de la
/// socket et figerait l'appareil entier — c'est exactement pour cette raison
/// que les vues passent déjà par un `watch` plutôt que par un appel direct.
///
/// L'état courant est envoyé **dès la connexion** : un plugin qui démarre
/// pendant qu'un morceau joue n'a pas à attendre le suivant pour se mettre au
/// travail.
///
/// Ne revient qu'en cas d'erreur ; à spawn dans une tâche dédiée par l'appelant.
pub async fn run_metadata_client(
    socket_path: &Path,
    name: String,
    enrich_tx: mpsc::Sender<(String, Enrichment)>,
    mut np_rx: tokio::sync::watch::Receiver<NowPlaying>,
) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to {}", socket_path.display()))?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    let mut a_envoyer = Some(np_rx.borrow_and_update().clone());
    loop {
        if let Some(np) = a_envoyer.take() {
            write.write_all(format!("{}\n", serde_json::to_string(&np)?).as_bytes()).await?;
        }
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    bail!("metadata plugin {name} connection closed");
                };
                match serde_json::from_str::<Enrichment>(&line) {
                    // `cleaned` ici, au plus près de l'entrée : le cœur n'a
                    // ensuite qu'une seule forme à traiter (voir `is_empty`,
                    // qui décide de l'arbitrage).
                    Ok(e) => {
                        if enrich_tx.send((name.clone(), e.cleaned())).await.is_err() {
                            bail!("core closed, stopping metadata relay {name}");
                        }
                    }
                    Err(e) => tracing::warn!("invalid enrichment from {name} ignored: {e}"),
                }
            }
            change = np_rx.changed() => {
                if change.is_err() {
                    bail!("now-playing channel closed, stopping metadata relay {name}");
                }
                a_envoyer = Some(np_rx.borrow_and_update().clone());
            }
        }
    }
}

/// Se connecte au plugin input et relaie chaque `InputMessage` reçu sur
/// `cmd_tx`, jusqu'à fermeture de la connexion (ne revient qu'en cas d'erreur ;
/// à spawn dans une tâche dédiée par l'appelant).
///
/// Accepte aussi bien l'enveloppe complète (`{"cmd":...,"held":true}`) que la
/// forme nue d'avant Tâche 1 (`{"cmd":...}`) : `InputMessage` désérialise les
/// deux, `held` retombant sur `false` en son absence.
pub async fn run_input_client(socket_path: &Path, cmd_tx: mpsc::Sender<InputMessage>) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to {}", socket_path.display()))?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        match serde_json::from_str::<InputMessage>(&line) {
            Ok(msg) => {
                // Récepteur disparu = boucle du cœur finie : continuer à lire
                // la socket pour jeter les commandes serait une fuite de
                // tâche. Même traitement que le cas symétrique du relais
                // metadata (canal now-playing fermé).
                if cmd_tx.send(msg).await.is_err() {
                    bail!("core closed, stopping input relay");
                }
            }
            Err(e) => tracing::warn!("invalid command received from input plugin: {e}"),
        }
    }
    bail!("input plugin connection closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::SourceAction;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn deux_afficheurs_coexistent_et_recoivent_le_meme_etat() {
        // Le singleton d'avant (`display_connect = Some(...)`) faisait
        // disparaitre le premier afficheur declare, sans erreur. Ce test
        // verifie la seule chose qu'il peut verifier : deux `DisplayClient`
        // vivent en parallele sur deux sockets et recoivent chacun le meme
        // etat.
        //
        // Il ne prouve PAS l'absence d'interference entre eux : deux lignes de
        // JSON n'emplissent pas le tampon d'un socket, donc l'afficheur jamais
        // lu ne bloquerait pas non plus avec une tache unique bouclant sur N
        // clients. La non-interference est garantie par construction cote
        // coeur — une tache et un socket par afficheur — pas ici. La durcir
        // par un remplissage de tampon serait lent et instable.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.sock");
        let b = dir.path().join("b.sock");
        let la = UnixListener::bind(&a).unwrap();
        let lb = UnixListener::bind(&b).unwrap();

        let client_a = DisplayClient::connect(&a).await.unwrap();
        let client_b = DisplayClient::connect(&b).await.unwrap();

        // `a` est accepte puis LU ; `b` est accepte et jamais lu.
        let (sa, _) = la.accept().await.unwrap();
        let (_sb, _) = lb.accept().await.unwrap();

        let etat = PlayerState::default();
        client_a.send(&etat).await.unwrap();
        client_b.send(&etat).await.unwrap();
        // Un second envoi vers `a` apres celui vers `b` : les deux clients
        // gardent chacun leur socket et leur verrou d'ecriture.
        client_a.send(&etat).await.unwrap();

        let mut lignes = BufReader::new(sa).lines();
        assert!(lignes.next_line().await.unwrap().is_some());
        assert!(lignes.next_line().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn source_client_correle_par_id_et_relaie_lidentite_et_la_selection() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::play("http://fip")),
                identity: Some(ritornello_proto::IdentityUpdate::Playing(
                    serde_json::json!({"kind": "stream", "url": "http://fip"}),
                )),
                transient: false,
                preset: Some(1),
                preset_count: None,
                preset_name: Some("FIP".into()),
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            let _ = socket_for_server; // garde le chemin vivant pour le débogage
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        let action = client.request(ritornello_proto::SourceReq::Activate).await.unwrap();
        assert_eq!(action, SourceAction::play("http://fip"));
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        // L'identité et la présélection arrivent dans la même mise à jour :
        // c'est ce qui garantit qu'on n'annonce jamais une station en
        // affichant l'autre.
        assert_eq!(
            update.identity,
            Some(ritornello_proto::IdentityUpdate::Playing(
                serde_json::json!({"kind": "stream", "url": "http://fip"})
            ))
        );
        // Le nom de présélection voyage dans la même mise à jour que le reste.
        assert_eq!(update.preset, Some(1));
        assert_eq!(update.preset_name.as_deref(), Some("FIP"));
    }

    #[tokio::test]
    async fn trame_seule_avec_le_compte_est_relayee() {
        // Une trame portant seulement preset_count est "intéressante"
        // et doit être relayée (même logique que preset).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: Some(5),
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(update.preset_count, Some(5));
    }

    #[tokio::test]
    async fn trame_seule_avec_la_capacite_dejection_reste_inerte_mais_voyage_avec_le_reste() {
        // Décision **volontaire**, et c'est ce test qui la tient : `can_eject`
        // n'entre pas dans le prédicat qui décide qu'une trame vaut d'être
        // relayée. Le sdk l'estampille sur chaque trame ; si elle rendait
        // « intéressante » une trame par ailleurs vide, une réponse nue
        // (`eject()` d'une radio, par exemple) atteindrait
        // `handle_source_update` — où une trame permanente sans `status`
        // **efface** le statut mémorisé. « PAS DE DISQUE » disparaîtrait de
        // l'écran à la première commande sans effet.
        //
        // La capacité arrive donc à cheval sur les trames que le cœur écoute
        // déjà : tous les chemins d'une vraie Source déclarent une identité ou
        // un statut.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // Première requête : réponse ne portant **que** la capacité.
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let nue = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: Some(true),
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&nue).unwrap()).as_bytes()).await.unwrap();
            // Seconde requête : la même capacité, cette fois accompagnée d'un
            // statut — c'est ainsi qu'elle atteint le cœur pour de vrai.
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let habillee = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: Some("AUDIO CD".into()),
                can_eject: Some(true),
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&habillee).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "cd".into(), update_tx).await.unwrap();
        client.request(SourceReq::Eject).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        // La **première** mise à jour reçue est celle de la seconde trame : la
        // trame nue n'a rien produit. Sans quoi ce `recv` rendrait un statut
        // vide, et l'assertion ci-dessous tomberait.
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "cd");
        assert_eq!(update.status.as_deref(), Some("AUDIO CD"), "la trame nue n'aurait pas du etre relayee");
        assert_eq!(update.can_eject, Some(true), "la capacite voyage avec la trame qui compte");
    }

    #[tokio::test]
    async fn trame_seule_avec_le_nom_est_relayee() {
        // C'est exactement le piège signalé par le cahier des charges : une
        // trame ne portant que `preset_name` (sans vue, identité, preset ni
        // compte) doit passer la condition qui décide qu'une trame est
        // "intéressante", sans quoi elle serait jetée en silence.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: Some("FIP".into()),
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(update.preset_name.as_deref(), Some("FIP"));
    }

    #[tokio::test]
    async fn trame_seule_avec_le_statut_est_relayee() {
        // Le même piège que pour `preset_name` (voir le cahier des charges) :
        // une trame ne portant que `status` (sans vue, identité, preset, compte
        // ni nom) doit passer la condition qui décide qu'une trame est
        // "intéressante", sans quoi elle serait jetée en silence.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: Some("PAS DE DISQUE".into()),
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(update.status.as_deref(), Some("PAS DE DISQUE"));
    }

    #[tokio::test]
    async fn une_reponse_a_list_presets_denoue_la_correlation_et_relaie_la_liste() {
        // Les deux moitiés dans le même aller-retour, et c'est le cœur du choix
        // de conception : la liste ne peut pas voyager comme la **réponse** (le
        // `oneshot` ne porte qu'un `SourceAction`), donc `request` doit rendre
        // sans attendre, et la liste arriver par le canal de mises à jour.
        // Le test **séquence** au lieu d'attendre : deux réponses, la première
        // ne portant que `presets`, la seconde ne portant qu'un statut (que le
        // prédicat relaie depuis toujours). La première mise à jour reçue doit
        // être celle des présélections. Sans le `|| msg.presets.is_some()`, ce
        // `recv` rendrait le **statut** et l'assertion tomberait tout de
        // suite — au lieu d'attendre à jamais une trame qui ne viendra pas.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(req.req, SourceReq::ListPresets);
            let liste = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: Some(vec![Preset { index: 5, name: "FIP".into() }]),
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&liste).unwrap()).as_bytes()).await.unwrap();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let statut = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: Some("RADIO".into()),
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&statut).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        // La corrélation se dénoue : sans le `Noop`, cet `await` durerait les
        // 5 s du délai puis échouerait.
        assert_eq!(client.request(SourceReq::ListPresets).await.unwrap(), SourceAction::Noop);
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(
            update.presets.as_deref(),
            Some(&[Preset { index: 5, name: "FIP".into() }][..]),
            "la premiere mise a jour doit etre celle des preselections, obtenu {update:?}"
        );
        // La seconde suit, et c'est bien le statut : l'ordre est celui du fil.
        let (_, suivante) = update_rx.recv().await.unwrap();
        assert_eq!(suivante.status.as_deref(), Some("RADIO"));
    }

    #[tokio::test]
    async fn une_source_qui_nenumere_pas_ne_reveille_pas_le_coeur() {
        // Le vrai `serve_source` face au vrai `SourceClient`, parce que le
        // défaut se joue **entre** les deux : une source qui ne surcharge pas
        // `list_presets` rend `Vec::new()`, et si cette liste vide voyageait,
        // elle passerait le prédicat de trame intéressante. Or une trame
        // relayée sans identité ni statut **efface** le statut mémorisé du
        // cœur (`Core::handle_source_update`) : « PAS DE DISQUE » disparaîtrait
        // de l'écran à la première énumération, sur toute source qui ne nomme
        // rien.
        //
        // Le test **séquence** au lieu d'attendre : après le `ListPresets`, un
        // `Activate` dont la réponse porte une identité — donc relayée à coup
        // sûr. La première mise à jour reçue doit être celle-là. Avec un
        // `Some([])`, ce serait la trame de `ListPresets`, sans identité, et
        // l'assertion tomberait sur-le-champ au lieu d'attendre en vain.
        struct SansNoms;
        #[async_trait::async_trait]
        impl crate::SourcePlugin for SansNoms {
            async fn activate(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::play("http://fip"))
                    .plays(serde_json::json!({"kind": "stream"}))
            }
            async fn deactivate(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn select(&mut self, _n: u8) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn next(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn prev(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            async fn eject(&mut self) -> crate::SourceOutcome {
                crate::SourceOutcome::new(SourceAction::Noop)
            }
            // `list_presets` n'est PAS surchargé : c'est tout l'objet du test.
        }

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = crate::bind_source(&socket).unwrap();
        tokio::spawn(async move {
            crate::serve_source(listener, SansNoms).await.unwrap();
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "cd".into(), update_tx).await.unwrap();
        // La corrélation se dénoue malgré l'absence de liste : le `Noop` est là.
        assert_eq!(client.request(SourceReq::ListPresets).await.unwrap(), SourceAction::Noop);
        client.request(SourceReq::Activate).await.unwrap();

        let (nom, premiere) = update_rx.recv().await.unwrap();
        assert_eq!(nom, "cd");
        assert!(
            premiere.identity.is_some(),
            "la premiere mise a jour doit etre celle de l'activate: la reponse a \
             ListPresets ne doit rien relayer, obtenu {premiere:?}"
        );
        assert_eq!(premiere.presets, None);
        // Et il n'y en a pas eu d'autre : une seule trame a valu la peine.
        assert!(update_rx.try_recv().is_err(), "aucune autre mise a jour ne doit etre relayee");
    }

    #[tokio::test]
    async fn trame_seule_avec_la_pochette_est_relayee() {
        // C'est exactement la forme sous laquelle une pochette arrive en
        // vrai (voir la doc de `SourceMessage::cover`, Task 2) : une
        // notification spontanée, plus tard que la réponse au `Play`, sans
        // rien d'autre. Sans l'entrée ajoutée au prédicat, elle serait jetée
        // en silence avant même d'atteindre `SourceUpdate`.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: Some(CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }),
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "files".into(), update_tx).await.unwrap();
        client.request(SourceReq::Activate).await.unwrap();
        let (name, update) = update_rx.recv().await.unwrap();
        assert_eq!(name, "files");
        assert_eq!(update.cover, Some(CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }));
    }

    #[tokio::test]
    async fn source_client_ne_relaie_rien_quand_la_trame_ne_porte_ni_vue_ni_identite() {
        // Une réponse à SetLocale, par exemple : inutile de réveiller la boucle
        // du cœur pour une trame qui ne dit rien de l'affichage.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = ritornello_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Noop),
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        client.request(SourceReq::SetLocale("fr".into())).await.unwrap();
        assert!(update_rx.try_recv().is_err(), "aucune mise a jour ne doit etre relayee");
    }

    #[tokio::test]
    async fn metadata_client_descend_letat_courant_puis_remonte_les_enrichissements() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // Le plugin reçoit l'état courant sans avoir rien demandé, puis
            // répond en écho de l'identité reçue.
            let line = lines.next_line().await.unwrap().unwrap();
            let np: NowPlaying = serde_json::from_str(&line).unwrap();
            let e = Enrichment {
                identity: np.identity.clone().unwrap(),
                // Espaces volontaires : c'est le relais qui normalise.
                artist: Some("  Mandrillus Sphynx ".into()),
                title: Some("Bikwix".into()),
                ..Default::default()
            };
            write.write_all(format!("{}\n", serde_json::to_string(&e).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (np_tx, np_rx) = tokio::sync::watch::channel(NowPlaying {
            source: "radio".into(),
            identity: Some(serde_json::json!({"kind": "stream", "url": "http://soma"})),
            ..Default::default()
        });
        let (enrich_tx, mut enrich_rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = run_metadata_client(&socket, "ouifm".into(), enrich_tx, np_rx).await;
        });

        let (nom, e) = enrich_rx.recv().await.unwrap();
        assert_eq!(nom, "ouifm");
        assert_eq!(e.artist.as_deref(), Some("Mandrillus Sphynx"), "les blancs doivent etre elagues");
        assert_eq!(e.title.as_deref(), Some("Bikwix"));
        assert_eq!(e.identity, serde_json::json!({"kind": "stream", "url": "http://soma"}));
        drop(np_tx);
    }

    #[tokio::test]
    async fn metadata_client_transmet_les_changements_de_ce_qui_joue() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("meta.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let vues = Arc::new(Mutex::new(Vec::<NowPlaying>::new()));
        let vues_srv = vues.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                vues_srv.lock().await.push(serde_json::from_str(&line).unwrap());
            }
        });

        let (np_tx, np_rx) = tokio::sync::watch::channel(NowPlaying {
            source: "radio".into(),
            identity: Some(serde_json::json!({"url": "un"})),
            ..Default::default()
        });
        let (enrich_tx, _enrich_rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = run_metadata_client(&socket, "ouifm".into(), enrich_tx, np_rx).await;
        });

        // Chaque envoi est attendu avant le suivant. Ce n'est pas de la
        // précaution de test : `watch` ne garantit **que** la dernière valeur,
        // et deux `send` consécutifs peuvent légitimement n'en produire qu'un
        // seul sur le fil. C'est la propriété qu'on veut (un plugin lent ne
        // retarde pas le cœur et ne rattrape pas un historique sans intérêt),
        // donc le test séquence au lieu de compter des trames.
        async fn attendre(vues: &Arc<Mutex<Vec<NowPlaying>>>, combien: usize) -> Vec<NowPlaying> {
            for _ in 0..100 {
                if vues.lock().await.len() >= combien {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            vues.lock().await.clone()
        }

        // L'état courant descend dès la connexion, sans changement préalable.
        let recues = attendre(&vues, 1).await;
        assert_eq!(recues.first().and_then(|np| np.identity.clone()), Some(serde_json::json!({"url": "un"})));

        np_tx
            .send(NowPlaying {
                source: "radio".into(),
                identity: Some(serde_json::json!({"url": "deux"})),
                ..Default::default()
            })
            .unwrap();
        let recues = attendre(&vues, 2).await;
        assert_eq!(recues.get(1).and_then(|np| np.identity.clone()), Some(serde_json::json!({"url": "deux"})));

        // L'arrêt descend aussi : c'est le signal qui fait cesser le travail du
        // plugin (couper une connexion HTTP ouverte, oublier son cache).
        np_tx.send(NowPlaying { source: "radio".into(), identity: None, ..Default::default() }).unwrap();
        let recues = attendre(&vues, 3).await;
        assert_eq!(recues.len(), 3, "{recues:?}");
        assert_eq!(recues[2].identity, None);
    }

    #[tokio::test]
    async fn display_client_envoie_letat_en_ligne() {
        // Les assertions de contenu vivent dans la tâche serveur : son
        // `JoinHandle` doit être **joint**, sans quoi une panique y serait
        // avalée et le test ne prouverait que « send() rend Ok » — il passait
        // avec un client écrivant du JSON faux ou la mauvaise ligne.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let serveur = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            // Le cœur écrit désormais une enveloppe : le serveur doit lire une
            // `DisplayFrame`, et la variante compte autant que le contenu — un
            // état poussé en catalogue passerait inaperçu de l'afficheur.
            match serde_json::from_str::<DisplayFrame>(&line).unwrap() {
                DisplayFrame::State(e) => assert_eq!(e.preset_name.as_deref(), Some("FIP")),
                autre => panic!("une trame d'etat etait attendue, obtenu {autre:?}"),
            }
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        let etat = PlayerState { source: "radio".into(), preset: Some(1), preset_name: Some("FIP".into()), ..Default::default() };
        client.send(&etat).await.unwrap();
        serveur.await.expect("les assertions du serveur ont paniqué");
    }

    #[tokio::test]
    async fn display_client_envoie_le_catalogue_sur_le_meme_socket_apres_un_etat() {
        // Deux trames de genres différents à la file sur la **même** connexion :
        // c'est ce que fait le relais du cœur au câblage d'un afficheur. Les
        // assertions vivent dans la tâche serveur, dont le `JoinHandle` est
        // joint — sans quoi une panique y serait avalée et le test ne prouverait
        // que « send_catalogue rend Ok ».
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let attendu = Catalogue {
            sources: vec![ritornello_proto::SourceCatalogue {
                name: "radio".into(),
                presets: vec![Preset { index: 5, name: "FIP".into() }],
            }],
        };
        let attendu_srv = attendu.clone();
        let serveur = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let premiere = lines.next_line().await.unwrap().unwrap();
            match serde_json::from_str::<DisplayFrame>(&premiere).unwrap() {
                DisplayFrame::State(e) => assert_eq!(e.source, "radio"),
                autre => panic!("la premiere trame doit etre un etat, obtenu {autre:?}"),
            }
            let seconde = lines.next_line().await.unwrap().unwrap();
            match serde_json::from_str::<DisplayFrame>(&seconde).unwrap() {
                DisplayFrame::Catalogue(c) => assert_eq!(c, attendu_srv),
                autre => panic!("la seconde trame doit etre un catalogue, obtenu {autre:?}"),
            }
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        client.send(&PlayerState { source: "radio".into(), ..Default::default() }).await.unwrap();
        client.send_catalogue(&attendu).await.unwrap();
        serveur.await.expect("les assertions du serveur ont paniqué");
    }

    #[tokio::test]
    async fn admin_client_correle_les_reponses() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // 1re requête (get_asset, id=1)
            let _ = lines.next_line().await.unwrap().unwrap();
            write
                .write_all(
                    b"{\"id\":1,\"result\":{\"kind\":\"Asset\",\"data\":{\"mime\":\"text/javascript\",\"body\":\"export default 1\"}}}\n",
                )
                .await
                .unwrap();
            // 2e requête (get_catalog, id=2)
            let _ = lines.next_line().await.unwrap().unwrap();
            write
                .write_all(b"{\"id\":2,\"result\":{\"kind\":\"Catalog\",\"data\":{\"btn_save\":\"Enregistrer\"}}}\n")
                .await
                .unwrap();
            // 3e requête (set_data, id=3)
            let _ = lines.next_line().await.unwrap().unwrap();
            write
                .write_all(b"{\"id\":3,\"result\":{\"kind\":\"Set\",\"data\":{\"ok\":false,\"error\":\"nope\"}}}\n")
                .await
                .unwrap();
            let _ = &write; // garde l'écriture vivante
            std::future::pending::<()>().await;
        });

        let client = AdminClient::connect(&socket).await.unwrap();
        assert_eq!(
            client.get_asset("ui.js").await.unwrap(),
            Some(("text/javascript".to_string(), "export default 1".to_string()))
        );
        assert_eq!(client.get_catalog().await.unwrap(), serde_json::json!({"btn_save": "Enregistrer"}));
        let verdict = client.set_data(serde_json::json!({})).await.unwrap();
        assert_eq!(verdict, Err("nope".to_string()));
    }

    #[tokio::test]
    async fn input_client_relaie_les_lignes_avec_et_sans_held() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let socket_for_client = socket.clone();
        tokio::spawn(async move {
            let _ = run_input_client(&socket_for_client, tx).await;
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        // A plain line from a pre-envelope plugin, then a held line.
        stream.write_all(b"{\"cmd\":\"VolumeUp\"}\n{\"cmd\":\"VolumeDown\",\"held\":true}\n").await.unwrap();
        let premier = rx.recv().await.unwrap();
        assert_eq!(premier, ritornello_proto::InputMessage::from(ritornello_proto::Command::VolumeUp));
        let second = rx.recv().await.unwrap();
        assert_eq!(second.cmd, ritornello_proto::Command::VolumeDown);
        assert!(second.held);
    }

    #[tokio::test]
    async fn requete_en_vol_echoue_vite_a_la_deconnexion() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Lit la requête puis ferme la connexion sans répondre.
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let _ = lines.next_line().await;
            // Fin du bloc : read et _write droppés -> EOF côté client.
        });
        let (update_tx, _update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        let res = client.request(SourceReq::Activate).await;
        let e = res.expect_err("une requête sans réponse doit échouer").to_string();
        // Le **message** distingue les deux chemins, là où une mesure de durée
        // ne le faisait que par une marge : « response dropped » vient du
        // pending drainé à l'EOF, « request timeout » de l'expiration des 5 s.
        // Asserter le message prouve donc exactement ce que ce test veut dire —
        // que l'échec est immédiat et non attendu — sans dépendre de la charge
        // de la machine, qui pouvait franchir la marge de 2 s contre 5 s.
        assert!(
            e.contains("response dropped"),
            "la requête doit échouer par le pending drainé, pas par le timeout de 5 s : {e}"
        );
        assert!(
            !e.contains("timeout"),
            "un échec par expiration signifierait que le pending n'a pas été drainé : {e}"
        );
    }
}
