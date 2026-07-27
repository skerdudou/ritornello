use anyhow::{bail, Context, Result};
use ritornello_proto::{
    AdminReq, AdminRequest, AdminResponse, AdminResult, Command, Enrichment, IdentityUpdate,
    NowPlaying, SourceAction, SourceMessage, SourceReq, SourceRequest, View,
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
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket_path).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    stream.with_context(|| format!("connexion a {} (10 s)", socket_path.display()))
}

/// Ce qu'une Source rapporte spontanément ou en marge d'une réponse : une vue
/// à afficher, une correction de l'identité de ce qui joue, ou les deux.
///
/// Les deux voyagent ensemble parce qu'ils sont produits ensemble par le
/// plugin, dans une seule trame : les séparer en deux canaux ferait exister des
/// instants où la vue affichée et l'identité annoncée aux plugins `metadata` se
/// contredisent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceUpdate {
    pub view: Option<View>,
    pub identity: Option<IdentityUpdate>,
    /// Voir `SourceMessage::line2_replaceable`. N'a de sens qu'accompagné d'une
    /// `view`.
    pub line2_replaceable: bool,
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
        view_tx: mpsc::Sender<(String, SourceUpdate)>,
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
                        tracing::warn!("message source invalide ignore: {e}");
                        continue;
                    }
                };
                if let (Some(id), Some(action)) = (msg.id, msg.action.clone()) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(action);
                    }
                }
                if msg.view.is_some() || msg.identity.is_some() {
                    let porte_identite = msg.identity.is_some();
                    let update = SourceUpdate {
                        view: msg.view,
                        identity: msg.identity,
                        line2_replaceable: msg.line2_replaceable,
                    };
                    if view_tx.try_send((name.clone(), update)).is_err() {
                        // Conséquence aggravée depuis que la trame porte aussi
                        // l'identité : une vue perdue était réparée par la
                        // suivante, une **identité** perdue ne l'est jamais — la
                        // Source ne la réémet que sur changement, donc le cœur
                        // garde celle du morceau précédent et les plugins
                        // `metadata` continuent de l'enrichir, sans que le
                        // garde-fou de péremption y voie quoi que ce soit.
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
                                "identite de {name} perdue (canal plein) : affichage et metadonnees possiblement perimes jusqu'au prochain changement"
                            );
                        } else {
                            tracing::warn!("vue de {name} perdue (canal plein)");
                        }
                    }
                }
            }
            // Déconnexion : drainer les requêtes en vol. Dropper chaque Sender
            // fait résoudre le rx.await de request() en Err immédiatement.
            pending.lock().await.clear();
            tracing::warn!("connexion au plugin source fermee");
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
            Ok(Err(_)) => bail!("plugin source: reponse abandonnee"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("plugin source: timeout de requete")
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

    pub async fn send(&self, view: &View) -> Result<()> {
        let mut w = self.writer.lock().await;
        w.write_all(format!("{}\n", serde_json::to_string(view)?).as_bytes()).await?;
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
                        tracing::warn!("reponse admin invalide ignoree: {e}");
                        continue;
                    }
                };
                if let Some(tx) = pending.lock().await.remove(&resp.id) {
                    let _ = tx.send(resp.result);
                }
            }
            // Déconnexion : drainer les requêtes en vol (voir SourceClient).
            pending.lock().await.clear();
            tracing::warn!("connexion au plugin admin fermee");
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
            Ok(Err(_)) => bail!("plugin admin: reponse abandonnee"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("plugin admin: timeout de requete")
            }
        }
    }

    pub async fn get_asset(&self, path: &str) -> Result<Option<(String, String)>> {
        match self.request(AdminReq::GetAsset(path.to_string())).await? {
            AdminResult::Asset { mime, body } => Ok(body.map(|b| (mime, b))),
            autre => anyhow::bail!("reponse inattendue a GetAsset: {autre:?}"),
        }
    }

    pub async fn get_catalog(&self) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetCatalog).await? {
            AdminResult::Catalog(v) => Ok(v),
            autre => anyhow::bail!("reponse inattendue a GetCatalog: {autre:?}"),
        }
    }

    pub async fn get_data(&self) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetData).await? {
            AdminResult::Data(v) => Ok(v),
            other => bail!("reponse admin inattendue pour GetData: {other:?}"),
        }
    }

    pub async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        match self.request(AdminReq::SetData(data)).await? {
            AdminResult::Set { ok: true, .. } => Ok(Ok(())),
            AdminResult::Set { ok: false, error } => Ok(Err(error.unwrap_or_default())),
            other => bail!("reponse admin inattendue pour SetData: {other:?}"),
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
                    bail!("connexion au plugin metadata {name} fermee");
                };
                match serde_json::from_str::<Enrichment>(&line) {
                    // `cleaned` ici, au plus près de l'entrée : le cœur n'a
                    // ensuite qu'une seule forme à traiter (voir `is_empty`,
                    // qui décide de l'arbitrage).
                    Ok(e) => {
                        if enrich_tx.send((name.clone(), e.cleaned())).await.is_err() {
                            bail!("coeur ferme, arret du relais metadata {name}");
                        }
                    }
                    Err(e) => tracing::warn!("enrichissement invalide de {name} ignore: {e}"),
                }
            }
            change = np_rx.changed() => {
                if change.is_err() {
                    bail!("canal now-playing ferme, arret du relais metadata {name}");
                }
                a_envoyer = Some(np_rx.borrow_and_update().clone());
            }
        }
    }
}

/// Se connecte au plugin input et relaie chaque `Command` reçue sur `cmd_tx`,
/// jusqu'à fermeture de la connexion (ne revient qu'en cas d'erreur ; à
/// spawn dans une tâche dédiée par l'appelant).
pub async fn run_input_client(socket_path: &Path, cmd_tx: mpsc::Sender<Command>) -> Result<()> {
    let stream = connect_with_retry(socket_path).await?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        match serde_json::from_str::<Command>(&line) {
            Ok(cmd) => {
                let _ = cmd_tx.send(cmd).await;
            }
            Err(e) => tracing::warn!("commande invalide recue du plugin input: {e}"),
        }
    }
    bail!("connexion au plugin input fermee")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::{SourceAction, View};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn source_client_correle_par_id_et_relaie_la_vue() {
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
                view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
                identity: Some(ritornello_proto::IdentityUpdate::Playing(
                    serde_json::json!({"kind": "stream", "url": "http://fip"}),
                )),
                line2_replaceable: false,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            let _ = socket_for_server; // garde le chemin vivant pour le débogage
        });

        let (view_tx, mut view_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), view_tx).await.unwrap();
        let action = client.request(ritornello_proto::SourceReq::Activate).await.unwrap();
        assert_eq!(action, SourceAction::Play { uri: "http://fip".into() });
        let (name, update) = view_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(update.view.unwrap().line2, "FIP");
        // La vue et l'identité arrivent dans la même mise à jour : c'est ce qui
        // garantit qu'on n'affiche jamais une station en annonçant l'autre.
        assert_eq!(
            update.identity,
            Some(ritornello_proto::IdentityUpdate::Playing(
                serde_json::json!({"kind": "stream", "url": "http://fip"})
            ))
        );
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
                view: None,
                identity: None,
                line2_replaceable: false,
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (view_tx, mut view_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), view_tx).await.unwrap();
        client.request(SourceReq::SetLocale("fr".into())).await.unwrap();
        assert!(view_rx.try_recv().is_err(), "aucune mise a jour ne doit etre relayee");
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
    async fn display_client_envoie_la_vue_en_ligne() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let v: View = serde_json::from_str(&line).unwrap();
            assert_eq!(v.line2, "FIP");
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        client.send(&View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
        let (view_tx, _view_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), view_tx).await.unwrap();
        let start = std::time::Instant::now();
        let res = client.request(SourceReq::Activate).await;
        assert!(res.is_err());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "la requête doit échouer AVANT le timeout de 5 s (pending drainé)"
        );
    }
}
