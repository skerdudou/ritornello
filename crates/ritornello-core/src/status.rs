use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub name: String,
    pub kind: String,
    pub connected: bool,
    pub admin: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusState {
    pub plugins: Vec<PluginStatus>,
    pub active_source: String,
}

impl<'de> serde::Deserialize<'de> for StatusState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            plugins: Vec<RawPlugin>,
            active_source: String,
        }
        #[derive(serde::Deserialize)]
        struct RawPlugin {
            name: String,
            kind: String,
            connected: bool,
            admin: bool,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(StatusState {
            plugins: raw
                .plugins
                .into_iter()
                .map(|p| PluginStatus { name: p.name, kind: p.kind, connected: p.connected, admin: p.admin })
                .collect(),
            active_source: raw.active_source,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub status: Arc<RwLock<StatusState>>,
    pub logs: Arc<LogBuffer>,
    pub audio_current: Arc<RwLock<Option<String>>>,
    pub audio_tx: mpsc::Sender<String>,
    pub catalog: Arc<RwLock<ritornello_i18n::Catalog>>,
    pub locale_current: Arc<RwLock<Option<String>>>,
    pub locale_tx: mpsc::Sender<String>,
    pub locales_root: std::path::PathBuf,
    pub admin_backends: Arc<std::collections::HashMap<String, Arc<dyn crate::admin::AdminBackend>>>,
    pub admin_assets: Arc<crate::admin::AssetCache>,
    pub cmd_tx: mpsc::Sender<ritornello_proto::Command>,
    pub theme_current: Arc<RwLock<crate::theme::ThemeState>>,
    pub theme_tx: mpsc::Sender<crate::theme::ThemeState>,
    /// Morceau en cours, alimenté par le cœur. Un `watch` : chaque connexion
    /// SSE clone ce récepteur, seule la dernière valeur compte, et un
    /// navigateur lent ne peut pas retenir le cœur.
    pub now_playing: tokio::sync::watch::Receiver<crate::metadata::NowPlayingState>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status_json))
        .route("/api/audio-output", get(audio_output_json).put(audio_output_put))
        .route("/api/locale", get(locale_json).put(locale_put))
        .route("/api/i18n", get(i18n_json))
        .route("/api/logs", get(logs_json))
        .route("/api/now-playing", get(now_playing_sse))
        .route("/api/theme", get(crate::theme::theme_json).put(crate::theme::theme_put))
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
    devices: Vec<String>,
    current: Option<String>,
}

async fn audio_output_json(State(state): State<AppState>) -> Json<AudioOutputResponse> {
    let devices = match crate::audio_output::list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("liste des sorties audio indisponible: {e}");
            Vec::new()
        }
    };
    let current = state.audio_current.read().await.clone();
    Json(AudioOutputResponse { devices, current })
}

#[derive(Deserialize)]
struct AudioOutputRequest {
    device: String,
}

/// Refuse un nom de sortie vide (ou uniquement blanc). Fonction pure, sur le
/// modèle de `theme::validate`.
///
/// L'ancienne page de statut était rendue côté serveur : faute de sortie
/// choisie, aucun `<option>` ne portait `selected`, donc le navigateur
/// sélectionnait le premier périphérique et « Changer » envoyait toujours un
/// nom réel. La SPA n'a pas cette garantie structurelle — d'où cette
/// validation côté cœur, qui ne dépend d'aucune IHM. Sans elle, sur une
/// installation neuve, `audio_current` valait `Some("")`,
/// `GET /api/audio-output` renvoyait `current: ""` indéfiniment, et `""`
/// était transmis à mpv puis persisté dans `state.json`.
pub fn validate_audio_device(device: &str) -> Result<(), String> {
    if device.trim().is_empty() {
        return Err("nom de sortie audio vide".to_string());
    }
    Ok(())
}

async fn audio_output_put(State(state): State<AppState>, Json(req): Json<AudioOutputRequest>) -> Response {
    if let Err(msg) = validate_audio_device(&req.device) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    *state.audio_current.write().await = Some(req.device.clone());
    if state.audio_tx.send(req.device).await.is_err() {
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

async fn locale_put(State(state): State<AppState>, Json(req): Json<LocaleRequest>) -> StatusCode {
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
/// juste une source de commandes supplémentaire).
async fn command_post(State(state): State<AppState>, Json(cmd): Json<ritornello_proto::Command>) -> StatusCode {
    if state.cmd_tx.send(cmd).await.is_err() {
        tracing::warn!("télécommande web: canal de commandes fermé");
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

/// Morceau en cours, en flux poussé (`text/event-stream`).
///
/// Poussé et non sondé, pour trois raisons mesurées avant de trancher : la SPA
/// ne sonde rien aujourd'hui (aucun `setInterval`, aucun WebSocket) ; le cœur
/// diffuse **déjà** ses changements sur un canal `watch`, donc la route ne
/// coûte que quelques lignes et n'ajoute aucun état ; et un appareil le plus
/// souvent inactif n'a pas à recevoir des requêtes qui n'apprennent rien.
///
/// L'état courant est émis **dès la connexion** — même propriété que le flux
/// d'OUI FM qu'on consomme par ailleurs : un onglet ouvert au milieu d'un
/// morceau ne doit pas rester vide jusqu'au suivant.
///
/// Pas d'authentification, comme toutes les autres routes de l'appareil : en
/// ajouter ici seulement donnerait l'illusion d'une protection alors que
/// `/api/command` pilote déjà la lecture sans en demander.
async fn now_playing_sse(
    State(state): State<AppState>,
) -> axum::response::Sse<impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>
{
    use futures::StreamExt;

    let flux = futures::stream::unfold((state.now_playing.clone(), true), |(mut rx, premier)| async move {
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
        // La sérialisation d'un `NowPlayingState` ne peut pas échouer (que des
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
    use crate::metadata::NowPlayingState;

    /// Récepteur de morceau en cours pour les montages qui ne testent pas le
    /// flux SSE : l'émetteur est lâché aussitôt, donc le flux se termine après
    /// la valeur initiale. Les tests du flux passent par
    /// `app_state_with_now_playing`, qui garde l'émetteur.
    pub(crate) fn now_playing_inerte() -> tokio::sync::watch::Receiver<NowPlayingState> {
        tokio::sync::watch::channel(NowPlayingState::default()).1
    }

    /// Montage avec l'émetteur de morceau en cours conservé, pour pousser des
    /// changements pendant un test du flux SSE.
    pub(crate) fn app_state_with_now_playing(
        initial: NowPlayingState,
    ) -> (AppState, tokio::sync::watch::Sender<NowPlayingState>) {
        let (tx, rx) = tokio::sync::watch::channel(initial);
        (AppState { now_playing: rx, ..app_state() }, tx)
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
            now_playing: now_playing_inerte(),
        }
    }

    pub(crate) fn app_state_with_audio() -> (AppState, tokio::sync::mpsc::Receiver<String>) {
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
            now_playing: now_playing_inerte(),
        };
        (state, audio_rx)
    }

    /// Variante avec un `cmd_tx` observable, pour les tests de la télécommande web.
    pub(crate) fn app_state_with_cmd() -> (AppState, tokio::sync::mpsc::Receiver<ritornello_proto::Command>) {
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
            now_playing: now_playing_inerte(),
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
            now_playing: now_playing_inerte(),
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
        assert_eq!(audio_rx.recv().await.unwrap(), "hw:CARD=Headphones");
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
        assert_eq!(cmd_rx.recv().await.unwrap(), ritornello_proto::Command::VolumeUp);
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
        assert_eq!(cmd_rx.recv().await.unwrap(), ritornello_proto::Command::Select(3));
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

    fn etat(titre: &str) -> crate::metadata::NowPlayingState {
        crate::metadata::NowPlayingState {
            source: "radio".into(),
            title: Some(titre.into()),
            origin: Some("icy".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn now_playing_emet_letat_courant_des_la_connexion() {
        // Propriété reprise du flux d'OUI FM : un onglet ouvert au milieu d'un
        // morceau ne doit pas rester vide jusqu'au suivant.
        let (state, _tx) = tests_support::app_state_with_now_playing(etat("Miles Davis - So What"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/now-playing").body(Body::empty()).unwrap()).await.unwrap();
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
    async fn now_playing_pousse_les_changements_suivants() {
        let (state, tx) = tests_support::app_state_with_now_playing(etat("premier"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/now-playing").body(Body::empty()).unwrap()).await.unwrap();
        let mut corps = resp.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps).await["title"], "premier");
        tx.send(etat("second")).unwrap();
        assert_eq!(prochaine_trame(&mut corps).await["title"], "second");
    }

    #[tokio::test]
    async fn deux_clients_recoivent_tous_les_deux() {
        let (state, tx) = tests_support::app_state_with_now_playing(etat("premier"));
        let un = router(state.clone())
            .oneshot(Request::get("/api/now-playing").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let deux = router(state)
            .oneshot(Request::get("/api/now-playing").body(Body::empty()).unwrap())
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
        let (state, tx) = tests_support::app_state_with_now_playing(etat("premier"));
        let survivant = router(state.clone())
            .oneshot(Request::get("/api/now-playing").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut corps_survivant = survivant.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps_survivant).await["title"], "premier");

        {
            let parti = router(state)
                .oneshot(Request::get("/api/now-playing").body(Body::empty()).unwrap())
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
}
