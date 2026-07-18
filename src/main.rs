mod cd;
mod config;
mod core;
mod display;
mod input;
mod keymap;
mod musicbrainz;
mod player;
mod state;
mod types;
mod web;

use crate::config::Stations;
use crate::types::{Command, DiscInfo, Event, View};
use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::sync::{broadcast, mpsc, watch};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let stations_path = PathBuf::from(env_or("RADIO_PI_STATIONS", "/etc/radio-pi/stations.toml"));
    let state_path = PathBuf::from(env_or("RADIO_PI_STATE", "/var/lib/radio-pi/state.json"));
    let mpv_socket = PathBuf::from(env_or("RADIO_PI_MPV_SOCKET", "/run/radio-pi/mpv.sock"));
    let mpv_bin = env_or("RADIO_PI_MPV_BIN", "mpv");
    let tty = PathBuf::from(env_or("RADIO_PI_TTY", "/dev/tty1"));
    let input_name = env_or("RADIO_PI_INPUT_NAME", "Media Center");
    let cd_dev = env_or("RADIO_PI_CD_DEV", "/dev/sr0");
    let http_addr = env_or("RADIO_PI_HTTP", "0.0.0.0:8080");

    let stations = Stations::load(&stations_path).unwrap_or_else(|e| {
        tracing::warn!("stations.toml invalide ou absent ({e}) : démarrage sans stations");
        Stations::default()
    });
    let persisted = state::load(&state_path);

    // Canaux : commandes (input/web) -> core ; événements (mpv/cd) -> core ; vue -> display.
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
    let (ev_tx, _) = broadcast::channel::<Event>(64);
    let mut ev_rx = ev_tx.subscribe();
    let (view_tx, mut view_rx) = watch::channel(View::default());
    let (disc_tx, mut disc_rx) = mpsc::channel::<Option<DiscInfo>>(4);

    // mpv
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, ev_tx.clone())
            .await
            .context("démarrage de mpv")?;

    // Affichage console (HDMI). Non bloquant pour le reste si le tty manque (dev).
    match display::ConsoleDisplay::open(&tty) {
        Ok(mut disp) => {
            tokio::spawn(async move {
                loop {
                    if view_rx.changed().await.is_err() {
                        break;
                    }
                    let v = view_rx.borrow_and_update().clone();
                    if let Err(e) = disp.show(&v) {
                        tracing::warn!("affichage: {e}");
                    }
                }
            });
        }
        Err(e) => tracing::warn!("pas d'affichage ({e}), on continue sans"),
    }

    // Télécommande. Absence tolérée (utile en dev sans récepteur branché).
    match input::find_device(&input_name) {
        Ok(dev) => {
            let tx = cmd_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = input::run(dev, tx).await {
                    tracing::error!("boucle evdev terminée: {e}");
                }
            });
        }
        Err(e) => tracing::warn!("télécommande absente: {e}"),
    }

    // Détection CD.
    {
        let ev_tx = ev_tx.clone();
        let dev = PathBuf::from(cd_dev.clone());
        tokio::spawn(async move {
            let (tx, mut rx) = mpsc::channel::<Event>(8);
            tokio::spawn(cd::watch(dev, tx));
            while let Some(ev) = rx.recv().await {
                let _ = ev_tx.send(ev);
            }
        });
    }

    // Web.
    {
        let app = web::router(web::WebState {
            stations_path: stations_path.clone(),
            cmd_tx: cmd_tx.clone(),
        });
        let listener = tokio::net::TcpListener::bind(&http_addr)
            .await
            .with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("interface web sur http://{http_addr}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("serveur web: {e}");
            }
        });
    }

    // Cœur.
    let mut core = core::Core::new(
        mpv_player,
        stations,
        persisted,
        state_path,
        stations_path,
        view_tx,
    );
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
                let eject = cmd == Command::Eject;
                if let Err(e) = core.handle_command(cmd).await {
                    tracing::warn!("commande: {e}");
                }
                if eject {
                    cd::eject(&cd_dev);
                }
            }
            Ok(ev) = ev_rx.recv() => {
                let cd_inserted = ev == Event::CdInserted;
                if matches!(ev, Event::Title(_) | Event::PlaybackActive) {
                    retry_at = None; // la lecture est repartie : on annule le retry programme
                }
                if let Some(delay) = core.handle_event(ev).await {
                    retry_at = Some(tokio::time::Instant::now() + delay);
                }
                if cd_inserted {
                    let dev = cd_dev.clone();
                    let disc_tx = disc_tx.clone();
                    tokio::spawn(async move {
                        let toc = tokio::task::spawn_blocking(move || {
                            cd::read_toc(&dev).and_then(|raw| cd::mb_toc_param(&raw))
                        })
                        .await;
                        let info = match toc {
                            Ok(Ok((toc, n))) => match musicbrainz::lookup(&toc, n).await {
                                Ok(info) => info,
                                Err(e) => {
                                    tracing::info!("lookup MusicBrainz: {e}");
                                    None
                                }
                            },
                            Ok(Err(e)) => {
                                tracing::info!("TOC illisible: {e}");
                                None
                            }
                            Err(e) => {
                                tracing::warn!("tache TOC interrompue: {e}");
                                None
                            }
                        };
                        let _ = disc_tx.send(info).await;
                    });
                }
            }
            Some(info) = disc_rx.recv() => {
                core.set_disc_info(info);
            }
            _ = retry_sleep => {
                retry_at = None;
                if let Err(e) = core.retry_stream().await {
                    tracing::warn!("retry flux: {e}");
                }
            }
            status = mpv_child.wait() => {
                // mpv est mort : on quitte, systemd relance le service entier.
                anyhow::bail!("mpv terminé ({status:?}), arrêt pour relance par systemd");
            }
        }
    }
}
