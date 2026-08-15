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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    pub name: String,
    pub kind: String,
    pub connected: bool,
    pub admin: bool,
}

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
    pub admin_backends: Arc<std::collections::HashMap<String, Arc<dyn crate::admin::AdminBackend>>>,
    pub admin_assets: Arc<crate::admin::AssetCache>,
    pub cmd_tx: mpsc::Sender<ritornello_proto::InputMessage>,
    pub theme_current: Arc<RwLock<crate::theme::ThemeState>>,
    pub theme_tx: mpsc::Sender<crate::theme::ThemeState>,
    /// Behavior settings shown on the config page. Same pattern as
    /// `theme_current`/`theme_tx`: the HTTP layer validates and updates the
    /// shared copy, the channel carries the change to the core loop.
    pub settings_current: Arc<RwLock<crate::state::Settings>>,
    pub settings_tx: mpsc::Sender<crate::state::Settings>,
    /// État du lecteur (source, volume, muet, veille, morceau), alimenté par le
    /// cœur. Un `watch` : chaque connexion SSE clone ce récepteur, seule la
    /// dernière valeur compte, et un navigateur lent ne peut pas retenir le cœur.
    pub player: tokio::sync::watch::Receiver<crate::metadata::PlayerState>,
    /// Process-lifetime system facts (start instant, what logind allows),
    /// read by the System tab's endpoints. One `Arc` field rather than
    /// three loose ones: every test constructor below would otherwise grow
    /// by three lines.
    pub system: Arc<crate::system::SystemInfo>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status_json))
        .route("/api/audio-output", get(audio_output_json).put(audio_output_put))
        .route("/api/locale", get(locale_json).put(locale_put))
        .route("/api/i18n", get(i18n_json))
        .route("/api/logs", get(logs_json))
        .route("/api/player", get(player_sse))
        .route("/api/theme", get(crate::theme::theme_json).put(crate::theme::theme_put))
        .route("/api/settings", get(settings_json).put(settings_put))
        .route("/api/system", get(crate::system::system_json))
        .route("/api/system/power", axum::routing::post(crate::system::power_post))
        .route("/api/command", axum::routing::post(command_post))
        .route(
            "/plugins/:name/api/data",
            get(crate::admin::admin_get_data).put(crate::admin::admin_put_data),
        )
        .route("/plugins/:name/api/i18n", get(crate::admin::admin_i18n))
        .route("/plugins/:name/:fichier", get(crate::admin::admin_asset))
        .merge(crate::web::routes())
        .fallback(crate::web::shell)
        .with_state(state)
}

async fn status_json(State(state): State<AppState>) -> Json<StatusState> {
    Json(state.status.read().await.clone())
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
/// fournit une version anglaise pour les journaux.
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

/// Refuse un nom de sortie vide (ou uniquement blanc). Fonction pure, sur le
/// modèle de `theme::validate` : elle ne connaît aucun catalogue, c'est la
/// route HTTP qui résout l'erreur rendue contre celui du cœur.
///
/// L'ancienne page de statut était rendue côté serveur : faute de sortie
/// choisie, aucun `<option>` ne portait `selected`, donc le navigateur
/// sélectionnait le premier périphérique et « Changer » envoyait toujours un
/// nom réel. La SPA n'a pas cette garantie structurelle — d'où cette
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

/// Bornes des quatre réglages, définies une seule fois et prises à la
/// comparaison elle-même : `SettingsError` les reporte telles quelles dans
/// ses paramètres, pour qu'un changement de borne ne puisse plus laisser un
/// message qui mente sur ses propres limites.
const INITIAL_DELAY_MS: std::ops::RangeInclusive<u32> = 200..=5000;
const REPEAT_INTERVAL_MS: std::ops::RangeInclusive<u32> = 100..=2000;
// Same bounds for both overlay durations: under a second an overlay is
// unreadable and the tens-offset capture becomes impractical (it takes two
// presses inside the window); past roughly fifteen seconds an overlay
// durably hides the "now playing" view.
const OVERLAY_MS: std::ops::RangeInclusive<u32> = 1000..=15000;
const TENS_WINDOW_MS: std::ops::RangeInclusive<u32> = 1000..=15000;

/// Erreur de validation des réglages, une variante par borne violée. Même
/// modèle que `AudioOutputError` : les paramètres `min`/`max` viennent de la
/// borne effectivement comparée, jamais recopiés à la main.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    InitialDelay { min: u32, max: u32 },
    RepeatInterval { min: u32, max: u32 },
    Overlay { min: u32, max: u32 },
    TensWindow { min: u32, max: u32 },
}

impl SettingsError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            SettingsError::InitialDelay { min, max } => catalog
                .get("settings_initial_delay_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::RepeatInterval { min, max } => catalog
                .get("settings_repeat_interval_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::Overlay { min, max } => catalog
                .get("settings_overlay_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::TensWindow { min, max } => catalog
                .get("settings_tens_window_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
        }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::InitialDelay { min, max } => {
                write!(f, "initial delay out of range ({min}-{max} ms)")
            }
            SettingsError::RepeatInterval { min, max } => {
                write!(f, "repeat interval out of range ({min}-{max} ms)")
            }
            SettingsError::Overlay { min, max } => {
                write!(f, "overlay duration out of range ({min}-{max} ms)")
            }
            SettingsError::TensWindow { min, max } => {
                write!(f, "tens-offset entry window out of range ({min}-{max} ms)")
            }
        }
    }
}

impl std::error::Error for SettingsError {}

/// Bounds for the hold-to-repeat timings. Pure function, same model as
/// `validate_audio_device`: the core itself accepts anything (tests use tiny
/// timings), the HTTP surface is where user input is checked.
pub fn validate_settings(s: &crate::state::Settings) -> Result<(), SettingsError> {
    if !INITIAL_DELAY_MS.contains(&s.volume_repeat_initial_ms) {
        return Err(SettingsError::InitialDelay {
            min: *INITIAL_DELAY_MS.start(),
            max: *INITIAL_DELAY_MS.end(),
        });
    }
    if !REPEAT_INTERVAL_MS.contains(&s.volume_repeat_interval_ms) {
        return Err(SettingsError::RepeatInterval {
            min: *REPEAT_INTERVAL_MS.start(),
            max: *REPEAT_INTERVAL_MS.end(),
        });
    }
    if !OVERLAY_MS.contains(&s.overlay_ms) {
        return Err(SettingsError::Overlay { min: *OVERLAY_MS.start(), max: *OVERLAY_MS.end() });
    }
    if !TENS_WINDOW_MS.contains(&s.tens_window_ms) {
        return Err(SettingsError::TensWindow {
            min: *TENS_WINDOW_MS.start(),
            max: *TENS_WINDOW_MS.end(),
        });
    }
    Ok(())
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

/// Noms de langues disponibles à partir des noms de fichiers d'un répertoire
/// `core/` : `en` (toujours) + chaque `<lang>.toml`. Fonction pure, testable,
/// séparée de l'accès disque (comme `audio_output::parse_device_list`).
pub fn parse_available_locales(filenames: &[String]) -> Vec<String> {
    let mut out = vec!["en".to_string()];
    for f in filenames {
        if let Some(stem) = f.strip_suffix(".toml") {
            if stem != "en" && !out.iter().any(|x| x == stem) {
                out.push(stem.to_string());
            }
        }
    }
    out
}

/// Marque le plugin `name` comme déconnecté dans l'état de statut : un plugin
/// dont le processus s'est terminé n'est plus joignable (supervision, page de
/// statut vivante). No-op si le nom est inconnu.
pub fn mark_plugin_disconnected(state: &mut StatusState, name: &str) {
    for p in &mut state.plugins {
        if p.name == name {
            p.connected = false;
        }
    }
}

/// Langues du cœur = `en` + les packs `<root>/core/*.toml` présents.
pub fn list_locales(root: &std::path::Path) -> Vec<String> {
    let names: Vec<String> = std::fs::read_dir(root.join("core"))
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).collect()
        })
        .unwrap_or_default();
    parse_available_locales(&names)
}

#[derive(Serialize)]
struct LocaleResponse {
    locales: Vec<String>,
    current: Option<String>,
}

async fn locale_json(State(state): State<AppState>) -> Json<LocaleResponse> {
    let locales = list_locales(&state.locales_root);
    let current = state.locale_current.read().await.clone();
    Json(LocaleResponse { locales, current })
}

#[derive(Deserialize)]
struct LocaleRequest {
    locale: String,
}

/// Forme d'un code de langue acceptable : ce que produisent les noms de
/// fichiers `<lang>.toml` des packs (`fr`, `en`, `pt-BR`…).
///
/// La valeur finit dans des chemins de fichiers (`<root>/<composant>/<lang>.toml`
/// via `Catalog::load`), dans `state.json` et en variable d'environnement des
/// plugins : même rigueur que pour le thème et la sortie audio, qui sont
/// validés — une chaîne arbitraire ouvrait une traversée de chemin
/// (`{"locale":"../../nimporte/quoi"}`) sur une API non authentifiée.
fn locale_valide(locale: &str) -> bool {
    !locale.is_empty()
        && locale.len() <= 16
        && locale.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn locale_put(State(state): State<AppState>, Json(req): Json<LocaleRequest>) -> StatusCode {
    if !locale_valide(&req.locale) {
        return StatusCode::BAD_REQUEST;
    }
    *state.locale_current.write().await = Some(req.locale.clone());
    if state.locale_tx.send(req.locale).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::NO_CONTENT
}

/// Catalogue du cœur dans la langue courante, à plat, pour le `t()` de la SPA.
async fn i18n_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cat = state.catalog.read().await;
    Json(serde_json::json!(cat.entries()))
}

/// Télécommande web : pousse la commande reçue dans le même canal `cmd_tx`
/// que celui alimenté par les plugins Input (aucune logique métier propre,
/// juste une source de commandes supplémentaire). Le drapeau `held` de
/// l'enveloppe traverse tel quel : le cœur cadence les commandes de volume
/// maintenues quelle que soit leur origine (voir `Core::handle_input`).
async fn command_post(State(state): State<AppState>, Json(msg): Json<ritornello_proto::InputMessage>) -> StatusCode {
    if state.cmd_tx.send(msg).await.is_err() {
        tracing::warn!("web remote: command channel closed");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::NO_CONTENT
}

#[derive(Serialize)]
struct LogsResponse {
    lines: Vec<String>,
}

/// Les dernières lignes WARN/ERROR, les plus récentes en premier — c'est
/// l'ordre dans lequel l'ancienne page de statut les affichait.
async fn logs_json(State(state): State<AppState>) -> Json<LogsResponse> {
    let mut lines = state.logs.snapshot();
    lines.reverse();
    Json(LogsResponse { lines })
}

/// État du lecteur en flux poussé (`text/event-stream`) : source active, volume,
/// muet, veille, et le morceau quand on le connaît.
///
/// Tout ce qui est **volatil** passe ici, et rien d'autre : c'est la raison pour
/// laquelle le volume n'est exposé par aucune route sondée. `/api/status` porte à
/// côté le contrat de navigation (quels plugins existent, lesquels ont une page
/// d'admin), structurellement stable et lu une fois au montage.
///
/// Poussé et non sondé, pour trois raisons mesurées avant de trancher : la SPA
/// ne sonde rien aujourd'hui (aucun `setInterval`, aucun WebSocket) ; le cœur
/// diffuse **déjà** ses changements sur un canal `watch`, donc la route ne
/// coûte que quelques lignes et n'ajoute aucun état ; et un appareil le plus
/// souvent inactif n'a pas à recevoir des requêtes qui n'apprennent rien.
/// Corollaire utile : le volume affiché suit la télécommande infrarouge et les
/// autres onglets, ce qu'un sondage n'aurait donné qu'avec un intervalle de
/// retard.
///
/// L'état courant est émis **dès la connexion** — même propriété que le flux
/// d'OUI FM qu'on consomme par ailleurs : un onglet ouvert au milieu d'un
/// morceau ne doit pas rester vide jusqu'au suivant.
///
/// Pas d'authentification, comme toutes les autres routes de l'appareil : en
/// ajouter ici seulement donnerait l'illusion d'une protection alors que
/// `/api/command` pilote déjà la lecture sans en demander.
async fn player_sse(
    State(state): State<AppState>,
) -> axum::response::Sse<impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>
{
    use futures::StreamExt;

    let flux = futures::stream::unfold((state.player.clone(), true), |(mut rx, premier)| async move {
        if premier {
            // `borrow_and_update` marque la valeur comme vue : le prochain
            // `changed()` attendra un vrai changement au lieu de renvoyer
            // aussitôt l'état déjà émis.
            let etat = rx.borrow_and_update().clone();
            return Some((etat, (rx, false)));
        }
        // Err = le cœur a lâché l'émetteur : fin du flux, le navigateur
        // reconnectera de lui-même (`EventSource` s'en charge).
        rx.changed().await.ok()?;
        let etat = rx.borrow_and_update().clone();
        Some((etat, (rx, false)))
    })
    .map(|etat| {
        // La sérialisation d'un `PlayerState` ne peut pas échouer (que des
        // types simples) ; en cas d'imprévu, un objet vide vaut mieux qu'une
        // connexion coupée, que le client interpréterait comme une panne.
        Ok(axum::response::sse::Event::default()
            .json_data(&etat)
            .unwrap_or_else(|_| axum::response::sse::Event::default().data("{}")))
    });

    axum::response::Sse::new(flux).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Tampon circulaire des dernières lignes de log (WARN/ERROR), affiché sur
/// la page de statut. `LogBufferWriter` (ci-dessous) y pousse les lignes
/// depuis une couche `tracing` installée dans `main`.
#[derive(Debug)]
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { lines: Mutex::new(VecDeque::with_capacity(capacity)), capacity }
    }

    pub fn push(&self, line: String) {
        let mut lines = self.lines.lock().unwrap();
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }
}

/// Adaptateur `io::Write` pour brancher `LogBuffer` comme sortie d'une
/// couche `tracing_subscriber::fmt::layer()` (voir Task 8).
pub struct LogBufferWriter(pub Arc<LogBuffer>);

impl std::io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(s) = std::str::from_utf8(buf) {
            let line = s.trim_end();
            if !line.is_empty() {
                self.0.push(line.to_string());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Constructeurs d'état partagés par les tests de `status.rs`, `web.rs` (et
/// au-delà) : extraits ici pour éviter à `web.rs` de les redéfinir.
/// Déplacement mécanique depuis `mod tests` ci-dessous, sans changement de
/// contenu.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::metadata::PlayerState;

    /// Récepteur de morceau en cours pour les montages qui ne testent pas le
    /// flux SSE : l'émetteur est lâché aussitôt, donc le flux se termine après
    /// la valeur initiale. Les tests du flux passent par
    /// `app_state_with_now_playing`, qui garde l'émetteur.
    pub(crate) fn player_inerte() -> tokio::sync::watch::Receiver<PlayerState> {
        tokio::sync::watch::channel(PlayerState::default()).1
    }

    /// Montage avec l'émetteur de morceau en cours conservé, pour pousser des
    /// changements pendant un test du flux SSE.
    pub(crate) fn app_state_with_player(
        initial: PlayerState,
    ) -> (AppState, tokio::sync::watch::Sender<PlayerState>) {
        let (tx, rx) = tokio::sync::watch::channel(initial);
        (AppState { player: rx, ..app_state() }, tx)
    }

    pub(crate) fn sample() -> StatusState {
        StatusState {
            plugins: vec![
                PluginStatus { name: "radio".into(), kind: "source".into(), connected: true, admin: true },
                PluginStatus { name: "cd".into(), kind: "source".into(), connected: false, admin: false },
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
                crate::core::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            locales_root: std::path::PathBuf::from("/nonexistent"),
            admin_backends: Arc::new(std::collections::HashMap::new()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: player_inerte(),
            system: Default::default(),
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
                crate::core::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            locales_root: std::path::PathBuf::from("/nonexistent"),
            admin_backends: Arc::new(std::collections::HashMap::new()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: player_inerte(),
            system: Default::default(),
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
                crate::core::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            locales_root: std::path::PathBuf::from("/nonexistent"),
            admin_backends: Arc::new(std::collections::HashMap::new()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: player_inerte(),
            system: Default::default(),
        };
        (state, cmd_rx)
    }

    /// Variante avec un `locale_tx` observable et un catalogue chargé en `fr`
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
                crate::core::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(Some("fr".to_string()))),
            locale_tx,
            locales_root: dir.path().to_path_buf(),
            admin_backends: Arc::new(std::collections::HashMap::new()),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            player: player_inerte(),
            system: Default::default(),
        };
        (state, locale_rx, dir)
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn parse_available_locales_prefixe_en_et_deduplique() {
        let noms = vec!["fr.toml".to_string(), "en.toml".to_string(), "README.md".to_string()];
        assert_eq!(parse_available_locales(&noms), vec!["en".to_string(), "fr".to_string()]);
    }

    #[tokio::test]
    async fn get_locale_liste_en_et_les_packs_core() {
        let (state, _rx, _dir) = app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], "fr");
        let locales: Vec<String> = serde_json::from_value(v["locales"].clone()).unwrap();
        assert!(locales.contains(&"en".to_string()));
        assert!(locales.contains(&"fr".to_string()));
    }

    #[tokio::test]
    async fn put_locale_notifie_et_met_a_jour_la_selection() {
        let (state, mut locale_rx, _dir) = app_state_fr();
        let locale_current = state.locale_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"fr"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(locale_rx.recv().await.unwrap(), "fr");
        assert_eq!(locale_current.read().await.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn put_locale_refuse_une_valeur_qui_nest_pas_un_code_de_langue() {
        // Régression (revue 2026-07-27) : la valeur finit dans des chemins de
        // fichiers, dans state.json et en variable d'environnement des
        // plugins ; `../../x` doit être refusé **avant** toute mise à jour,
        // comme le thème et la sortie audio le font déjà pour leurs champs.
        let (state, mut locale_rx, _dir) = app_state_fr();
        let locale_current = state.locale_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"../../var/lib/quelconque"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // Ni notifiée, ni retenue comme sélection courante.
        assert!(locale_rx.try_recv().is_err());
        assert_eq!(locale_current.read().await.as_deref(), Some("fr"));
    }

    #[test]
    fn locale_valide_accepte_les_codes_et_refuse_le_reste() {
        for ok in ["en", "fr", "pt-BR", "zh_Hant", "fr-CA"] {
            assert!(locale_valide(ok), "{ok} devrait passer");
        }
        for ko in ["", "..", "../fr", "fr/..", "fr toml", "a".repeat(17).as_str()] {
            assert!(!locale_valide(ko), "{ko:?} devrait être refusé");
        }
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
        // Installation neuve : la SPA laissait le déclencheur vide et « Changer »
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
        // Chaque périphérique est une paire nom/description, plus une chaîne nue.
        if let Some(premier) = v["devices"].get(0) {
            assert!(premier["name"].is_string());
            assert!(premier["description"].is_string());
        }
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

    #[tokio::test]
    async fn api_logs_renvoie_les_lignes_les_plus_recentes_en_premier() {
        let state = tests_support::app_state();
        state.logs.push("WARN premiere".into());
        state.logs.push("WARN seconde".into());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/logs").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lignes: Vec<String> = serde_json::from_value(v["lines"].clone()).unwrap();
        // Ordre inverse, comme le faisait la page rendue cote serveur.
        assert_eq!(lignes, vec!["WARN seconde".to_string(), "WARN premiere".to_string()]);
    }

    /// Lit la prochaine trame SSE d'un corps de réponse.
    ///
    /// Le flux est **infini** : un `collect()` sur le corps ne rendrait jamais
    /// la main. On lit donc morceau par morceau, en accumulant jusqu'à une
    /// trame complète (terminée par la ligne vide qui sépare les événements
    /// SSE), et on renvoie la charge utile de la ligne `data:`.
    async fn prochaine_trame(corps: &mut axum::body::BodyDataStream) -> serde_json::Value {
        use futures::StreamExt;
        let mut tampon = String::new();
        for _ in 0..50 {
            let Some(chunk) = corps.next().await else { panic!("flux termine avant la trame") };
            tampon.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
            if let Some(data) = tampon.lines().find_map(|l| l.strip_prefix("data:")) {
                if tampon.contains("\n\n") {
                    return serde_json::from_str(data.trim()).expect("charge utile JSON");
                }
            }
        }
        panic!("aucune trame complete recue : {tampon:?}");
    }

    fn etat(titre: &str) -> crate::metadata::PlayerState {
        crate::metadata::PlayerState {
            source: "radio".into(),
            volume: 60,
            morceau: crate::metadata::Morceau {
                title: Some(titre.into()),
                origin: Some("icy".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn player_emet_letat_courant_des_la_connexion() {
        // Propriété reprise du flux d'OUI FM : un onglet ouvert au milieu d'un
        // morceau ne doit pas rester vide jusqu'au suivant.
        let (state, _tx) = tests_support::app_state_with_player(etat("Miles Davis - So What"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/player").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap().to_str().unwrap(),
            "text/event-stream"
        );
        let mut corps = resp.into_body().into_data_stream();
        let v = prochaine_trame(&mut corps).await;
        assert_eq!(v["title"], "Miles Davis - So What");
        assert_eq!(v["source"], "radio");
        assert_eq!(v["origin"], "icy");
    }

    #[tokio::test]
    async fn player_pousse_les_changements_suivants() {
        let (state, tx) = tests_support::app_state_with_player(etat("premier"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/player").body(Body::empty()).unwrap()).await.unwrap();
        let mut corps = resp.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps).await["title"], "premier");
        tx.send(etat("second")).unwrap();
        assert_eq!(prochaine_trame(&mut corps).await["title"], "second");
    }

    #[tokio::test]
    async fn deux_clients_recoivent_tous_les_deux() {
        let (state, tx) = tests_support::app_state_with_player(etat("premier"));
        let un = router(state.clone())
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let deux = router(state)
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut corps_un = un.into_body().into_data_stream();
        let mut corps_deux = deux.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps_un).await["title"], "premier");
        assert_eq!(prochaine_trame(&mut corps_deux).await["title"], "premier");
        tx.send(etat("second")).unwrap();
        assert_eq!(prochaine_trame(&mut corps_un).await["title"], "second");
        assert_eq!(prochaine_trame(&mut corps_deux).await["title"], "second");
    }

    #[tokio::test]
    async fn un_client_qui_se_deconnecte_ne_perturbe_ni_le_canal_ni_les_autres() {
        let (state, tx) = tests_support::app_state_with_player(etat("premier"));
        let survivant = router(state.clone())
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut corps_survivant = survivant.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps_survivant).await["title"], "premier");

        {
            let parti = router(state)
                .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let mut corps = parti.into_body().into_data_stream();
            prochaine_trame(&mut corps).await;
            // Fin de portée : le corps est lâché, comme un onglet fermé.
        }

        // L'émission continue de fonctionner, et l'autre client la reçoit.
        assert!(tx.send(etat("second")).is_ok(), "le canal ne doit pas etre casse");
        assert_eq!(prochaine_trame(&mut corps_survivant).await["title"], "second");
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

    #[test]
    fn log_buffer_plafonne_a_50_lignes() {
        let buf = LogBuffer::new(50);
        for i in 0..60 {
            buf.push(format!("ligne {i}"));
        }
        let lines = buf.snapshot();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "ligne 10"); // les 10 plus anciennes ont ete evincees
        assert_eq!(lines[49], "ligne 59");
    }

    #[test]
    fn log_buffer_writer_pousse_les_lignes_completes() {
        use std::io::Write;
        let buf = Arc::new(LogBuffer::new(10));
        let mut w = LogBufferWriter(buf.clone());
        writeln!(w, "WARN plugin radio indisponible").unwrap();
        assert_eq!(buf.snapshot(), vec!["WARN plugin radio indisponible".to_string()]);
    }

    #[test]
    fn mark_plugin_disconnected_bascule_connected() {
        let mut st = StatusState {
            plugins: vec![
                PluginStatus { name: "radio".into(), kind: "source".into(), connected: true, admin: true },
                PluginStatus { name: "cd".into(), kind: "source".into(), connected: true, admin: false },
            ],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "cd");
        assert!(!st.plugins.iter().find(|p| p.name == "cd").unwrap().connected);
        assert!(st.plugins.iter().find(|p| p.name == "radio").unwrap().connected);
        // Nom inconnu : no-op, ne panique pas.
        mark_plugin_disconnected(&mut st, "inconnu");
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
    async fn api_i18n_renvoie_le_catalogue_a_plat() {
        let app = router(tests_support::app_state());
        let resp = app.oneshot(Request::get("/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // L'anglais embarque du coeur porte ces cles (src/locales/en.toml).
        assert!(v["remote_title"].is_string());
        assert!(v["audio_output"].is_string());
    }

    #[tokio::test]
    async fn api_i18n_suit_la_langue_courante() {
        let (state, _rx, _dir) = tests_support::app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["audio_output"], "Sortie audio");
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
        assert_eq!(v["start_in_standby"], false);
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
                        r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":250,"start_in_standby":true,"overlay_ms":3000,"tens_window_ms":9000}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let recu = settings_rx.recv().await.unwrap();
        assert_eq!(recu.volume_repeat_initial_ms, 800);
        assert!(recu.start_in_standby);
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
            r#"{"volume_repeat_initial_ms":100,"volume_repeat_interval_ms":500,"start_in_standby":false,"overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":1000,"volume_repeat_interval_ms":50,"start_in_standby":false,"overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":9000,"volume_repeat_interval_ms":500,"start_in_standby":false,"overlay_ms":5000,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"start_in_standby":false,"overlay_ms":999,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"start_in_standby":false,"overlay_ms":15001,"tens_window_ms":5000}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"start_in_standby":false,"overlay_ms":5000,"tens_window_ms":999}"#,
            r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":200,"start_in_standby":false,"overlay_ms":5000,"tens_window_ms":15001}"#,
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
    fn validate_settings_borne_les_deux_delais() {
        use crate::state::Settings;
        assert!(validate_settings(&Settings::default()).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 200, volume_repeat_interval_ms: 100, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 5000, volume_repeat_interval_ms: 2000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 199, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 5001, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_interval_ms: 99, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_interval_ms: 2001, ..Default::default() }).is_err());
    }

    #[test]
    fn validate_settings_borne_les_deux_durees_dincrustation() {
        use crate::state::Settings;
        assert!(validate_settings(&Settings { overlay_ms: 1000, tens_window_ms: 1000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { overlay_ms: 15000, tens_window_ms: 15000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { overlay_ms: 999, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { overlay_ms: 15001, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { tens_window_ms: 999, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { tens_window_ms: 15001, ..Default::default() }).is_err());
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
            "audio_output_name_empty = \"nom de sortie vide\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::core::EN);
        assert_eq!(AudioOutputError::EmptyName.message(&cat), "nom de sortie vide");
    }

    #[test]
    fn validate_settings_rend_la_bonne_variante_avec_ses_bornes() {
        use crate::state::Settings;
        assert_eq!(
            validate_settings(&Settings { volume_repeat_initial_ms: 1, ..Default::default() }),
            Err(SettingsError::InitialDelay { min: 200, max: 5000 })
        );
        assert_eq!(
            validate_settings(&Settings { volume_repeat_interval_ms: 1, ..Default::default() }),
            Err(SettingsError::RepeatInterval { min: 100, max: 2000 })
        );
        assert_eq!(
            validate_settings(&Settings { overlay_ms: 1, ..Default::default() }),
            Err(SettingsError::Overlay { min: 1000, max: 15000 })
        );
        assert_eq!(
            validate_settings(&Settings { tens_window_ms: 1, ..Default::default() }),
            Err(SettingsError::TensWindow { min: 1000, max: 15000 })
        );
    }

    #[test]
    fn message_settings_interpole_les_bornes_contre_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "settings_initial_delay_out_of_range = \"delai hors bornes ({min}-{max})\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::core::EN);
        let err = SettingsError::InitialDelay { min: 200, max: 5000 };
        assert_eq!(err.message(&cat), "delai hors bornes (200-5000)");
    }
}
