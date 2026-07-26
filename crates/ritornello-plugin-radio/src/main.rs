mod admin;
mod config;
mod state;

use crate::admin::RadioAdmin;
use anyhow::Result;
use config::Stations;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{run_admin_plugin, run_source_plugin, SourceOutcome, SourcePlugin};
use ritornello_proto::{SourceAction, View};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

pub(crate) const RADIO_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn arg_value(flag: &str) -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == flag).map(|i| PathBuf::from(&args[i + 1]))
}

struct RadioSource {
    state_path: PathBuf,
    stations: Arc<AsyncRwLock<Stations>>,
    preset: u8,
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
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
            let empty = self.catalog.read().unwrap().get("empty_preset").to_string();
            SourceOutcome { action: SourceAction::Noop, view: Some(self.view_for(self.preset, &empty)) }
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
    async fn set_locale(&mut self, locale: String) {
        *self.catalog.write().unwrap() = Catalog::load("radio", &locale, &self.locales_root, RADIO_EN);
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
    let stations_shared = Arc::new(AsyncRwLock::new(stations));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let catalog = Arc::new(RwLock::new(Catalog::load("radio", "en", &locales_root, RADIO_EN)));

    let source = RadioSource {
        state_path,
        stations: stations_shared.clone(),
        preset,
        catalog: catalog.clone(),
        locales_root,
    };
    let admin = RadioAdmin { stations_path, stations: stations_shared, catalog };

    // Les deux moitiés sont indépendantes : une panne (déconnexion, erreur
    // d'écriture, voire panique sur un lock empoisonné) sur la socket admin ne
    // doit pas tuer la lecture audio, et réciproquement. Chaque moitié tourne
    // dans sa propre tâche tokio::spawn : une panique y est capturée dans le
    // JoinHandle (JoinError) au lieu de dérouler la pile de l'autre moitié,
    // ce qu'un simple tokio::join! sur des blocs async inline ne garantirait
    // pas (les deux futures seraient pollées dans la même tâche).
    let source_handle = tokio::spawn(async move { run_source_plugin(source, &socket_path).await });
    let admin_handle = tokio::spawn(async move { run_admin_plugin(admin, &admin_socket).await });

    let (source_res, admin_res) = tokio::join!(source_handle, admin_handle);

    match source_res {
        Ok(Ok(())) => tracing::warn!("plugin radio (moitie source) termine normalement"),
        Ok(Err(e)) => tracing::warn!("plugin radio (moitie source) erreur: {e}"),
        Err(join_err) => tracing::error!("plugin radio (moitie source) a panique: {join_err}"),
    }
    match admin_res {
        Ok(Ok(())) => tracing::warn!("plugin radio (moitie admin) termine normalement"),
        Ok(Err(e)) => tracing::warn!("plugin radio (moitie admin) erreur: {e}"),
        Err(join_err) => tracing::error!("plugin radio (moitie admin) a panique: {join_err}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_preset_utilise_le_catalogue_apres_set_locale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "empty_preset = \"PRESET VIDE\"\n").unwrap();

        let state_dir = tempfile::tempdir().unwrap();
        let catalog = Arc::new(RwLock::new(Catalog::load("radio", "en", dir.path(), RADIO_EN)));
        let mut source = RadioSource {
            state_path: state_dir.path().join("plugin-radio.json"),
            stations: Arc::new(AsyncRwLock::new(Stations::default())),
            preset: 1,
            catalog: catalog.clone(),
            locales_root: dir.path().to_path_buf(),
        };
        source.set_locale("fr".into()).await;
        // aucun preset chargé → branche "empty_preset"
        let outcome = source.select(1).await;
        assert_eq!(outcome.view.unwrap().line2, "PRESET VIDE");
    }

    #[test]
    fn en_embarque_radio_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(RADIO_EN).unwrap().is_empty());
    }
}
