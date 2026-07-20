use crate::types::Event;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{broadcast, oneshot, Mutex};

pub struct MpvIpc {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>,
    next_id: AtomicU64,
}

impl MpvIpc {
    pub fn from_stream(stream: UnixStream, events: broadcast::Sender<Event>) -> Arc<Self> {
        let (read, write) = stream.into_split();
        let ipc = Arc::new(Self {
            writer: Mutex::new(write),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
        });
        let pending = ipc.pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(id) = v.get("request_id").and_then(Value::as_u64) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let res = if v["error"] == json!("success") {
                            Ok(v.get("data").cloned().unwrap_or(Value::Null))
                        } else {
                            Err(anyhow::anyhow!("mpv: {}", v["error"]))
                        };
                        let _ = tx.send(res);
                    }
                } else if v["event"] == json!("property-change") {
                    let ev = match (v["name"].as_str(), &v["data"]) {
                        (Some("media-title"), Value::String(t)) => Some(Event::Title(t.clone())),
                        (Some("idle-active"), Value::Bool(true)) => Some(Event::PlaybackIdle),
                        (Some("idle-active"), Value::Bool(false)) => Some(Event::PlaybackActive),
                        (Some("playlist-pos"), Value::Number(n)) => {
                            n.as_i64().map(Event::TrackChanged)
                        }
                        _ => None,
                    };
                    if let Some(ev) = ev {
                        let _ = events.send(ev);
                    }
                }
            }
            tracing::warn!("socket mpv fermée");
        });
        ipc
    }

    pub async fn command(&self, args: &[Value]) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({ "command": args, "request_id": id });
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{msg}\n").as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => bail!("mpv: réponse abandonnée"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("mpv: timeout de commande")
            }
        }
    }

    pub async fn observe(&self, name: &str) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.command(&[json!("observe_property"), json!(id), json!(name)]).await?;
        Ok(())
    }
}

pub struct MpvPlayer {
    ipc: Arc<MpvIpc>,
}

/// Lance mpv en démon idle et s'y connecte. Le Child est rendu à l'appelant :
/// s'il meurt, main quitte et systemd relance tout le service.
pub async fn start(
    mpv_bin: &str,
    socket: &Path,
    cd_dev: &str,
    events: broadcast::Sender<Event>,
) -> Result<(MpvPlayer, tokio::process::Child)> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket);
    let child = tokio::process::Command::new(mpv_bin)
        .arg("--idle=yes")
        .arg("--no-video")
        .arg("--no-terminal")
        .arg(format!("--input-ipc-server={}", socket.display()))
        .arg(format!("--cdda-device={cd_dev}"))
        .kill_on_drop(true)
        .spawn()
        .context("lancement de mpv")?;

    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stream = stream.context("connexion à la socket mpv (10 s)")?;
    let ipc = MpvIpc::from_stream(stream, events);
    ipc.observe("media-title").await?;
    ipc.observe("idle-active").await?;
    ipc.observe("playlist-pos").await?;
    Ok((MpvPlayer { ipc }, child))
}

#[async_trait::async_trait]
impl super::Player for MpvPlayer {
    async fn play(&self, uri: &str) -> Result<()> {
        self.ipc.command(&[json!("loadfile"), json!(uri), json!("replace")]).await?;
        self.ipc.command(&[json!("set_property"), json!("pause"), json!(false)]).await?;
        Ok(())
    }
    async fn stop(&self) -> Result<()> {
        self.ipc.command(&[json!("stop")]).await?;
        Ok(())
    }
    async fn toggle_pause(&self) -> Result<()> {
        self.ipc.command(&[json!("cycle"), json!("pause")]).await?;
        Ok(())
    }
    async fn next(&self) -> Result<()> {
        self.ipc.command(&[json!("playlist-next")]).await?;
        Ok(())
    }
    async fn prev(&self) -> Result<()> {
        self.ipc.command(&[json!("playlist-prev")]).await?;
        Ok(())
    }
    async fn set_volume(&self, volume: u8) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("volume"), json!(volume)]).await?;
        Ok(())
    }
    async fn set_mute(&self, mute: bool) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("mute"), json!(mute)]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Event;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn command_recoit_la_reponse_correspondante() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = broadcast::channel(16);
        let ipc = MpvIpc::from_stream(client, tx);

        tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = req["request_id"].as_u64().unwrap();
            let resp = format!("{{\"error\":\"success\",\"data\":42,\"request_id\":{id}}}\n");
            w.write_all(resp.as_bytes()).await.unwrap();
        });

        let v = ipc.command(&[serde_json::json!("get_property"), serde_json::json!("volume")])
            .await
            .unwrap();
        assert_eq!(v, serde_json::json!(42));
    }

    #[tokio::test]
    async fn property_change_devient_event() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);

        let (_r, mut w) = server.into_split();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"media-title\",\"data\":\"FIP - Miles Davis\"}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":true}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":false}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"playlist-pos\",\"data\":3}\n")
            .await
            .unwrap();

        assert_eq!(rx.recv().await.unwrap(), Event::Title("FIP - Miles Davis".into()));
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackIdle);
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        assert_eq!(rx.recv().await.unwrap(), Event::TrackChanged(3));
    }

    #[tokio::test]
    async fn erreur_mpv_remonte_en_err() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = broadcast::channel(16);
        let ipc = MpvIpc::from_stream(client, tx);
        tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = req["request_id"].as_u64().unwrap();
            let resp = format!("{{\"error\":\"invalid parameter\",\"request_id\":{id}}}\n");
            w.write_all(resp.as_bytes()).await.unwrap();
        });
        assert!(ipc.command(&[serde_json::json!("loadfile")]).await.is_err());
    }
}
