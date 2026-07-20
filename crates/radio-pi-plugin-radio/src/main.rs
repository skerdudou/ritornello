mod config;
mod state;
mod web;

use anyhow::{Context, Result};
use config::Stations;
use radio_pi_plugin_sdk::{run_source_plugin, SourceOutcome, SourcePlugin};
use radio_pi_proto::{SourceAction, View};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct RadioSource {
    state_path: PathBuf,
    stations: Arc<RwLock<Stations>>,
    preset: u8,
}

impl RadioSource {
    fn view_for(&self, preset: u8, status: &str) -> View {
        View {
            line1: format!("RADIO  P{preset}"),
            line2: status.to_string(),
            line3: String::new(),
        }
    }

    async fn play_preset(&mut self, n: u8) -> SourceOutcome {
        let stations = self.stations.read().await;
        if let Some(st) = stations.by_preset(n) {
            self.preset = n;
            let _ = state::save(&self.state_path, &state::PluginState { preset: n });
            SourceOutcome {
                action: SourceAction::Play { uri: st.url.clone() },
                view: Some(View { line1: format!("RADIO  P{n}"), line2: st.name.clone(), line3: String::new() }),
            }
        } else {
            SourceOutcome { action: SourceAction::Noop, view: Some(self.view_for(self.preset, "présélection vide")) }
        }
    }
}

#[async_trait::async_trait]
impl SourcePlugin for RadioSource {
    async fn activate(&mut self) -> SourceOutcome {
        let preset = self.preset;
        self.play_preset(preset).await
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Stop, view: None }
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        self.play_preset(n).await
    }
    async fn next(&mut self) -> SourceOutcome {
        let next = self.stations.read().await.next_preset(self.preset);
        match next {
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome { action: SourceAction::Noop, view: None },
        }
    }
    async fn prev(&mut self) -> SourceOutcome {
        let prev = self.stations.read().await.prev_preset(self.preset);
        match prev {
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome { action: SourceAction::Noop, view: None },
        }
    }
    async fn next_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn prev_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn eject(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = socket_path_from_args();
    let stations_path = PathBuf::from(env_or("RADIO_PI_RADIO_STATIONS", "/etc/radio-pi/stations.toml"));
    let state_path = PathBuf::from(env_or("RADIO_PI_RADIO_STATE", "/var/lib/radio-pi/plugin-radio.json"));
    let http_addr = env_or("RADIO_PI_RADIO_HTTP", "0.0.0.0:8081");

    let stations = Stations::load(&stations_path).unwrap_or_else(|e| {
        tracing::warn!("stations.toml invalide ou absent ({e}) : demarrage sans stations");
        Stations::default()
    });
    let preset = state::load(&state_path).preset;
    let stations_shared = Arc::new(RwLock::new(stations));

    {
        let app = web::router(web::WebState { stations_path: stations_path.clone(), stations: stations_shared.clone() });
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("admin radio sur http://{http_addr}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("serveur web radio: {e}");
            }
        });
    }

    let source = RadioSource { state_path, stations: stations_shared, preset };
    run_source_plugin(source, &socket_path).await
}
