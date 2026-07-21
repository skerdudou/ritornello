mod admin;
mod config;
mod state;

use crate::admin::RadioAdmin;
use anyhow::Result;
use config::Stations;
use ritornello_plugin_sdk::{run_admin_plugin, run_source_plugin, SourceOutcome, SourcePlugin};
use ritornello_proto::{SourceAction, View};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn arg_value(flag: &str) -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == flag).map(|i| PathBuf::from(&args[i + 1]))
}

struct RadioSource {
    state_path: PathBuf,
    stations: Arc<RwLock<Stations>>,
    preset: u8,
}

impl RadioSource {
    fn view_for(&self, preset: u8, status: &str) -> View {
        View { line1: format!("RADIO  P{preset}"), line2: status.to_string(), line3: String::new() }
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

    let socket_path = arg_value("--socket").expect("--socket <path> requis");
    let admin_socket = arg_value("--admin-socket").expect("--admin-socket <path> requis");
    let stations_path = PathBuf::from(env_or("RITORNELLO_RADIO_STATIONS", "/etc/ritornello/stations.toml"));
    let state_path = PathBuf::from(env_or("RITORNELLO_RADIO_STATE", "/var/lib/ritornello/plugin-radio.json"));

    let stations = Stations::load(&stations_path).unwrap_or_else(|e| {
        tracing::warn!("stations.toml invalide ou absent ({e}) : demarrage sans stations");
        Stations::default()
    });
    let preset = state::load(&state_path).preset;
    let stations_shared = Arc::new(RwLock::new(stations));

    let source = RadioSource { state_path, stations: stations_shared.clone(), preset };
    let admin = RadioAdmin { stations_path, stations: stations_shared };

    tokio::try_join!(
        run_source_plugin(source, &socket_path),
        run_admin_plugin(admin, &admin_socket),
    )?;
    Ok(())
}
