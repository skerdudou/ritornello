use anyhow::{Context, Result};
use radio_pi_proto::{SourceAction, SourceReq, SourceRequest, SourceMessage, View};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

pub struct SourceOutcome {
    pub action: SourceAction,
    pub view: Option<View>,
}

#[async_trait::async_trait]
pub trait SourcePlugin: Send + 'static {
    async fn activate(&mut self) -> SourceOutcome;
    async fn deactivate(&mut self) -> SourceOutcome;
    async fn select(&mut self, n: u8) -> SourceOutcome;
    async fn next(&mut self) -> SourceOutcome;
    async fn prev(&mut self) -> SourceOutcome;
    async fn next_track(&mut self) -> SourceOutcome;
    async fn prev_track(&mut self) -> SourceOutcome;
    async fn eject(&mut self) -> SourceOutcome;

    /// Notification spontanée (ex. changement de piste, métadonnées arrivées en
    /// différé). Par défaut ne se termine jamais : un plugin sans notification
    /// spontanée (Radio) n'a rien à écrire de plus.
    async fn poll_notification(&mut self) -> Option<View> {
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

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let req: SourceRequest = serde_json::from_str(&line)
                    .with_context(|| format!("requete invalide: {line}"))?;
                let outcome = match req.req {
                    SourceReq::Activate => plugin.activate().await,
                    SourceReq::Deactivate => plugin.deactivate().await,
                    SourceReq::Select(n) => plugin.select(n).await,
                    SourceReq::Next => plugin.next().await,
                    SourceReq::Prev => plugin.prev().await,
                    SourceReq::NextTrack => plugin.next_track().await,
                    SourceReq::PrevTrack => plugin.prev_track().await,
                    SourceReq::Eject => plugin.eject().await,
                };
                let msg = SourceMessage { id: Some(req.id), action: Some(outcome.action), view: outcome.view };
                write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
            }
            view = plugin.poll_notification() => {
                if let Some(view) = view {
                    let msg = SourceMessage { id: None, action: None, view: Some(view) };
                    write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
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
        let view: View = serde_json::from_str(&line)
            .with_context(|| format!("vue invalide: {line}"))?;
        plugin.show(view).await?;
    }
    Ok(())
}

use radio_pi_proto::Command;

#[async_trait::async_trait]
pub trait InputPlugin: Send + 'static {
    async fn next_command(&mut self) -> Result<Command>;
}

pub async fn run_input_plugin(mut plugin: impl InputPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    let (stream, _) = listener.accept().await?;
    let (_read, mut write) = stream.into_split();
    loop {
        let cmd = plugin.next_command().await?;
        write.write_all(format!("{}\n", serde_json::to_string(&cmd)?).as_bytes()).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use radio_pi_proto::{SourceAction, View};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    struct EchoSource;

    #[async_trait::async_trait]
    impl SourcePlugin for EchoSource {
        async fn activate(&mut self) -> SourceOutcome {
            SourceOutcome {
                action: SourceAction::Play { uri: "http://fip".into() },
                view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
            }
        }
        async fn deactivate(&mut self) -> SourceOutcome {
            SourceOutcome { action: SourceAction::Stop, view: None }
        }
        async fn select(&mut self, n: u8) -> SourceOutcome {
            SourceOutcome { action: SourceAction::Play { uri: format!("http://station-{n}") }, view: None }
        }
        async fn next(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
        async fn prev(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
        async fn next_track(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::PlayerNext, view: None } }
        async fn prev_track(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::PlayerPrev, view: None } }
        async fn eject(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
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
        let msg: radio_pi_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://fip".into() }));
        assert_eq!(msg.view.unwrap().line2, "FIP");

        write.write_all(b"{\"id\":2,\"req\":\"Select\",\"arg\":3}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: radio_pi_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(2));
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://station-3".into() }));
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use radio_pi_proto::View;
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
mod input_tests {
    use super::*;
    use radio_pi_proto::Command;

    struct FixedCommands {
        remaining: Vec<Command>,
    }

    #[async_trait::async_trait]
    impl InputPlugin for FixedCommands {
        async fn next_command(&mut self) -> anyhow::Result<Command> {
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
        let plugin = FixedCommands { remaining: vec![Command::Select(3), Command::Stop] };
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
        assert_eq!(serde_json::from_str::<Command>(&l1).unwrap(), Command::Select(3));
        let l2 = lines.next_line().await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Command>(&l2).unwrap(), Command::Stop);
    }
}
