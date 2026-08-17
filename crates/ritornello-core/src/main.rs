mod audio_output;
mod admin;
mod core;
mod metadata;
mod placeholder;
mod player;
mod plugins;
mod state;
mod status;
mod system;
mod theme;
mod types;
mod web;

use crate::core::MetadataCablage;
use crate::metadata::PlayerState;
use crate::plugins::{PluginKind, PluginManifest};
use crate::status::{AppState, LogBuffer, LogBufferWriter, PluginStatus, StatusState};
use crate::types::Event;
use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use ritornello_proto::{Enrichment, InputMessage, NowPlaying};
use ritornello_plugin_sdk::{run_input_client, run_metadata_client, DisplayClient, SourceClient, SourceUpdate};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, RwLock};
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
        .with_context(|| format!("loading {}", plugins_path.display()))?;
    let persisted = state::load(&state_path);

    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let catalog = Arc::new(RwLock::new(ritornello_i18n::Catalog::load(
        "core",
        persisted.locale.as_deref().unwrap_or("en"),
        &locales_root,
        core::EN,
    )));

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<InputMessage>(32);
    // Événements de mpv : un `mpsc`, pas un `broadcast` — il n'y a qu'un
    // consommateur (la boucle ci-dessous), et la sémantique avec perte de
    // `broadcast` (`Lagged`) pouvait jeter un `PlaybackIdle` que mpv, qui ne
    // signale que les transitions, n'aurait jamais réémis : flux coupé sans
    // relance jusqu'à la prochaine action. Ici, canal plein = contre-pression
    // sur la pompe d'événements, jamais de perte.
    let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(64);
    let (source_update_tx, mut source_update_rx) = mpsc::channel::<(String, SourceUpdate)>(32);
    // Ce qui joue, vers les plugins `metadata` : un `watch`, parce que seule la
    // dernière valeur compte et qu'un plugin lent ne doit pas bloquer le cœur.
    let (now_playing_tx, now_playing_rx) = watch::channel(NowPlaying {
        source: persisted.active_source.clone(),
        identity: None,
    });
    // État structuré du lecteur : vers la SPA (route SSE) et vers les plugins
    // Display, qui composent eux-mêmes leur mise en page depuis cette même
    // trame (un seul canal depuis la Task 4 de « afficheurs, état structuré »).
    let (etat_tx, etat_rx) = watch::channel(PlayerState {
        source: persisted.active_source.clone(),
        ..Default::default()
    });
    let (enrich_tx, mut enrich_rx) = mpsc::channel::<(String, Enrichment)>(32);
    let (audio_tx, mut audio_rx) = mpsc::channel::<Option<String>>(4);
    let (locale_tx, mut locale_rx) = mpsc::channel::<String>(4);
    let (theme_tx, mut theme_rx) = mpsc::channel::<theme::ThemeState>(4);
    let (settings_tx, mut settings_rx) = mpsc::channel::<state::Settings>(4);

    // mpv. Les deux durées de tampon sont réglables sans recompiler : la bonne
    // valeur dépend du réseau et de la charge de la machine, pas du code.
    let audio_buffer_brut = std::env::var("RITORNELLO_AUDIO_BUFFER").ok();
    let readahead_brut = std::env::var("RITORNELLO_NETWORK_READAHEAD").ok();
    let audio_buffer = player::mpv::audio_buffer_regle(audio_buffer_brut.as_deref());
    let readahead = player::mpv::readahead_regle(readahead_brut.as_deref());
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, audio_buffer, readahead, ev_tx)
            .await
            .context("starting mpv")?;

    // Plugins `metadata` déclarés, **dans l'ordre du fichier** : cet ordre est
    // la priorité d'arbitrage. La liste est bâtie depuis le manifeste et non
    // depuis les plugins effectivement lancés — la priorité est une propriété
    // de configuration, pas d'exécution ; un plugin qui n'a pas démarré ne
    // répondra jamais, donc ne gagnera jamais, sans que l'ordre des autres
    // change d'un démarrage à l'autre.
    let metadata_plugins: Vec<String> = manifest
        .plugins
        .iter()
        .filter(|p| p.kind == PluginKind::Metadata)
        .map(|p| p.name.clone())
        .collect();

    // Spawn et connexion de chaque plugin déclaré.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    let mut plugin_statuses = Vec::new();
    let mut plugin_waits = FuturesUnordered::new();
    let mut source_connects = Vec::new();
    let mut display_connect = None;
    let mut admin_connects = Vec::new();

    for p in &manifest.plugins {
        let socket_path = PathBuf::from(format!("{runtime_dir}/{}.sock", p.name));
        // La socket d'admin est proposée à **tous** les plugins : celui qui a
        // une page la lie (c'est sa déclaration), les autres ignorent
        // l'argument. Plus de champ `admin` dans plugins.toml — c'était une
        // propriété du binaire que l'opérateur devait connaître, et son oubli
        // produisait un mode dégradé silencieux.
        let admin_socket = PathBuf::from(format!("{runtime_dir}/{}-admin.sock", p.name));
        match plugins::spawn(&p.exec, &socket_path, &admin_socket, persisted.locale.as_deref()) {
            Ok(child) => {
                let wname = p.name.clone();
                plugin_waits.push(async move {
                    let mut child = child;
                    let status = child.wait().await;
                    (wname, status)
                });
                {
                    let name = p.name.clone();
                    admin_connects.push(tokio::spawn(async move {
                        // Le fichier a été supprimé avant le spawn : son
                        // apparition ne peut venir que du plugin. La liaison
                        // est la première chose que fait une moitié admin, la
                        // fenêtre est donc large — et elle court en parallèle
                        // des connexions de genre, pas après.
                        if !plugins::attend_liaison(&admin_socket, std::time::Duration::from_secs(2)).await {
                            return (name, None);
                        }
                        match ritornello_plugin_sdk::AdminClient::connect(&admin_socket).await {
                            Ok(client) => (name, Some(client)),
                            Err(e) => {
                                tracing::warn!("admin plugin {name} unreachable: {e}");
                                (name, None)
                            }
                        }
                    }));
                }
                match p.kind {
                    PluginKind::Source => {
                        let name = p.name.clone();
                        let update_tx = source_update_tx.clone();
                        source_connects.push(tokio::spawn(async move {
                            let result = SourceClient::connect(&socket_path, name.clone(), update_tx).await;
                            (name, result)
                        }));
                    }
                    PluginKind::Display => {
                        let name = p.name.clone();
                        display_connect = Some(tokio::spawn(async move {
                            let result = DisplayClient::connect(&socket_path).await;
                            (name, result)
                        }));
                    }
                    PluginKind::Input => {
                        let tx = cmd_tx.clone();
                        let socket_for_task = socket_path.clone();
                        let name = p.name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_input_client(&socket_for_task, tx).await {
                                tracing::warn!("input plugin {name} disconnected: {e}");
                            }
                        });
                        // `admin` est posé à faux partout ici : la détection
                        // (liaison de la socket d'admin) complète le drapeau
                        // plus bas, une fois les tâches d'observation jointes.
                        plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "input".into(), connected: true, admin: false });
                    }
                    PluginKind::Metadata => {
                        // Relais dans les deux sens, dans sa propre tâche : sa
                        // panne ne concerne que les métadonnées. **La lecture
                        // n'est jamais affectée** par un plugin `metadata`.
                        let tx = enrich_tx.clone();
                        let np_rx = now_playing_rx.clone();
                        let socket_for_task = socket_path.clone();
                        let name = p.name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_metadata_client(&socket_for_task, name.clone(), tx, np_rx).await {
                                tracing::warn!("metadata plugin {name} disconnected: {e}");
                            }
                        });
                        plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "metadata".into(), connected: true, admin: false });
                    }
                }
            }
            Err(e) => {
                // `{e:#}` et non `{e}` : la chaîne de contexte porte le chemin
                // cherché, que le seul message d'erreur système n'indique pas.
                tracing::warn!("failed to launch plugin {}: {e:#}", p.name);
                plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: format!("{:?}", p.kind).to_lowercase(), connected: false, admin: false });
            }
        }
    }

    for handle in source_connects {
        let (name, result) = handle.await.context("source plugin connection task interrupted")?;
        match result {
            Ok(client) => {
                sources.insert(name.clone(), client);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: true, admin: false });
            }
            Err(e) => {
                tracing::warn!("plugin {} unavailable: {e}", name);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: false, admin: false });
            }
        }
    }

    let mut display_client: Option<Arc<DisplayClient>> = None;
    if let Some(handle) = display_connect {
        let (name, result) = handle.await.context("display plugin connection task interrupted")?;
        match result {
            Ok(client) => {
                display_client = Some(client);
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: true, admin: false });
            }
            Err(e) => {
                tracing::warn!("display plugin {name} unavailable: {e}");
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: false, admin: false });
            }
        }
    }

    // La page d'admin est une capacité **observée** : le drapeau des statuts
    // suit ce qui a réellement été détecté, pas une déclaration de fichier.
    let mut admin_backends: HashMap<String, Arc<dyn admin::AdminBackend>> = HashMap::new();
    for handle in admin_connects {
        let (name, backend) = handle.await.context("admin detection task interrupted")?;
        if let Some(client) = backend {
            if let Some(st) = plugin_statuses.iter_mut().find(|s| s.name == name) {
                st.admin = true;
            }
            admin_backends.insert(name, client);
        }
    }

    if sources.is_empty() {
        anyhow::bail!("no source available (plugins.toml empty or all source plugins unavailable)");
    }

    // Relais de l'état vers le plugin d'affichage, s'il est connecté : le même
    // canal qui alimente la route SSE de la SPA, le plugin composant lui-même
    // sa mise en page depuis la trame reçue.
    match display_client {
        Some(display_client) => {
            let mut display_rx = etat_rx.clone();
            tokio::spawn(async move {
                loop {
                    if display_rx.changed().await.is_err() {
                        break;
                    }
                    let etat = display_rx.borrow_and_update().clone();
                    if let Err(e) = display_client.send(&etat).await {
                        tracing::warn!("display: {e}");
                    }
                }
            });
        }
        None => tracing::warn!("no display plugin connected, continuing without display"),
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
    let settings_current = Arc::new(RwLock::new(persisted.settings.clone()));
    {
        // Asked once, before serving: the answer gates the System tab's two
        // OS buttons, and asking per request would mean spawning `busctl`
        // twice every five seconds.
        let sonde = system::probe_capabilities().await;
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
            settings_current: settings_current.clone(),
            settings_tx: settings_tx.clone(),
            player: etat_rx.clone(),
            system: Arc::new(system::SystemInfo {
                can_power_off: sonde.can_power_off,
                can_reboot: sonde.can_reboot,
                logind_reachable: sonde.logind_reachable,
                // Le crochet de relance tue mpv **avant** de sortir. Sans
                // cela, mpv survivait au cœur et continuait de jouer : il est
                // lancé en `kill_on_drop(true)`, mais `std::process::exit` ne
                // déroule pas la pile et n'exécute donc aucun `Drop` — la
                // garantie annoncée par `kill_on_drop` ne valait rien sur ce
                // chemin.
                //
                // Le service ne le montrait pas : quand le processus principal
                // d'une unité sort, systemd tue le reste du groupe de contrôle
                // avant de relancer. C'est en développement, sans superviseur,
                // que l'orphelin restait — à jouer, et à tenir le périphérique
                // audio que le cœur relancé voulait reprendre.
                //
                // La mort de mpv fait aussi sortir la boucle principale (voir
                // `mpv_child.wait()` plus bas) : les deux chemins courent,
                // mais ils mènent au même endroit, et c'est l'`exit(0)`
                // ci-dessous qui gagne en pratique. Le détail du signal et sa
                // justification vivent dans `system::terminate_process`, où un
                // test les épingle sur un vrai processus.
                restart: {
                    let pid = mpv_child.id();
                    Arc::new(move || {
                        system::terminate_process(pid);
                        std::process::exit(0)
                    })
                },
                ..Default::default()
            }),
        });
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("web interface on http://{http_addr}/");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("status server: {e}");
            }
        });
    }

    // Cœur. La source active affichée est tenue à jour en direct par la boucle
    // ci-dessous (mise à jour de status_state.active_source après chaque commande).
    let start_in_standby = persisted.settings.start_in_standby;
    let mut core = core::Core::new(
        mpv_player,
        core::Cablage {
            sources,
            persisted,
            state_path,
            catalog: catalog.clone(),
            locales_root: locales_root.clone(),
            metadata: MetadataCablage {
                plugins: metadata_plugins,
                now_playing: now_playing_tx,
                etat: etat_tx,
            },
        },
    );
    // Best-effort, like the wake via `Power` (see the comment below): startup
    // must never put systemd in a restart loop. `start_in_standby` skips the
    // source wake but still configures mpv, so the first `Power` starts right.
    let demarrage = if start_in_standby { core.start_in_standby().await } else { core.resume().await };
    if let Err(e) = demarrage {
        tracing::warn!("startup wake: {e}");
    }

    let mut retry_at: Option<tokio::time::Instant> = None;

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
            Some(msg) = cmd_rx.recv() => {
                if let Err(e) = core.handle_input(msg).await {
                    tracing::warn!("command: {e}");
                }
                status_state.write().await.active_source = core.active_source().to_string();
            }
            // Canal fermé (pompe mpv morte) : le motif `Some(..)` cesse de
            // matcher et `tokio::select!` désactive le bras — le bras
            // `mpv_child.wait()` prendra le relais pour sortir proprement.
            Some(ev) = ev_rx.recv() => {
                // C'est le cœur qui qualifie l'événement (voir `EventOutcome`) :
                // la liste des variantes qui attestent la vivacité du flux
                // n'existe qu'à un seul endroit.
                match core.handle_event(ev).await {
                    core::EventOutcome::StreamAlive => retry_at = None,
                    core::EventOutcome::RetryIn(delay) => {
                        retry_at = Some(tokio::time::Instant::now() + delay);
                    }
                    core::EventOutcome::Nothing => {}
                }
            }
            Some((name, update)) = source_update_rx.recv() => {
                core.handle_source_update(&name, update);
            }
            Some((plugin, enrichment)) = enrich_rx.recv() => {
                core.handle_enrichment(&plugin, enrichment);
            }
            Some(device) = audio_rx.recv() => {
                if let Err(e) = core.set_audio_device(device).await {
                    tracing::warn!("audio output change: {e}");
                }
            }
            Some(locale) = locale_rx.recv() => {
                if let Err(e) = core.set_locale(locale).await {
                    tracing::warn!("locale change: {e}");
                }
            }
            Some(t) = theme_rx.recv() => {
                core.set_theme(t);
            }
            Some(s) = settings_rx.recv() => {
                core.set_settings(s);
            }
            _ = retry_sleep => {
                retry_at = None;
                if let Err(e) = core.retry_stream().await {
                    tracing::warn!("stream retry: {e}");
                }
            }
            _ = overlay_sleep => {
                core.expire_overlay();
            }
            // `next()` et non `select_next_some()` : `tokio::select!` ne
            // consulte pas `is_terminated`, et re-poller un `FuturesUnordered`
            // épuisé via `select_next_some` panique (`SelectNextSome polled
            // after terminated`) — c'est-à-dire que la mort du **dernier**
            // plugin tuait le cœur à l'itération suivante, l'inverse exact de
            // la dégradation voulue. Avec `next()`, l'épuisement rend `None`,
            // le motif ne matche pas, et le bras est simplement désactivé.
            Some((name, status)) = plugin_waits.next() => {
                tracing::warn!("plugin {name} exited: {status:?}");
                crate::status::mark_plugin_disconnected(&mut *status_state.write().await, &name);
            }
            status = mpv_child.wait() => {
                anyhow::bail!("mpv exited ({status:?}), stopping for restart by systemd");
            }
        }
    }
}
