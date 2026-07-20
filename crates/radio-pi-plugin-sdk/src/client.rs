use anyhow::{bail, Context, Result};
use radio_pi_proto::{Command, SourceAction, SourceMessage, SourceReq, SourceRequest, View};
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

pub struct SourceClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<SourceAction>>>>,
    next_id: AtomicU64,
}

impl SourceClient {
    pub async fn connect(
        socket_path: &Path,
        name: String,
        view_tx: mpsc::Sender<(String, View)>,
    ) -> Result<Arc<Self>> {
        let stream = connect_with_retry(socket_path).await?;
        let (read, write) = stream.into_split();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self { writer: Mutex::new(write), pending: pending.clone(), next_id: AtomicU64::new(1) });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<SourceMessage>(&line) else { continue };
                if let (Some(id), Some(action)) = (msg.id, msg.action.clone()) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(action);
                    }
                }
                if let Some(view) = msg.view {
                    if view_tx.try_send((name.clone(), view)).is_err() {
                        tracing::warn!("vue de {name} perdue (canal plein)");
                    }
                }
            }
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
    use radio_pi_proto::{SourceAction, View};
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
            let req: radio_pi_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = radio_pi_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Play { uri: "http://fip".into() }),
                view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            let _ = socket_for_server; // garde le chemin vivant pour le débogage
        });

        let (view_tx, mut view_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), view_tx).await.unwrap();
        let action = client.request(radio_pi_proto::SourceReq::Activate).await.unwrap();
        assert_eq!(action, SourceAction::Play { uri: "http://fip".into() });
        let (name, view) = view_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(view.line2, "FIP");
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
}
