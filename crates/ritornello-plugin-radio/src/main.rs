mod admin;
mod config;
mod directory;
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
            // Une seule station configurée : next_preset reboucle sur le
            // preset courant. Rejouer provoquerait une reconnexion du flux
            // live (loadfile mpv), audible comme un changement de station
            // alors que l'affichage ne bouge pas. Rien à faire dans ce cas.
            Some(n) if n == self.preset => SourceOutcome { action: SourceAction::Noop, view: None },
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome { action: SourceAction::Noop, view: None },
        }
    }
    async fn prev(&mut self) -> SourceOutcome {
        let prev = self.stations.read().await.prev_preset(self.preset);
        match prev {
            // Voir commentaire dans next() : même garde contre la
            // reconnexion audible quand une seule station est configurée.
            Some(n) if n == self.preset => SourceOutcome { action: SourceAction::Noop, view: None },
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
    // `--admin-socket` n'est fourni par le cœur que si `admin = true` dans
    // plugins.toml. Absent (oubli lors d'une mise à jour de plugins.toml, ou
    // usage volontaire sans page d'admin), on continue en mode dégradé :
    // la moitié Source tourne seule, sans page de gestion des stations.
    let admin_socket = arg_value("--admin-socket");
    if admin_socket.is_none() {
        tracing::warn!(
            "--admin-socket absent : la page de gestion des stations ne sera pas servie, seule la moitie Source tourne (il manque 'admin = true' dans plugins.toml)"
        );
    }
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
    // Annuaire en ligne : la liste intégrée de serveurs, essayés dans l'ordre
    // jusqu'au premier qui répond, ou l'unique serveur épinglé par
    // `RITORNELLO_RADIO_DIRECTORY`. Journalisé au démarrage : sur un Pi sans
    // écran, savoir quels serveurs seront interrogés évite de deviner.
    let directory = directory::HttpDirectory::from_env();
    tracing::info!("annuaire radio, serveurs candidats: {}", directory.bases.join(", "));
    // La moitié admin n'est construite que si `--admin-socket` a été fourni
    // (mode dégradé sinon, voir plus haut).
    let admin = admin_socket.map(|admin_socket| {
        (
            RadioAdmin {
                stations_path,
                stations: stations_shared,
                catalog,
                directory: Arc::new(directory),
                search: RwLock::new(Vec::new()),
            },
            admin_socket,
        )
    });

    // Les deux moitiés sont indépendantes : une panne (déconnexion, erreur
    // d'écriture, voire panique sur un lock empoisonné) sur la socket admin ne
    // doit pas tuer la lecture audio, et réciproquement. Chaque moitié tourne
    // dans sa propre tâche tokio::spawn : une panique y est capturée dans le
    // JoinHandle (JoinError) au lieu de dérouler la pile de l'autre moitié,
    // ce qu'un simple tokio::join! sur des blocs async inline ne garantirait
    // pas (les deux futures seraient pollées dans la même tâche). Quand
    // `admin` est `None`, seule la moitié Source est lancée : jamais de
    // `try_join!` ici, les deux tâches (quand les deux existent) restent
    // suivies indépendamment.
    let source_handle = tokio::spawn(async move { run_source_plugin(source, &socket_path).await });

    match admin {
        Some((admin, admin_socket)) => {
            let admin_handle = tokio::spawn(async move { run_admin_plugin(admin, &admin_socket).await });
            let (source_res, admin_res) = tokio::join!(source_handle, admin_handle);
            log_half("moitie source", source_res);
            log_half("moitie admin", admin_res);
        }
        None => {
            log_half("moitie source", source_handle.await);
        }
    }

    Ok(())
}

/// Logue le résultat d'une des deux moitiés (succès / erreur applicative /
/// panique) sans jamais faire remonter l'échec d'une moitié sur l'autre.
fn log_half(label: &str, res: std::result::Result<Result<()>, tokio::task::JoinError>) {
    match res {
        Ok(Ok(())) => tracing::warn!("plugin radio ({label}) termine normalement"),
        Ok(Err(e)) => tracing::warn!("plugin radio ({label}) erreur: {e}"),
        Err(join_err) => tracing::error!("plugin radio ({label}) a panique: {join_err}"),
    }
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

    fn make_source(stations: Stations, preset: u8) -> RadioSource {
        let dir = tempfile::tempdir().unwrap();
        RadioSource {
            state_path: dir.path().join("plugin-radio.json"),
            stations: Arc::new(AsyncRwLock::new(stations)),
            preset,
            catalog: Arc::new(RwLock::new(Catalog::load("radio", "en", dir.path(), RADIO_EN))),
            locales_root: dir.path().to_path_buf(),
        }
    }

    fn one_station() -> Stations {
        config::Stations {
            stations: vec![config::Station {
                name: "FIP".into(),
                url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(),
                preset: 1,
            }],
        }
    }

    fn two_stations() -> Stations {
        config::Stations {
            stations: vec![
                config::Station {
                    name: "FIP".into(),
                    url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(),
                    preset: 1,
                },
                config::Station {
                    name: "France Inter".into(),
                    url: "http://icecast.radiofrance.fr/franceinter-midfi.mp3".into(),
                    preset: 2,
                },
            ],
        }
    }

    #[tokio::test]
    async fn une_seule_station_next_et_prev_sont_sans_effet() {
        let mut source = make_source(one_station(), 1);
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.view.is_none());

        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Noop));
        assert!(outcome.view.is_none());
    }

    #[tokio::test]
    async fn deux_stations_next_et_prev_rebouclent_toujours_vers_l_autre() {
        let mut source = make_source(two_stations(), 1);
        let outcome = source.next().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));

        let mut source = make_source(two_stations(), 1);
        let outcome = source.prev().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));
    }

    #[tokio::test]
    async fn activate_sur_le_preset_courant_rejoue_quand_meme_le_flux() {
        // Chemin de récupération après coupure (retry_stream côté cœur) :
        // activate() doit continuer à rejouer le même preset, sans la garde
        // ajoutée à next()/prev().
        let mut source = make_source(one_station(), 1);
        let outcome = source.activate().await;
        assert!(matches!(outcome.action, SourceAction::Play { .. }));
    }
}
