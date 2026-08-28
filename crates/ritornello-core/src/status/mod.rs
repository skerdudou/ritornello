//! La surface HTTP du cœur : `AppState`, le routeur, la sortie audio, les
//! réglages et la route de commande. Un module enfant par sujet — `plugins`,
//! `logs`, `locales`, `settings_validation` — et ce fichier ré-exporte ce
//! que le reste du crate importe, pour qu'aucun import externe ne nomme un enfant.

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
    /// Pages d'admin joignables. Sous verrou : un greffon qui s'announcement en
    /// retard doit voir sa page apparaître sans redémarrage du cœur.
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
    /// État du player (source, volume, muet, veille, track), alimenté par le
    /// cœur. Un `watch` : chaque connexion SSE clone ce récepteur, seule la
    /// dernière valeur compte, et un navigateur lent ne peut pas retenir le cœur.
    pub player: tokio::sync::watch::Receiver<crate::metadata::PlayerState>,
    /// Process-lifetime system facts (start instant, what logind allows),
    /// read by the System tab's endpoints. One `Arc` field rather than
    /// three loose ones: every test constructor below would otherwise grow
    /// by three lines.
    pub system: Arc<crate::system::SystemInfo>,
    /// Pochettes retenues, servies sur `/api/cover/{clé}`. Un `Arc` : la
    /// tâche de téléchargement du cœur y insère, le routeur y read.
    pub covers: Arc<crate::cover::CoverCache>,
    /// Bascule active/inactif des plugins : le manifest à réécrire, les names
    /// acceptés, et l'oreille du cœur.
    pub plugins: Arc<PluginsControl>,
    /// SourcesCatalog des sources et de leurs présélections nommées, tel que le cœur
    /// le diffuse aux afficheurs (`Core::sources_catalog`). Le même `watch` que celui
    /// des plugins Display : la route read la dernière valeur, rien n'est
    /// sondé côté cœur, et la liste ne change qu'à l'announcement ou au départ
    /// d'une source.
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
        .route("/api/command", axum::routing::post(command_post))
        .route("/api/cover/:key", get(crate::cover::cover_get))
        .route(
            "/plugins/:name/api/data",
            get(crate::admin::admin_get_data).put(crate::admin::admin_put_data),
        )
        .route("/plugins/:name/api/i18n", get(crate::admin::admin_i18n))
        .route("/plugins/:name/:fichier", get(crate::admin::admin_asset))
        .route("/api/plugins/:name/enabled", axum::routing::put(plugin_enabled_put))
        .merge(crate::web::routes())
        .fallback(crate::web::shell)
        .with_state(state)
}

async fn status_json(State(state): State<AppState>) -> Json<StatusState> {
    let mut status = state.status.read().await.clone();
    // Toutes les sondes en parallèle : un greffon occupé ne retarde pas la
    // réponse au-delà de son propre budget (500 ms + grâce). Le verrou des
    // dorsaux est relâché avant les allers-retours IPC, comme dans `admin.rs`.
    let dorsaux = state.admin_backends.read().await.clone();
    let sondes = status.plugins.iter().filter(|p| p.admin).map(|p| {
        let dorsal = dorsaux.get(&p.name).cloned();
        let name = p.name.clone();
        async move {
            let occupe = match dorsal {
                Some(d) => matches!(
                    d.ping().await.map_err(|e| e.downcast::<ritornello_plugin_sdk::AdminIpcError>()),
                    Err(Ok(ritornello_plugin_sdk::AdminIpcError::Timeout))
                ),
                None => false,
            };
            (name, occupe)
        }
    });
    let verdicts: std::collections::HashMap<String, bool> =
        futures::future::join_all(sondes).await.into_iter().collect();
    for p in status.plugins.iter_mut() {
        p.busy = verdicts.get(&p.name).copied().unwrap_or(false);
    }
    Json(status)
}

/// Les présélections nommées de chaque source, pour les tuiles de la
/// télécommande web. Une playback de la valeur courante, pas un stream : la page
/// la recharge au changement de source (la trame SSE le lui dit), et c'est
/// assez — voir la spec, décision 6.
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

/// Erreur de validation de la sortie audio. Suit le modèle de
/// `ValidationError` (`ritornello-plugin-radio/src/config.rs`) : le texte
/// utilisateur est produit à la frontière via `message(&Catalog)`, `Display`
/// fournit une version anglaise pour les logs.
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

/// Refuse un name de sortie clear (ou uniquement blanc). Fonction pure, sur le
/// modèle de `theme::validate` : elle ne connaît aucun sources_catalog, c'est la
/// route HTTP qui résout l'erreur rendue contre celui du cœur.
///
/// L'ancienne page de statut était rendue côté serveur : faute de sortie
/// choisie, aucun `<option>` ne portait `selected`, donc le navigateur
/// sélectionnait le premier périphérique et « Changer » envoyait toujours un
/// name réel. La SPA n'a pas cette garantie structurelle — d'où cette
/// validation côté cœur, qui ne dépend d'aucune IHM. Sans elle, sur une
/// installation neuve, `audio_current` valait `Some("")`,
/// `GET /api/audio-output` renvoyait `current: ""` indéfiniment, et `""`
/// était transmis à mpv puis persisté dans `state.json`.
pub fn validate_audio_device(device: &str) -> Result<(), AudioOutputError> {
    if device.trim().is_empty() {
        return Err(AudioOutputError::EmptyName);
    }
    Ok(())
}

async fn audio_output_put(State(state): State<AppState>, Json(req): Json<AudioOutputRequest>) -> Response {
    // `null` (or absent) = follow the system default. A named device is
    // validated as before: the empty string stays refused.
    if let Some(device) = &req.device {
        if let Err(e) = validate_audio_device(device) {
            let msg = e.message(&*state.catalog.read().await);
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
                .into_response();
        }
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

/// Télécommande web : push_cover la commande reçue dans le même canal `cmd_tx`
/// que celui alimenté par les plugins Input (aucune logique métier propre,
/// juste une source de commands supplémentaire). Le drapeau `held` de
/// l'enveloppe traverse tel quel : le cœur cadence les commands de volume
/// maintenues quelle que soit leur origine (voir `Core::handle_input`).
async fn command_post(State(state): State<AppState>, Json(msg): Json<ritornello_proto::InputMessage>) -> StatusCode {
    if state.cmd_tx.send(msg).await.is_err() {
        tracing::warn!("web remote: command channel closed");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::NO_CONTENT
}

/// Constructeurs d'état partagés par les tests de `status.rs`, `web.rs` (et
/// au-delà) : extraits ici pour éviter à `web.rs` de les redéfinir.
/// Déplacement mécanique depuis `mod tests` ci-dessous, sans changement de
/// contenu.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::metadata::PlayerState;

    /// Récepteur de track en cours pour les montages qui ne testent pas le
    /// stream SSE : l'émetteur est lâché aussitôt, donc le stream se terminate après
    /// la valeur initiale. Les tests du stream passent par
    /// `app_state_with_now_playing`, qui garde l'émetteur.
    pub(crate) fn inert_player() -> tokio::sync::watch::Receiver<PlayerState> {
        tokio::sync::watch::channel(PlayerState::default()).1
    }

    /// Rig avec l'émetteur de track en cours conservé, pour pousser des
    /// changements pendant un test du stream SSE.
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

    /// Variante avec un `cmd_tx` observable, pour les tests de la télécommande web.
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

    /// Variante avec un `locale_tx` observable et un sources_catalog chargé en `fr`
    /// depuis une racine temporaire (le TempDir est retourné pour rester vivant).
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

    /// Variante avec un `theme_tx` observable, pour les tests de `/api/theme`.
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
    async fn put_audio_output_notifie_et_met_a_jour_la_selection_affichee() {
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
    async fn put_audio_output_null_choisit_le_defaut_systeme() {
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
    fn validate_audio_device_refuse_le_vide_et_le_blanc() {
        assert!(validate_audio_device("hw:CARD=Headphones").is_ok());
        assert!(validate_audio_device("default").is_ok());
        assert!(validate_audio_device("").is_err());
        assert!(validate_audio_device("   ").is_err());
    }

    #[tokio::test]
    async fn put_audio_output_vide_renvoie_422_et_ne_change_rien() {
        // Installation neuve : la SPA laissait le déclencheur clear et « Changer »
        // envoyait `device: ""`, que le cœur stockait sans validation — d'où
        // `current: ""` renvoyé indéfiniment, `""` transmis à mpv, et un toast
        // de succès. Le cœur refuse maintenant, comme le fait `theme_put`.
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
        // Un message d'erreur exploitable par le client (`api.put` en fait le
        // texte du toast), comme pour `/api/theme`.
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string());
        // L'état partagé n'a pas bougé et rien n'est parti vers mpv.
        assert_eq!(audio_current.read().await.as_deref(), Some("default"));
        assert!(audio_rx.try_recv().is_err(), "rien ne doit partir dans le canal");
    }

    #[tokio::test]
    async fn post_command_relaie_une_commande_sans_argument() {
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
    async fn post_command_relaie_une_commande_avec_argument() {
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
    async fn post_command_accepte_le_drapeau_held() {
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
        let recu = cmd_rx.recv().await.unwrap();
        assert_eq!(recu.cmd, ritornello_proto::Command::VolumeUp);
        assert!(recu.held);
    }

    #[tokio::test]
    async fn get_audio_output_liste_les_peripheriques_et_la_selection() {
        let (state, _audio_rx) = app_state_with_audio();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/audio-output").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], "default");
        assert!(v["devices"].is_array());
        // Chaque périphérique est une paire name/description, plus une chaîne nue.
        if let Some(premier) = v["devices"].get(0) {
            assert!(premier["name"].is_string());
            assert!(premier["description"].is_string());
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
    async fn un_greffon_qui_ne_repond_pas_au_ping_est_occupe_dans_le_statut() {
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
        // Sans page d'admin, rien à sonder : le champ reste absent, comme `stalled`.
        assert!(v["plugins"][1].get("busy").is_none());
    }

    #[tokio::test]
    async fn api_status_liste_les_plugins() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let s: StatusState = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.plugins.len(), 2);
        assert_eq!(s.active_source, "radio");
    }

    /// Les tuiles de la télécommande web lisent ici le name des présélections :
    /// le cœur tient déjà ce sources_catalog pour les afficheurs, la route ne fait
    /// que le rendre lisible en HTTP. Une source qui n'énumère pas n'a pas de
    /// champ `presets` — la page retombe alors sur les numéros seuls.
    #[tokio::test]
    async fn api_presets_sert_le_catalogue_courant() {
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
        assert_eq!(v["sources"][1].get("presets"), None, "une source qui n'énumère pas n'a pas de champ presets");
    }

    #[tokio::test]
    async fn lancienne_route_status_est_desormais_servie_par_la_spa() {
        // `/status` reste une URL valide (README, liens existants) : elle sert
        // maintenant le shell, plus du HTML genere par le coeur.
        let app = router(tests_support::app_state());
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
        assert!(!html.contains("<table"), "le coeur ne genere plus de HTML metier");
    }

    #[tokio::test]
    async fn le_statut_json_porte_le_drapeau_fige() {
        // Ce que l'IHM read réellement : la route, pas seulement la structure.
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
            "un greffon mort n'est pas fige, et le champ ne doit pas apparaitre"
        );
    }

    #[tokio::test]
    async fn get_theme_renvoie_les_defauts_quand_rien_nest_persiste() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/theme").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["theme"], "northern-lights");
        assert_eq!(v["mode"], "light");
    }

    #[tokio::test]
    async fn put_theme_notifie_et_met_a_jour_la_selection() {
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
        let recu = theme_rx.recv().await.unwrap();
        assert_eq!(recu.theme, "cyberpunk");
        assert_eq!(recu.mode, "dark");
        assert_eq!(theme_current.read().await.theme, "cyberpunk");
    }

    #[tokio::test]
    async fn put_theme_invalide_renvoie_422_et_ne_change_rien() {
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
        assert!(theme_rx.try_recv().is_err(), "rien ne doit partir dans le canal");
    }

    #[tokio::test]
    async fn get_settings_renvoie_les_valeurs_courantes() {
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
    async fn put_settings_notifie_et_met_a_jour_la_selection() {
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
        let recu = settings_rx.recv().await.unwrap();
        assert_eq!(recu.volume_repeat_initial_ms, 800);
        assert_eq!(recu.startup_power, crate::state::StartupPower::Previous);
        assert_eq!(recu.overlay_ms, 3000);
        assert_eq!(recu.tens_window_ms, 9000);
        assert_eq!(settings_current.read().await.volume_repeat_interval_ms, 250);
        assert_eq!(settings_current.read().await.tens_window_ms, 9000);
    }

    #[tokio::test]
    async fn put_settings_hors_bornes_renvoie_422_et_ne_change_rien() {
        // Same contract as /api/audio-output and /api/theme: validated before
        // any state change, with an `error` message the SPA turns into a toast.
        let (state, mut settings_rx) = app_state_with_settings();
        let settings_current = state.settings_current.clone();
        let app = router(state);
        for corps in [
            r#"{"volume_repeat_initial_ms":100,"volume_repeat_interval_ms":500,"startup_power":"on","overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":1000,"volume_repeat_interval_ms":50,"startup_power":"on","overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":9000,"volume_repeat_interval_ms":500,"startup_power":"on","overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":999,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":15001,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":5000,"tens_window_ms":999}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"startup_power":"on","overlay_ms":5000,"tens_window_ms":15001}"#,
        ] {
            // `AppState` est `Clone` : chaque oneshot repart du même montage.
            let resp = app
                .clone()
                .oneshot(
                    Request::put("/api/settings")
                        .header("content-type", "application/json")
                        .body(Body::from(corps))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{corps}");
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(v["error"].is_string());
        }
        assert_eq!(settings_current.read().await.volume_repeat_initial_ms, 800);
        assert_eq!(settings_current.read().await.overlay_ms, 5000);
        assert_eq!(settings_current.read().await.tens_window_ms, 5000);
        assert!(settings_rx.try_recv().is_err(), "rien ne doit partir dans le canal");
    }

    #[test]
    fn validate_audio_device_rend_une_erreur_typee() {
        assert_eq!(validate_audio_device(""), Err(AudioOutputError::EmptyName));
        assert_eq!(validate_audio_device("   "), Err(AudioOutputError::EmptyName));
    }

    #[test]
    fn message_audio_output_utilise_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "audio_output_name_empty = \"name de sortie clear\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::i18n::EN);
        assert_eq!(AudioOutputError::EmptyName.message(&cat), "name de sortie clear");
    }

    #[tokio::test]
    async fn le_pas_de_deplacement_hors_bornes_est_refuse() {
        for (pas, valide) in [(0u32, false), (1, true), (10, true), (120, true), (121, false)] {
            let s = crate::state::Settings { seek_step_s: pas, ..Default::default() };
            let resultat = validate_settings(&s);
            assert_eq!(resultat.is_ok(), valide, "pas = {pas}");
            // Discriminant : une mauvaise variante passerait le simple `is_ok`
            // ci-dessus, et l'utilisateur lirait le message d'une autre bounded.
            if !valide {
                assert_eq!(resultat, Err(SettingsError::SeekStep { min: 1, max: 120 }), "pas = {pas}");
            }
        }
    }

}
