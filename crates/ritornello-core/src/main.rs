mod audio_output;
mod admin;
mod core;
mod placeholder;
mod player;
mod plugins;
mod state;
mod status;
mod theme;
mod types;
mod web;

use crate::plugins::{PluginKind, PluginManifest};
use crate::status::{AppState, LogBuffer, LogBufferWriter, PluginStatus, StatusState};
use crate::types::Event;
use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
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

    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let catalog = Arc::new(RwLock::new(ritornello_i18n::Catalog::load(
        "core",
        persisted.locale.as_deref().unwrap_or("en"),
        &locales_root,
        core::EN,
    )));

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
    let (ev_tx, mut ev_rx) = broadcast::channel::<Event>(64);
    let (view_tx, mut view_rx) = watch::channel(View::default());
    let (source_view_tx, mut source_view_rx) = mpsc::channel::<(String, View)>(32);
    let (audio_tx, mut audio_rx) = mpsc::channel::<String>(4);
    let (locale_tx, mut locale_rx) = mpsc::channel::<String>(4);
    let (theme_tx, mut theme_rx) = mpsc::channel::<theme::ThemeState>(4);

    // mpv. Les deux durées de tampon sont réglables sans recompiler : la bonne
    // valeur dépend du réseau et de la charge de la machine, pas du code.
    let audio_buffer_brut = std::env::var("RITORNELLO_AUDIO_BUFFER").ok();
    let readahead_brut = std::env::var("RITORNELLO_NETWORK_READAHEAD").ok();
    let audio_buffer = player::mpv::audio_buffer_regle(audio_buffer_brut.as_deref());
    let readahead = player::mpv::readahead_regle(readahead_brut.as_deref());
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, audio_buffer, readahead, ev_tx.clone())
            .await
            .context("démarrage de mpv")?;

    // Spawn et connexion de chaque plugin déclaré.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    let mut plugin_statuses = Vec::new();
    let mut plugin_waits = FuturesUnordered::new();
    let mut source_connects = Vec::new();
    let mut display_connect = None;
    let mut admin_connects = Vec::new();

    for p in &manifest.plugins {
        let socket_path = PathBuf::from(format!("{runtime_dir}/{}.sock", p.name));
        let admin_socket = p
            .admin
            .then(|| PathBuf::from(format!("{runtime_dir}/{}-admin.sock", p.name)));
        match plugins::spawn(&p.exec, &socket_path, admin_socket.as_deref(), persisted.locale.as_deref()) {
            Ok(child) => {
                let wname = p.name.clone();
                plugin_waits.push(async move {
                    let mut child = child;
                    let status = child.wait().await;
                    (wname, status)
                });
                if p.admin {
                    let name = p.name.clone();
                    let asock = PathBuf::from(format!("{runtime_dir}/{}-admin.sock", p.name));
                    admin_connects.push(tokio::spawn(async move {
                        let result = ritornello_plugin_sdk::AdminClient::connect(&asock).await;
                        (name, result)
                    }));
                }
                match p.kind {
                    PluginKind::Source => {
                        let name = p.name.clone();
                        let admin = p.admin;
                        let view_tx = source_view_tx.clone();
                        source_connects.push(tokio::spawn(async move {
                            let result = SourceClient::connect(&socket_path, name.clone(), view_tx).await;
                            (name, admin, result)
                        }));
                    }
                    PluginKind::Display => {
                        let name = p.name.clone();
                        let admin = p.admin;
                        display_connect = Some(tokio::spawn(async move {
                            let result = DisplayClient::connect(&socket_path).await;
                            (name, admin, result)
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
                        plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "input".into(), connected: true, admin: p.admin });
                    }
                }
            }
            Err(e) => {
                tracing::warn!("lancement du plugin {} impossible: {e}", p.name);
                plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: format!("{:?}", p.kind).to_lowercase(), connected: false, admin: p.admin });
            }
        }
    }

    for handle in source_connects {
        let (name, admin, result) = handle.await.context("tache de connexion plugin source interrompue")?;
        match result {
            Ok(client) => {
                sources.insert(name.clone(), client);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: true, admin });
            }
            Err(e) => {
                tracing::warn!("plugin {} indisponible: {e}", name);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: false, admin });
            }
        }
    }

    let mut display_client: Option<Arc<DisplayClient>> = None;
    if let Some(handle) = display_connect {
        let (name, admin, result) = handle.await.context("tache de connexion plugin display interrompue")?;
        match result {
            Ok(client) => {
                display_client = Some(client);
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: true, admin });
            }
            Err(e) => {
                tracing::warn!("plugin display {name} indisponible: {e}");
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: false, admin });
            }
        }
    }

    let mut admin_backends: HashMap<String, Arc<dyn admin::AdminBackend>> = HashMap::new();
    for handle in admin_connects {
        let (name, result) = handle.await.context("tache de connexion admin interrompue")?;
        match result {
            Ok(client) => {
                admin_backends.insert(name, client);
            }
            Err(e) => tracing::warn!("plugin admin {name} injoignable: {e}"),
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
    let locale_current = Arc::new(RwLock::new(persisted.locale.clone()));
    // `state.json` est relu sans garantie : `theme_put` valide le chemin HTTP,
    // mais un fichier d'etat corrompu ou edite a la main peut porter n'importe
    // quoi. Un nom de theme inconnu fait sortir `applyTheme` cote SPA sans
    // poser une seule variable CSS, et `theme.css` n'a pas de valeur de repli :
    // l'IHM s'affiche entierement non themee. `from_persisted` valide et
    // retombe sur les defauts en journalisant un avertissement.
    let theme_current = Arc::new(RwLock::new(theme::from_persisted(
        persisted.theme.as_deref(),
        persisted.mode.as_deref(),
    )));
    {
        let app = status::router(AppState {
            status: status_state.clone(),
            logs: log_buffer.clone(),
            audio_current: audio_current.clone(),
            audio_tx: audio_tx.clone(),
            catalog: catalog.clone(),
            locale_current: locale_current.clone(),
            locale_tx: locale_tx.clone(),
            locales_root: locales_root.clone(),
            admin_backends: Arc::new(admin_backends),
            admin_assets: Arc::new(Default::default()),
            cmd_tx: cmd_tx.clone(),
            theme_current: theme_current.clone(),
            theme_tx: theme_tx.clone(),
        });
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("interface web sur http://{http_addr}/");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("serveur de statut: {e}");
            }
        });
    }

    // Cœur. La source active affichée est tenue à jour en direct par la boucle
    // ci-dessous (mise à jour de status_state.active_source après chaque commande).
    let mut core = core::Core::new(
        mpv_player,
        sources,
        persisted,
        state_path,
        view_tx,
        catalog.clone(),
        locales_root.clone(),
    );
    core.resume().await?;

    let mut retry_at: Option<tokio::time::Instant> = None;
    let mut events_open = true;

    loop {
        let retry_sleep = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // Echeance de l'overlay volume/muet, lue dans une variable locale
        // avant le `select!` (comme `retry_at`) pour ne pas garder d'emprunt
        // sur `core` pendant l'attente.
        let overlay_at = core.overlay_deadline().map(tokio::time::Instant::from);
        let overlay_sleep = async {
            match overlay_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                if let Err(e) = core.handle_command(cmd).await {
                    tracing::warn!("commande: {e}");
                }
                status_state.write().await.active_source = core.active_source().to_string();
            }
            ev = ev_rx.recv(), if events_open => {
                match ev {
                    Ok(ev) => {
                        if matches!(ev, Event::Title(_) | Event::PlaybackActive) {
                            retry_at = None;
                        }
                        if let Some(delay) = core.handle_event(ev).await {
                            retry_at = Some(tokio::time::Instant::now() + delay);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("events en retard, {n} perdus");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Canal fermé : on désactive le bras (comme aujourd'hui,
                        // le bras cessait de matcher) pour éviter de tourner à vide.
                        events_open = false;
                    }
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
            Some(locale) = locale_rx.recv() => {
                if let Err(e) = core.set_locale(locale).await {
                    tracing::warn!("changement de langue: {e}");
                }
            }
            Some(t) = theme_rx.recv() => {
                core.set_theme(t);
            }
            _ = retry_sleep => {
                retry_at = None;
                if let Err(e) = core.retry_stream().await {
                    tracing::warn!("retry flux: {e}");
                }
            }
            _ = overlay_sleep => {
                core.expire_overlay();
            }
            (name, status) = plugin_waits.select_next_some() => {
                tracing::warn!("plugin {name} termine: {status:?}");
                crate::status::mark_plugin_disconnected(&mut *status_state.write().await, &name);
            }
            status = mpv_child.wait() => {
                anyhow::bail!("mpv termine ({status:?}), arret pour relance par systemd");
            }
        }
    }
}
