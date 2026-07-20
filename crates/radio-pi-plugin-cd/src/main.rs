mod cd;
mod disc;
mod musicbrainz;

use anyhow::Result;
use disc::DiscInfo;
use radio_pi_plugin_sdk::{run_source_plugin, SourceOutcome, SourcePlugin};
use radio_pi_proto::{SourceAction, View};
use std::path::PathBuf;
use tokio::sync::mpsc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct CdSource {
    cd_dev: String,
    present: bool,
    track: i64,
    info: Option<DiscInfo>,
    total_tracks: usize,
    epoch: u64,
    presence_rx: mpsc::Receiver<bool>,
    metadata_tx: mpsc::Sender<(u64, usize, Option<DiscInfo>)>,
    metadata_rx: mpsc::Receiver<(u64, usize, Option<DiscInfo>)>,
}

impl CdSource {
    fn view(&self) -> View {
        if !self.present {
            return View { line1: "CD".into(), line2: "pas de disque".into(), line3: String::new() };
        }
        let n = self.track.max(0) as usize;
        match &self.info {
            Some(info) => View {
                line1: format!("CD  {}/{}", n + 1, info.tracks.len()),
                line2: format!("{} — {}", info.artist, info.album),
                line3: info.tracks.get(n).cloned().unwrap_or_default(),
            },
            None => View {
                line1: format!("CD  piste {}", n + 1),
                line2: "CD audio".into(),
                line3: String::new(),
            },
        }
    }

    fn spawn_lookup(&self) {
        let cd_dev = self.cd_dev.clone();
        let tx = self.metadata_tx.clone();
        let epoch = self.epoch;
        tokio::spawn(async move {
            let toc_result = tokio::task::spawn_blocking(move || {
                cd::read_toc(&cd_dev).and_then(|raw| cd::mb_toc_param(&raw))
            })
            .await;
            let (total_tracks, info) = match toc_result {
                Ok(Ok((toc, n))) => {
                    let info = match musicbrainz::lookup(&toc, n).await {
                        Ok(info) => info,
                        Err(e) => {
                            tracing::info!("lookup MusicBrainz: {e}");
                            None
                        }
                    };
                    (n, info)
                }
                Ok(Err(e)) => {
                    tracing::info!("TOC illisible: {e}");
                    (0, None)
                }
                Err(e) => {
                    tracing::warn!("tache TOC interrompue: {e}");
                    (0, None)
                }
            };
            let _ = tx.send((epoch, total_tracks, info)).await;
        });
    }
}

#[async_trait::async_trait]
impl SourcePlugin for CdSource {
    async fn activate(&mut self) -> SourceOutcome {
        if self.present {
            SourceOutcome { action: SourceAction::Play { uri: "cdda://".into() }, view: Some(self.view()) }
        } else {
            SourceOutcome { action: SourceAction::Noop, view: Some(self.view()) }
        }
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Stop, view: None }
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        if !self.present || n == 0 {
            return SourceOutcome { action: SourceAction::Noop, view: None };
        }
        if self.total_tracks > 0 && (n as usize) > self.total_tracks {
            return SourceOutcome { action: SourceAction::Noop, view: Some(self.view()) };
        }
        self.track = (n - 1) as i64;
        SourceOutcome { action: SourceAction::Play { uri: format!("cdda://{n}") }, view: Some(self.view()) }
    }
    async fn next(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn prev(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn next_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::PlayerNext, view: None }
    }
    async fn prev_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::PlayerPrev, view: None }
    }
    async fn eject(&mut self) -> SourceOutcome {
        let cd_dev = self.cd_dev.clone();
        tokio::spawn(async move {
            tokio::task::spawn_blocking(move || cd::eject(&cd_dev)).await.ok();
        });
        self.present = false;
        self.track = 0;
        self.info = None;
        self.total_tracks = 0;
        self.epoch = self.epoch.wrapping_add(1);
        SourceOutcome { action: SourceAction::Stop, view: Some(self.view()) }
    }

    async fn poll_notification(&mut self) -> Option<View> {
        tokio::select! {
            presence = self.presence_rx.recv() => {
                let present = presence?;
                self.present = present;
                self.track = 0;
                self.info = None;
                self.total_tracks = 0;
                self.epoch = self.epoch.wrapping_add(1);
                if present {
                    self.spawn_lookup();
                }
                Some(self.view())
            }
            metadata = self.metadata_rx.recv() => {
                let (epoch, total_tracks, info) = metadata?;
                if epoch != self.epoch {
                    return None;
                }
                self.total_tracks = total_tracks;
                self.info = info;
                Some(self.view())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = socket_path_from_args();
    let cd_dev = env_or("RADIO_PI_CD_DEV", "/dev/sr0");

    let (presence_tx, presence_rx) = mpsc::channel(8);
    tokio::spawn(cd::watch(PathBuf::from(cd_dev.clone()), presence_tx));

    let (metadata_tx, metadata_rx) = mpsc::channel::<(u64, usize, Option<DiscInfo>)>(4);

    let source = CdSource {
        cd_dev,
        present: false,
        track: 0,
        info: None,
        total_tracks: 0,
        epoch: 0,
        presence_rx,
        metadata_tx,
        metadata_rx,
    };
    run_source_plugin(source, &socket_path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_with_channels() -> (CdSource, mpsc::Sender<bool>, mpsc::Sender<(u64, usize, Option<DiscInfo>)>) {
        let (presence_tx, presence_rx) = mpsc::channel(8);
        let (metadata_tx, metadata_rx) = mpsc::channel(4);
        let source = CdSource {
            cd_dev: "/dev/sr0".into(),
            present: true,
            track: 0,
            info: None,
            total_tracks: 0,
            epoch: 5,
            presence_rx,
            metadata_tx: metadata_tx.clone(),
            metadata_rx,
        };
        (source, presence_tx, metadata_tx)
    }

    #[tokio::test]
    async fn resultat_perime_ignore_resultat_frais_applique() {
        let (mut source, _presence_tx, metadata_tx) = source_with_channels();
        // Un resultat perime (epoch 4, alors que source.epoch == 5) est ignore.
        metadata_tx.send((4, 99, None)).await.unwrap();
        let view = source.poll_notification().await;
        assert!(view.is_none(), "un resultat perime ne doit produire aucune notification");
        assert_eq!(source.total_tracks, 0, "l'etat ne doit pas etre modifie par un resultat perime");

        // Un resultat a jour (epoch 5) est applique.
        metadata_tx.send((5, 12, None)).await.unwrap();
        let view = source.poll_notification().await;
        assert!(view.is_some());
        assert_eq!(source.total_tracks, 12);
    }
}
