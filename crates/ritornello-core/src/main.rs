mod audio_output;
mod core;
mod player;
mod plugins;
mod state;
mod status;
mod types;

use crate::plugins::{PluginKind, PluginManifest};
use crate::status::{AppState, LogBuffer, LogBufferWriter, PluginStatus, StatusState};
use crate::types::Event;
use anyhow::{Context, Result};
use ritornello_proto::{Command, View};
use ritornello_plugin_sdk::{run_input_client, DisplayClient, SourceClient};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[async_trait::async_trait]
impl core::Source for SourceClient {
    async fn request(&self, req: ritornello_proto::SourceReq) -> Result<ritornello_proto::SourceAction> {
        SourceClient::request(self, req).await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let log_buffer = Arc::new(LogBuffer::new(50));
    let log_buffer_for_writer = log_buffer.clone();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(move || LogBufferWriter(log_buffer_for_writer.clone()))
                .with_filter(LevelFilter::WARN),
        )
        .init();

    let plugins_path = PathBuf::from(env_or("RITORNELLO_PLUGINS", "/etc/ritornello/plugins.toml"));
    let state_path = PathBuf::from(env_or("RITORNELLO_STATE", "/var/lib/ritornello/state.json"));
    let mpv_socket = PathBuf::from(env_or("RITORNELLO_MPV_SOCKET", "/run/ritornello/mpv.sock"));
    let mpv_bin = env_or("RITORNELLO_MPV_BIN", "mpv");
    let cd_dev = env_or("RITORNELLO_CD_DEV", "/dev/sr0");
    let http_addr = env_or("RITORNELLO_HTTP", "0.0.0.0:8080");
    let runtime_dir = env_or("RITORNELLO_RUNTIME_DIR", "/run/ritornello");

    let manifest = PluginManifest::load(&plugins_path)
        .with_context(|| format!("chargement de {}", plugins_path.display()))?;
    let persisted = state::load(&state_path);

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
    let (ev_tx, mut ev_rx) = broadcast::channel::<Event>(64);
    let (view_tx, mut view_rx) = watch::channel(View::default());
    let (source_view_tx, mut source_view_rx) = mpsc::channel::<(String, View)>(32);
    let (audio_tx, mut audio_rx) = mpsc::channel::<String>(4);

    // mpv (inchangé).
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, ev_tx.clone())
            .await
            .context("démarrage de mpv")?;

    // Spawn et connexion de chaque plugin déclaré.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    let mut plugin_statuses = Vec::new();
    let mut children = Vec::new();
    let mut source_connects = Vec::new();
    let mut display_connect = None;

    for p in &manifest.plugins {
        let socket_path = PathBuf::from(format!("{runtime_dir}/{}.sock", p.name));
        match plugins::spawn(&p.exec, &socket_path) {
            Ok(child) => {
                children.push(child);
                match p.kind {
                    PluginKind::Source => {
                        let name = p.name.clone();
                        let admin_url = p.admin_url.clone();
                        let view_tx = source_view_tx.clone();
                        source_connects.push(tokio::spawn(async move {
                            let result = SourceClient::connect(&socket_path, name.clone(), view_tx).await;
                            (name, admin_url, result)
                        }));
                    }
                    PluginKind::Display => {
                        let name = p.name.clone();
                        let admin_url = p.admin_url.clone();
                        display_connect = Some(tokio::spawn(async move {
                            let result = DisplayClient::connect(&socket_path).await;
                            (name, admin_url, result)
                        }));
                    }
                    PluginKind::Input => {
                        let tx = cmd_tx.clone();
                        let socket_for_task = socket_path.clone();
                        let name = p.name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_input_client(&socket_for_task, tx).await {
                                tracing::warn!("plugin input {name} deconnecte: {e}");
                            }
                        });
                        plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "input".into(), connected: true, admin_url: p.admin_url.clone() });
                    }
                }
            }
            Err(e) => {
                tracing::warn!("lancement du plugin {} impossible: {e}", p.name);
                plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: format!("{:?}", p.kind).to_lowercase(), connected: false, admin_url: p.admin_url.clone() });
            }
        }
    }

    for handle in source_connects {
        let (name, admin_url, result) = handle.await.context("tache de connexion plugin source interrompue")?;
        match result {
            Ok(client) => {
                sources.insert(name.clone(), client);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: true, admin_url });
            }
            Err(e) => {
                tracing::warn!("plugin {} indisponible: {e}", name);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: false, admin_url });
            }
        }
    }

    let mut display_client: Option<Arc<DisplayClient>> = None;
    if let Some(handle) = display_connect {
        let (name, admin_url, result) = handle.await.context("tache de connexion plugin display interrompue")?;
        match result {
            Ok(client) => {
                display_client = Some(client);
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: true, admin_url });
            }
            Err(e) => {
                tracing::warn!("plugin display {name} indisponible: {e}");
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: false, admin_url });
            }
        }
    }

    if sources.is_empty() {
        anyhow::bail!("aucune source disponible (plugins.toml vide ou tous les plugins source indisponibles)");
    }

    // Relais des vues vers le plugin d'affichage, s'il est connecté.
    match display_client {
        Some(display_client) => {
            tokio::spawn(async move {
                loop {
                    if view_rx.changed().await.is_err() {
                        break;
                    }
                    let v = view_rx.borrow_and_update().clone();
                    if let Err(e) = display_client.send(&v).await {
                        tracing::warn!("affichage: {e}");
                    }
                }
            });
        }
        None => tracing::warn!("pas de plugin display connecte, on continue sans affichage"),
    }

    // Page de statut du cœur (plugins, source active, dernières erreurs, sortie audio).
    let status_state = Arc::new(RwLock::new(StatusState {
        plugins: plugin_statuses,
        active_source: persisted.active_source.clone(),
    }));
    let audio_current = Arc::new(RwLock::new(persisted.audio_device.clone()));
    {
        let app = status::router(AppState {
            status: status_state.clone(),
            logs: log_buffer.clone(),
            audio_current: audio_current.clone(),
            audio_tx: audio_tx.clone(),
        });
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("page de statut sur http://{http_addr}/status");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("serveur de statut: {e}");
            }
        });
    }

    // Cœur. La page de statut affiche la source active telle que persistée
    // au démarrage (`persisted.active_source`) ; elle n'est pas mise à jour
    // en direct si l'utilisateur change de source ensuite — hors périmètre
    // de cette livraison (aucun test ne l'exige).
    let mut core = core::Core::new(mpv_player, sources, persisted, state_path, view_tx);
    core.resume().await?;

    let mut retry_at: Option<tokio::time::Instant> = None;

    loop {
        let retry_sleep = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                if let Err(e) = core.handle_command(cmd).await {
                    tracing::warn!("commande: {e}");
                }
            }
            Ok(ev) = ev_rx.recv() => {
                if matches!(ev, Event::Title(_) | Event::PlaybackActive) {
                    retry_at = None;
                }
                if let Some(delay) = core.handle_event(ev).await {
                    retry_at = Some(tokio::time::Instant::now() + delay);
                }
            }
            Some((name, view)) = source_view_rx.recv() => {
                core.handle_source_view(&name, view);
            }
            Some(device) = audio_rx.recv() => {
                if let Err(e) = core.set_audio_device(device).await {
                    tracing::warn!("changement de sortie audio: {e}");
                }
            }
            _ = retry_sleep => {
                retry_at = None;
                if let Err(e) = core.retry_stream().await {
                    tracing::warn!("retry flux: {e}");
                }
            }
            status = mpv_child.wait() => {
                anyhow::bail!("mpv termine ({status:?}), arret pour relance par systemd");
            }
        }
    }
}
