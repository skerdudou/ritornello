//! The core's HTTP surface: `AppState`, the router, the audio output, the
//! settings and the command route. One child module per topic — `plugins`,
//! `logs`, `locales`, `settings_validation` — and this file re-exports what
//! the rest of the crate imports, so that no external import names a child.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ritornello_i18n::Catalog;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

mod plugin_status;
mod logs;
mod locales;
use locales::{i18n_json, locale_json, locale_put};
use plugin_status::plugin_enabled_put;
pub use plugin_status::{mark_plugin_disconnected, replace_plugin_lines, PluginsControl, PluginOrder, PluginStatus};
mod settings_validation;
use logs::{logs_json, player_sse};
pub use logs::{LogBuffer, LogBufferWriter};
pub use settings_validation::validate_settings;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusState {
    pub plugins: Vec<PluginStatus>,
    pub active_source: String,
}

#[derive(Clone)]
pub struct AppState {
    pub status: Arc<RwLock<StatusState>>,
    pub logs: Arc<LogBuffer>,
    pub audio_current: Arc<RwLock<Option<String>>>,
    pub audio_tx: mpsc::Sender<Option<String>>,
    pub catalog: Arc<RwLock<ritornello_i18n::Catalog>>,
    pub locale_current: Arc<RwLock<Option<String>>>,
    pub locale_tx: mpsc::Sender<String>,
    pub locales_root: std::path::PathBuf,
    /// Reachable admin pages. Under a lock: a plugin that announces itself
    /// late must see its page appear without restarting the core.
    pub admin_backends: crate::admin::AdminBackends,
    pub admin_assets: Arc<crate::admin::AssetCache>,
    pub cmd_tx: mpsc::Sender<ritornello_proto::InputMessage>,
    pub theme_current: Arc<RwLock<crate::theme::ThemeState>>,
    pub theme_tx: mpsc::Sender<crate::theme::ThemeState>,
    /// Behavior settings shown on the config page. Same pattern as
    /// `theme_current`/`theme_tx`: the HTTP layer validates and updates the
    /// shared copy, the channel carries the change to the core loop.
    pub settings_current: Arc<RwLock<crate::state::Settings>>,
    pub settings_tx: mpsc::Sender<crate::state::Settings>,
    /// Player state (source, volume, mute, standby, track), fed by the core.
    /// A `watch`: every SSE connection clones this receiver, only the last
    /// value matters, and a slow browser cannot hold the core back.
    pub player: tokio::sync::watch::Receiver<crate::metadata::PlayerState>,
    /// Process-lifetime system facts (start instant, what logind allows),
    /// read by the System tab's endpoints. One `Arc` field rather than
    /// three loose ones: every test constructor below would otherwise grow
    /// by three lines.
    pub system: Arc<crate::system::SystemInfo>,
    /// Retained covers, served on `/api/cover/{key}`. An `Arc`: the core's
    /// download task inserts into it, the router reads from it.
    pub covers: Arc<crate::cover::CoverCache>,
    /// Enabled/disabled toggle of the plugins: the manifest to rewrite, the
    /// accepted names, and the core's ear.
    pub plugins: Arc<PluginsControl>,
    /// Catalog of the sources and their named presets, as the core broadcasts
    /// it to the displays (`Core::sources_catalog`). The same `watch` as the
    /// Display plugins': the route reads the last value, nothing is probed on
    /// the core side, and the list only changes when a source announces
    /// itself or leaves.
    pub sources_catalog: tokio::sync::watch::Receiver<ritornello_proto::SourcesCatalog>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status_json))
        .route("/api/audio-output", get(audio_output_json).put(audio_output_put))
        .route("/api/locale", get(locale_json).put(locale_put))
        .route("/api/i18n", get(i18n_json))
        .route("/api/logs", get(logs_json))
        .route("/api/player", get(player_sse))
        .route("/api/presets", get(presets_json))
        .route("/api/theme", get(crate::theme::theme_json).put(crate::theme::theme_put))
        .route("/api/settings", get(settings_json).put(settings_put))
        .route("/api/system", get(crate::system::system_json))
        .route("/api/system/power", axum::routing::post(crate::system::power_post))
        .route("/api/system/processes", get(crate::processes::processes_json))
        .route("/api/command", axum::routing::post(command_post))
        .route("/api/cover/{key}", get(crate::cover::cover_get))
        .route(
            "/plugins/{name}/api/data",
            get(crate::admin::admin_get_data).put(crate::admin::admin_put_data),
        )
        .route("/plugins/{name}/api/i18n", get(crate::admin::admin_i18n))
        .route("/plugins/{name}/{file}", get(crate::admin::admin_asset))
        .route("/api/plugins/{name}/enabled", axum::routing::put(plugin_enabled_put))
        .merge(crate::web::routes())
        .fallback(crate::web::shell)
        .with_state(state)
}

async fn status_json(State(state): State<AppState>) -> Json<StatusState> {
    let mut status = state.status.read().await.clone();
    // All probes in parallel: a busy plugin does not delay the response
    // beyond its own budget (500 ms + grace). The backends lock is released
    // before the IPC round trips, as in `admin.rs`.
    let backends = state.admin_backends.read().await.clone();
    let probes = status.plugins.iter().filter(|p| p.admin).map(|p| {
        let backend = backends.get(&p.name).cloned();
        let name = p.name.clone();
        async move {
            let busy = match backend {
                Some(d) => matches!(
                    d.ping().await.map_err(|e| e.downcast::<ritornello_plugin_sdk::AdminIpcError>()),
                    Err(Ok(ritornello_plugin_sdk::AdminIpcError::Timeout))
                ),
                None => false,
            };
            (name, busy)
        }
    });
    let verdicts: std::collections::HashMap<String, bool> =
        futures::future::join_all(probes).await.into_iter().collect();
    for p in status.plugins.iter_mut() {
        p.busy = verdicts.get(&p.name).copied().unwrap_or(false);
    }
    Json(status)
}

/// The named presets of every source, for the tiles of the web remote. A
/// read of the current value, not a stream: the page reloads it on a source
/// change (the SSE frame tells it so), and that is enough — see the spec,
/// decision 6.
async fn presets_json(State(state): State<AppState>) -> Json<ritornello_proto::SourcesCatalog> {
    Json(state.sources_catalog.borrow().clone())
}

#[derive(Serialize)]
struct AudioOutputResponse {
    devices: Vec<crate::audio_output::AudioDevice>,
    current: Option<String>,
}

async fn audio_output_json(State(state): State<AppState>) -> Json<AudioOutputResponse> {
    let devices = match crate::audio_output::list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("audio output list unavailable: {e}");
            Vec::new()
        }
    };
    let current = state.audio_current.read().await.clone();
    Json(AudioOutputResponse { devices, current })
}

#[derive(Deserialize)]
struct AudioOutputRequest {
    device: Option<String>,
}

/// Audio output validation error. Follows the model of `ValidationError`
/// (`ritornello-plugin-radio/src/config.rs`): the user-facing text is
/// produced at the boundary via `message(&Catalog)`, `Display` provides an
/// English version for the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioOutputError {
    EmptyName,
}

impl AudioOutputError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            AudioOutputError::EmptyName => catalog.get("audio_output_name_empty").to_string(),
        }
    }
}

impl std::fmt::Display for AudioOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioOutputError::EmptyName => write!(f, "empty audio output name"),
        }
    }
}

impl std::error::Error for AudioOutputError {}

/// Refuses an empty (or whitespace-only) output name. Pure function, on the
/// model of `theme::validate`: it knows no catalog, the HTTP route resolves
/// the rendered error against the core's.
///
/// The old status page was server-rendered: with no output chosen, no
/// `<option>` carried `selected`, so the browser selected the first device
/// and "Change" always sent a real name. The SPA has no such structural
/// guarantee — hence this core-side validation, which depends on no UI.
/// Without it, on a fresh install, `audio_current` was `Some("")`,
/// `GET /api/audio-output` returned `current: ""` indefinitely, and `""`
/// was passed to mpv then persisted in `state.json`.
pub fn validate_audio_device(device: &str) -> Result<(), AudioOutputError> {
    if device.trim().is_empty() {
        return Err(AudioOutputError::EmptyName);
    }
    Ok(())
}

async fn audio_output_put(State(state): State<AppState>, Json(req): Json<AudioOutputRequest>) -> Response {
    // `null` (or absent) = follow the system default. A named device is
    // validated as before: the empty string stays refused.
    if let Some(device) = &req.device
        && let Err(e) = validate_audio_device(device)
    {
        let msg = e.message(&*state.catalog.read().await);
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    *state.audio_current.write().await = req.device.clone();
    if state.audio_tx.send(req.device).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn settings_json(State(state): State<AppState>) -> Json<crate::state::Settings> {
    Json(state.settings_current.read().await.clone())
}

/// Full replacement: the SPA GETs the struct, edits it, and PUTs it back
/// whole. A field absent from the body falls back to its default (the struct
/// is `serde(default)`), which is the price of reusing the state type — fine
/// on a single-user device.
async fn settings_put(State(state): State<AppState>, Json(req): Json<crate::state::Settings>) -> Response {
    if let Err(e) = validate_settings(&req) {
        let msg = e.message(&*state.catalog.read().await);
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    *state.settings_current.write().await = req.clone();
    if state.settings_tx.send(req).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Web remote: pushes the received command into the same `cmd_tx` channel
/// as the one fed by the Input plugins (no business logic of its own, just
/// one more source of commands). The envelope's `held` flag passes through
/// as is: the core paces held volume commands whatever their origin (see
/// `Core::handle_input`).
async fn command_post(State(state): State<AppState>, Json(msg): Json<ritornello_proto::InputMessage>) -> StatusCode {
    if state.cmd_tx.send(msg).await.is_err() {
        tracing::warn!("web remote: command channel closed");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::NO_CONTENT
}

/// State constructors shared by the tests of `status.rs`, `web.rs` (and
/// beyond): extracted here to spare `web.rs` from redefining them.
/// Mechanical move from `mod tests` below, without any change of content.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::metadata::PlayerState;

    /// Now-playing receiver for the rigs that do not test the SSE stream: the
    /// sender is dropped immediately, so the stream ends after the initial
    /// value. The stream tests go through `app_state_with_now_playing`, which
    /// keeps the sender.
    pub(crate) fn inert_player() -> tokio::sync::watch::Receiver<PlayerState> {
        tokio::sync::watch::channel(PlayerState::default()).1
    }

    /// Rig with the now-playing sender kept, to push changes during an SSE
    /// stream test.
    pub(crate) fn app_state_with_player(
        initial: PlayerState,
    ) -> (AppState, tokio::sync::watch::Sender<PlayerState>) {
        let (tx, rx) = tokio::sync::watch::channel(initial);
        (AppState { player: rx, ..app_state() }, tx)
    }

    pub(crate) fn sample() -> StatusState {
        StatusState {
            plugins: vec![
                PluginStatus::kind("radio", "source", true, true),
                PluginStatus::kind("cd", "source", false, false),
            ],
            active_source: "radio".into(),
        }
    }

    pub(crate) fn app_state() -> AppState {
        let (audio_tx, _audio_rx) = tokio::sync::mpsc::channel(4);
        let (locale_tx, _locale_rx) = tokio::sync::mpsc::channel(4);
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel(4);
        AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
            catalog: Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
                "core",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::i18n::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            locales_root: std::path::PathBuf::from("/nonexistent"),
            admin_backends: Arc::new(Default::default()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: inert_player(),
            sources_catalog: tokio::sync::watch::channel(ritornello_proto::SourcesCatalog::default()).1,
            system: Default::default(),
            covers: Arc::new(crate::cover::CoverCache::new()),
            plugins: Arc::new(PluginsControl {
                manifest: std::path::PathBuf::from("/nonexistent"),
                names: Vec::new(),
                tx: tokio::sync::mpsc::channel(1).0,
            }),
        }
    }

    pub(crate) fn app_state_with_audio() -> (AppState, tokio::sync::mpsc::Receiver<Option<String>>) {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
        let (locale_tx, _locale_rx) = tokio::sync::mpsc::channel(4);
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel(4);
        let state = AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(Some("default".to_string()))),
            audio_tx,
            catalog: Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
                "core",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::i18n::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            locales_root: std::path::PathBuf::from("/nonexistent"),
            admin_backends: Arc::new(Default::default()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: inert_player(),
            sources_catalog: tokio::sync::watch::channel(ritornello_proto::SourcesCatalog::default()).1,
            system: Default::default(),
            covers: Arc::new(crate::cover::CoverCache::new()),
            plugins: Arc::new(PluginsControl {
                manifest: std::path::PathBuf::from("/nonexistent"),
                names: Vec::new(),
                tx: tokio::sync::mpsc::channel(1).0,
            }),
        };
        (state, audio_rx)
    }

    /// Variant with an observable `cmd_tx`, for the web remote tests.
    pub(crate) fn app_state_with_cmd() -> (AppState, tokio::sync::mpsc::Receiver<ritornello_proto::InputMessage>) {
        let (audio_tx, _audio_rx) = tokio::sync::mpsc::channel(4);
        let (locale_tx, _locale_rx) = tokio::sync::mpsc::channel(4);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(4);
        let state = AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
            catalog: Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
                "core",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::i18n::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            locales_root: std::path::PathBuf::from("/nonexistent"),
            admin_backends: Arc::new(Default::default()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: inert_player(),
            sources_catalog: tokio::sync::watch::channel(ritornello_proto::SourcesCatalog::default()).1,
            system: Default::default(),
            covers: Arc::new(crate::cover::CoverCache::new()),
            plugins: Arc::new(PluginsControl {
                manifest: std::path::PathBuf::from("/nonexistent"),
                names: Vec::new(),
                tx: tokio::sync::mpsc::channel(1).0,
            }),
        };
        (state, cmd_rx)
    }

    /// Variant with an observable `locale_tx` and a catalog loaded in `fr`
    /// from a temporary root (the TempDir is returned so it stays alive).
    pub(crate) fn app_state_fr() -> (AppState, tokio::sync::mpsc::Receiver<String>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "active_source_label = \"Source active\"\naudio_output = \"Sortie audio\"\n",
        )
        .unwrap();
        let (audio_tx, _audio_rx) = tokio::sync::mpsc::channel(4);
        let (locale_tx, locale_rx) = tokio::sync::mpsc::channel(4);
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel(4);
        let state = AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
            catalog: Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
                "core",
                "fr",
                dir.path(),
                crate::i18n::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(Some("fr".to_string()))),
            locale_tx,
            locales_root: dir.path().to_path_buf(),
            admin_backends: Arc::new(Default::default()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: inert_player(),
            sources_catalog: tokio::sync::watch::channel(ritornello_proto::SourcesCatalog::default()).1,
            system: Default::default(),
            covers: Arc::new(crate::cover::CoverCache::new()),
            plugins: Arc::new(PluginsControl {
                manifest: std::path::PathBuf::from("/nonexistent"),
                names: Vec::new(),
                tx: tokio::sync::mpsc::channel(1).0,
            }),
        };
        (state, locale_rx, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::settings_validation::SettingsError;
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    /// Variant with an observable `theme_tx`, for the `/api/theme` tests.
    fn app_state_with_theme() -> (AppState, tokio::sync::mpsc::Receiver<crate::theme::ThemeState>) {
        let (state, _audio_rx) = app_state_with_audio();
        let (theme_tx, theme_rx) = tokio::sync::mpsc::channel(4);
        (AppState { theme_tx, ..state }, theme_rx)
    }

    /// Variant with an observable `settings_tx`, for the `/api/settings` tests.
    fn app_state_with_settings() -> (AppState, tokio::sync::mpsc::Receiver<crate::state::Settings>) {
        let (state, _audio_rx) = app_state_with_audio();
        let (settings_tx, settings_rx) = tokio::sync::mpsc::channel(4);
        (AppState { settings_tx, ..state }, settings_rx)
    }

    #[tokio::test]
    async fn put_audio_output_notifies_and_updates_the_displayed_selection() {
        let (state, mut audio_rx) = app_state_with_audio();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/audio-output")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"device":"hw:CARD=Headphones"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(audio_rx.recv().await.unwrap(), Some("hw:CARD=Headphones".to_string()));
    }

    #[tokio::test]
    async fn put_audio_output_null_selects_the_system_default() {
        let (state, mut audio_rx) = app_state_with_audio();
        let audio_current = state.audio_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/audio-output")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"device":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(audio_rx.recv().await.unwrap(), None);
        assert_eq!(*audio_current.read().await, None);
    }

    #[test]
    fn validate_audio_device_refuses_empty_and_blank() {
        assert!(validate_audio_device("hw:CARD=Headphones").is_ok());
        assert!(validate_audio_device("default").is_ok());
        assert!(validate_audio_device("").is_err());
        assert!(validate_audio_device("   ").is_err());
    }

    #[tokio::test]
    async fn put_audio_output_empty_returns_422_and_changes_nothing() {
        // Fresh install: the SPA left the trigger empty and "Change" sent
        // `device: ""`, which the core stored without validation — hence
        // `current: ""` returned indefinitely, `""` passed to mpv, and a
        // success toast. The core now refuses, as `theme_put` does.
        let (state, mut audio_rx) = app_state_with_audio();
        let audio_current = state.audio_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/audio-output")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"device":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // An error message the client can use (`api.put` makes it the toast
        // text), as for `/api/theme`.
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string());
        // The shared state has not moved and nothing went out to mpv.
        assert_eq!(audio_current.read().await.as_deref(), Some("default"));
        assert!(audio_rx.try_recv().is_err(), "nothing must go out on the channel");
    }

    #[tokio::test]
    async fn post_command_relays_a_command_without_argument() {
        let (state, mut cmd_rx) = app_state_with_cmd();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::post("/api/command")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cmd":"VolumeUp"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(cmd_rx.recv().await.unwrap().cmd, ritornello_proto::Command::VolumeUp);
    }

    #[tokio::test]
    async fn post_command_relays_a_command_with_argument() {
        let (state, mut cmd_rx) = app_state_with_cmd();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::post("/api/command")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cmd":"Select","arg":3}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(cmd_rx.recv().await.unwrap().cmd, ritornello_proto::Command::Select(3));
    }

    #[tokio::test]
    async fn post_command_accepts_the_held_flag() {
        let (state, mut cmd_rx) = app_state_with_cmd();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::post("/api/command")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cmd":"VolumeUp","held":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let received = cmd_rx.recv().await.unwrap();
        assert_eq!(received.cmd, ritornello_proto::Command::VolumeUp);
        assert!(received.held);
    }

    #[tokio::test]
    async fn get_audio_output_lists_the_devices_and_the_selection() {
        let (state, _audio_rx) = app_state_with_audio();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/audio-output").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], "default");
        assert!(v["devices"].is_array());
        // Every device is a name/description pair, rather than a bare string.
        if let Some(first) = v["devices"].get(0) {
            assert!(first["name"].is_string());
            assert!(first["description"].is_string());
        }
    }

    struct FakeOccupe;
    #[async_trait::async_trait]
    impl crate::admin::AdminBackend for FakeOccupe {
        async fn asset(&self, _: &str) -> anyhow::Result<Option<(String, String)>> { Ok(None) }
        async fn catalog(&self) -> anyhow::Result<serde_json::Value> { Ok(serde_json::json!({})) }
        async fn get_data(&self) -> anyhow::Result<serde_json::Value> { Ok(serde_json::json!({})) }
        async fn set_data(&self, _: serde_json::Value) -> anyhow::Result<Result<(), String>> { Ok(Ok(())) }
        async fn ping(&self) -> anyhow::Result<()> { Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
    }

    #[tokio::test]
    async fn a_plugin_that_does_not_answer_the_ping_is_busy_in_the_status() {
        let st = app_state();
        st.status.write().await.plugins = vec![
            PluginStatus::kind("files", "source", true, true),
            PluginStatus::kind("radio", "source", true, false),
        ];
        st.admin_backends.write().await.insert("files".into(), Arc::new(FakeOccupe));
        let app = router(st);
        let resp = app.oneshot(Request::get("/api/status").body(Body::empty()).unwrap()).await.unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["plugins"][0]["busy"], serde_json::json!(true));
        // Without an admin page, nothing to probe: the field stays absent, like `stalled`.
        assert!(v["plugins"][1].get("busy").is_none());
    }

    #[tokio::test]
    async fn api_status_lists_the_plugins() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let s: StatusState = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.plugins.len(), 2);
        assert_eq!(s.active_source, "radio");
    }

    /// The web remote tiles read the preset names here: the core already
    /// holds this catalog for the displays, the route only makes it readable
    /// over HTTP. A source that does not enumerate has no `presets` field —
    /// the page then falls back on the numbers alone.
    #[tokio::test]
    async fn api_presets_serves_the_current_catalog() {
        use ritornello_proto::{SourcesCatalog, Preset, SourceCatalog};
        let (tx, rx) = tokio::sync::watch::channel(SourcesCatalog::default());
        let app = router(AppState { sources_catalog: rx, ..app_state() });
        tx.send(SourcesCatalog {
            sources: vec![
                SourceCatalog {
                    name: "radio".into(),
                    presets: vec![Preset { index: 1, name: "FIP".into() }],
                },
                SourceCatalog { name: "cd".into(), presets: vec![] },
            ],
        })
        .unwrap();
        let resp = app.oneshot(Request::get("/api/presets").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["sources"][0]["name"], "radio");
        assert_eq!(v["sources"][0]["presets"][0], serde_json::json!({ "index": 1, "name": "FIP" }));
        assert_eq!(v["sources"][1]["name"], "cd");
        assert_eq!(v["sources"][1].get("presets"), None, "a source that does not enumerate has no presets field");
    }

    #[tokio::test]
    async fn the_old_status_route_is_now_served_by_the_spa() {
        // `/status` remains a valid URL (README, existing links): it now
        // serves the shell, no longer HTML generated by the core.
        let app = router(tests_support::app_state());
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
        assert!(!html.contains("<table"), "the core no longer generates business HTML");
    }

    #[tokio::test]
    async fn the_json_status_carries_the_stalled_flag() {
        // What the UI actually reads: the route, not just the struct.
        let state = app_state();
        state.status.write().await.plugins = vec![
            PluginStatus::unknown_kind("files", true),
            PluginStatus::unknown_kind("cd", false),
        ];
        let app = router(state);
        let resp =
            app.oneshot(Request::get("/api/status").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["plugins"][0]["stalled"], serde_json::json!(true));
        assert_eq!(
            v["plugins"][1].get("stalled"),
            None,
            "a dead plugin is not stalled, and the field must not appear"
        );
    }

    #[tokio::test]
    async fn get_theme_returns_the_defaults_when_nothing_is_persisted() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/theme").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["theme"], "northern-lights");
        assert_eq!(v["mode"], "light");
    }

    #[tokio::test]
    async fn put_theme_notifies_and_updates_the_selection() {
        let (state, mut theme_rx) = app_state_with_theme();
        let theme_current = state.theme_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/theme")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"theme":"cyberpunk","mode":"dark"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let received = theme_rx.recv().await.unwrap();
        assert_eq!(received.theme, "cyberpunk");
        assert_eq!(received.mode, "dark");
        assert_eq!(theme_current.read().await.theme, "cyberpunk");
    }

    #[tokio::test]
    async fn put_theme_invalid_returns_422_and_changes_nothing() {
        let (state, mut theme_rx) = app_state_with_theme();
        let theme_current = state.theme_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/theme")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"theme":"cyberpunk","mode":"system"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(theme_current.read().await.theme, "northern-lights");
        assert!(theme_rx.try_recv().is_err(), "nothing must go out on the channel");
    }

    #[tokio::test]
    async fn get_settings_returns_the_current_values() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/settings").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["volume_repeat_initial_ms"], 800);
        assert_eq!(v["volume_repeat_interval_ms"], 200);
        assert_eq!(v["startup_power"], "on");
        assert_eq!(v["overlay_ms"], 5000);
        assert_eq!(v["tens_window_ms"], 5000);
    }

    #[tokio::test]
    async fn put_settings_notifies_and_updates_the_selection() {
        let (state, mut settings_rx) = app_state_with_settings();
        let settings_current = state.settings_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":250,"startup_power":"previous","overlay_ms":3000,"tens_window_ms":9000}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let received = settings_rx.recv().await.unwrap();
        assert_eq!(received.volume_repeat_initial_ms, 800);
        assert_eq!(received.startup_power, crate::state::StartupPower::Previous);
        assert_eq!(received.overlay_ms, 3000);
        assert_eq!(received.tens_window_ms, 9000);
        assert_eq!(settings_current.read().await.volume_repeat_interval_ms, 250);
        assert_eq!(settings_current.read().await.tens_window_ms, 9000);
    }

    #[tokio::test]
    async fn put_settings_out_of_bounds_returns_422_and_changes_nothing() {
        // Same contract as /api/audio-output and /api/theme: validated before
        // any state change, with an `error` message the SPA turns into a toast.
        let (state, mut settings_rx) = app_state_with_settings();
        let settings_current = state.settings_current.clone();
        let app = router(state);
        for body in [
            r#"{"volume_repeat_initial_ms":100,"volume_repeat_interval_ms":500,"startup_power":"on","overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":1000,"volume_repeat_interval_ms":50,"startup_power":"on","overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":9000,"volume_repeat_interval_ms":500,"startup_power":"on","overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":999,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":15001,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":5000,"tens_window_ms":999}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":5000,"tens_window_ms":15001}"#,
        ] {
            // `AppState` is `Clone`: every oneshot starts from the same rig.
            let resp = app
                .clone()
                .oneshot(
                    Request::put("/api/settings")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(v["error"].is_string());
        }
        assert_eq!(settings_current.read().await.volume_repeat_initial_ms, 800);
        assert_eq!(settings_current.read().await.overlay_ms, 5000);
        assert_eq!(settings_current.read().await.tens_window_ms, 5000);
        assert!(settings_rx.try_recv().is_err(), "nothing must go out on the channel");
    }

    #[test]
    fn validate_audio_device_returns_a_typed_error() {
        assert_eq!(validate_audio_device(""), Err(AudioOutputError::EmptyName));
        assert_eq!(validate_audio_device("   "), Err(AudioOutputError::EmptyName));
    }

    #[test]
    fn audio_output_message_uses_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "audio_output_name_empty = \"nom de sortie vide\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::i18n::EN);
        assert_eq!(AudioOutputError::EmptyName.message(&cat), "nom de sortie vide");
    }

    #[tokio::test]
    async fn an_out_of_bounds_seek_step_is_refused() {
        for (step, valid) in [(0u32, false), (1, true), (10, true), (120, true), (121, false)] {
            let s = crate::state::Settings { seek_step_s: step, ..Default::default() };
            let result = validate_settings(&s);
            assert_eq!(result.is_ok(), valid, "step = {step}");
            // Discriminating: a wrong variant would pass the plain `is_ok`
            // above, and the user would read another bound's message.
            if !valid {
                assert_eq!(result, Err(SettingsError::SeekStep { min: 1, max: 120 }), "step = {step}");
            }
        }
    }

}
