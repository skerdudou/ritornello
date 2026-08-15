use anyhow::{bail, Context, Result};
use ritornello_proto::{
    AdminReq, AdminRequest, AdminResponse, AdminResult, Enrichment, IdentityUpdate, InputMessage,
    NowPlaying, PlayerState, SourceAction, SourceMessage, SourceReq, SourceRequest,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

async fn connect_with_retry(socket_path: &Path) -> Result<UnixStream> {
    // La dernière erreur est conservée pour le rapport final : « connecting to
    // <socket> (10s) » seul cache la cause, et une erreur permanente (droits
    // refusés sur la socket) était retentée 100 fois puis rapportée comme un
    // simple délai dépassé — diagnostic inutilement difficile au démarrage.
    let mut derniere = None;
    for _ in 0..100 {
        match UnixStream::connect(socket_path).await {
            Ok(s) => return Ok(s),
            Err(e) => derniere = Some(e),
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!(derniere.expect("at least one attempt")))
        .with_context(|| format!("connecting to {} (10s)", socket_path.display()))
}

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
        let stream = connect_with_retry(socket_path).await?;
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
                {
                    let porte_identite = msg.identity.is_some();
                    let update = SourceUpdate {
                        identity: msg.identity,
                        transient: msg.transient,
                        preset: msg.preset,
                        preset_count: msg.preset_count,
                        preset_name: msg.preset_name,
                        status: msg.status,
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
        let stream = connect_with_retry(socket_path).await?;
        let (_read, write) = stream.into_split();
        Ok(Arc::new(Self { writer: Mutex::new(write) }))
    }

    pub async fn send(&self, state: &PlayerState) -> Result<()> {
        let mut w = self.writer.lock().await;
        w.write_all(format!("{}\n", serde_json::to_string(state)?).as_bytes()).await?;
        Ok(())
    }
}

pub struct AdminClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AdminResult>>>>,
    next_id: AtomicU64,
}

impl AdminClient {
    pub async fn connect(socket_path: &Path) -> Result<Arc<Self>> {
        let stream = connect_with_retry(socket_path).await?;
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
            Ok(Err(_)) => bail!("admin plugin: response dropped"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("admin plugin: request timeout")
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
    let stream = connect_with_retry(socket_path).await?;
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
    let stream = connect_with_retry(socket_path).await?;
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
                action: Some(SourceAction::Play { uri: "http://fip".into() }),
                identity: Some(ritornello_proto::IdentityUpdate::Playing(
                    serde_json::json!({"kind": "stream", "url": "http://fip"}),
                )),
                transient: false,
                preset: Some(1),
                preset_count: None,
                preset_name: Some("FIP".into()),
                status: None,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            let _ = socket_for_server; // garde le chemin vivant pour le débogage
        });

        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), update_tx).await.unwrap();
        let action = client.request(ritornello_proto::SourceReq::Activate).await.unwrap();
        assert_eq!(action, SourceAction::Play { uri: "http://fip".into() });
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

        np_tx.send(NowPlaying { source: "radio".into(), identity: Some(serde_json::json!({"url": "deux"})) }).unwrap();
        let recues = attendre(&vues, 2).await;
        assert_eq!(recues.get(1).and_then(|np| np.identity.clone()), Some(serde_json::json!({"url": "deux"})));

        // L'arrêt descend aussi : c'est le signal qui fait cesser le travail du
        // plugin (couper une connexion HTTP ouverte, oublier son cache).
        np_tx.send(NowPlaying { source: "radio".into(), identity: None }).unwrap();
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
            let e: PlayerState = serde_json::from_str(&line).unwrap();
            assert_eq!(e.preset_name.as_deref(), Some("FIP"));
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        let etat = PlayerState { source: "radio".into(), preset: Some(1), preset_name: Some("FIP".into()), ..Default::default() };
        client.send(&etat).await.unwrap();
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
        let start = std::time::Instant::now();
        let res = client.request(SourceReq::Activate).await;
        assert!(res.is_err());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "la requête doit échouer AVANT le timeout de 5 s (pending drainé)"
        );
    }
}
