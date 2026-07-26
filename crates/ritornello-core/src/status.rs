use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
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
    pub cmd_tx: mpsc::Sender<ritornello_proto::Command>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status_page))
        .route("/api/status", get(status_json))
        .route("/api/audio-output", get(audio_output_json).put(audio_output_put))
        .route("/api/locale", get(locale_json).put(locale_put))
        .route("/api/command", axum::routing::post(command_post))
        .route("/plugins/:name/", get(crate::admin::admin_page))
        .route(
            "/plugins/:name/api/data",
            get(crate::admin::admin_get_data).put(crate::admin::admin_put_data),
        )
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

async fn audio_output_put(State(state): State<AppState>, Json(req): Json<AudioOutputRequest>) -> StatusCode {
    *state.audio_current.write().await = Some(req.device.clone());
    if state.audio_tx.send(req.device).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::NO_CONTENT
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

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn status_page(State(state): State<AppState>) -> Html<String> {
    let s = state.status.read().await;
    let cat = state.catalog.read().await;
    let current_locale = state.locale_current.read().await.clone().unwrap_or_else(|| "en".to_string());

    let mut rows = String::new();
    for p in &s.plugins {
        let etat = if p.connected { cat.get("connected") } else { cat.get("unavailable") };
        let lien = if p.admin {
            format!("<a href=\"/plugins/{}/\">{}</a>", escape_html(&p.name), escape_html(cat.get("admin_link")))
        } else {
            "-".to_string()
        };
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{lien}</td></tr>",
            escape_html(&p.name),
            escape_html(&p.kind),
            escape_html(etat)
        ));
    }
    let logs: String = state
        .logs
        .snapshot()
        .iter()
        .rev()
        .map(|l| format!("<li>{}</li>", escape_html(l)))
        .collect();

    let devices = match crate::audio_output::list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("liste des sorties audio indisponible: {e}");
            Vec::new()
        }
    };
    let current = state.audio_current.read().await.clone();
    let options: String = devices
        .iter()
        .map(|d| {
            let sel = if Some(d) == current.as_ref() { " selected" } else { "" };
            format!("<option value=\"{}\"{sel}>{}</option>", escape_html(d), escape_html(d))
        })
        .collect();

    let locales = list_locales(&state.locales_root);
    let locale_options: String = locales
        .iter()
        .map(|l| {
            let sel = if *l == current_locale { " selected" } else { "" };
            format!("<option value=\"{}\"{sel}>{}</option>", escape_html(l), escape_html(l))
        })
        .collect();

    let mut remote_buttons = String::new();
    for n in 1..=9u8 {
        remote_buttons.push_str(&format!("<button onclick=\"sendCmd({{cmd:'Select',arg:{n}}})\">{n}</button> "));
    }
    let simple_commands: [(&str, &str); 12] = [
        ("remote_preset_next", "Next"),
        ("remote_preset_prev", "Prev"),
        ("remote_vol_up", "VolumeUp"),
        ("remote_vol_down", "VolumeDown"),
        ("remote_mute", "Mute"),
        ("remote_play_pause", "PlayPause"),
        ("remote_stop", "Stop"),
        ("remote_track_next", "NextTrack"),
        ("remote_track_prev", "PrevTrack"),
        ("remote_eject", "Eject"),
        ("remote_source", "SourceCycle"),
        ("remote_power", "Power"),
    ];
    for (key, cmd) in simple_commands {
        remote_buttons.push_str(&format!(
            "<button onclick=\"sendCmd({{cmd:'{cmd}'}})\">{}</button> ",
            escape_html(cat.get(key))
        ));
    }

    Html(format!(
        "<!doctype html><html lang=\"{lang}\"><meta charset=\"utf-8\"><title>ritornello — {title}</title>\
         <h1>ritornello</h1><p>{active_label} : {active}</p>\
         <table border=\"1\"><tr><th>{c_plugin}</th><th>{c_kind}</th><th>{c_state}</th><th>{c_admin}</th></tr>{rows}</table>\
         <h2>{audio}</h2>\
         <select id=\"audio-device\">{options}</select>\
         <button onclick=\"setAudioOutput()\">{change}</button> <span id=\"audio-msg\"></span>\
         <h2>{language}</h2>\
         <select id=\"locale\">{locale_options}</select>\
         <button onclick=\"setLocale()\">{change}</button> <span id=\"locale-msg\"></span>\
         <h2>{remote_title}</h2>\
         <div>{remote_buttons}</div>\
         <span id=\"remote-msg\"></span>\
         <script>\
         async function setAudioOutput() {{\
           const device = document.getElementById('audio-device').value;\
           const r = await fetch('/api/audio-output', {{method:'PUT', headers:{{'content-type':'application/json'}}, body: JSON.stringify({{device}})}});\
           document.getElementById('audio-msg').textContent = r.ok ? '{ok}' : '{error}';\
         }}\
         async function setLocale() {{\
           const locale = document.getElementById('locale').value;\
           const r = await fetch('/api/locale', {{method:'PUT', headers:{{'content-type':'application/json'}}, body: JSON.stringify({{locale}})}});\
           if (r.ok) {{ location.reload(); }} else {{ document.getElementById('locale-msg').textContent = '{error}'; }}\
         }}\
         async function sendCmd(payload) {{\
           const r = await fetch('/api/command', {{method:'POST', headers:{{'content-type':'application/json'}}, body: JSON.stringify(payload)}});\
           document.getElementById('remote-msg').textContent = r.ok ? '{ok}' : '{error}';\
         }}\
         </script>\
         <h2>{recent}</h2><ul>{logs}</ul></html>",
        lang = escape_html(&current_locale),
        title = escape_html(cat.get("status_title")),
        active_label = escape_html(cat.get("active_source_label")),
        active = escape_html(&s.active_source),
        c_plugin = escape_html(cat.get("col_plugin")),
        c_kind = escape_html(cat.get("col_kind")),
        c_state = escape_html(cat.get("col_state")),
        c_admin = escape_html(cat.get("col_admin")),
        audio = escape_html(cat.get("audio_output")),
        change = escape_html(cat.get("change")),
        language = escape_html(cat.get("language")),
        ok = escape_html(cat.get("ok")),
        error = escape_html(cat.get("error")),
        remote_title = escape_html(cat.get("remote_title")),
        remote_buttons = remote_buttons,
        recent = escape_html(cat.get("recent_errors")),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn sample() -> StatusState {
        StatusState {
            plugins: vec![
                PluginStatus { name: "radio".into(), kind: "source".into(), connected: true, admin: true },
                PluginStatus { name: "cd".into(), kind: "source".into(), connected: false, admin: false },
            ],
            active_source: "radio".into(),
        }
    }

    fn app_state() -> AppState {
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
            cmd_tx,
        }
    }

    fn app_state_with_audio() -> (AppState, tokio::sync::mpsc::Receiver<String>) {
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
            cmd_tx,
        };
        (state, audio_rx)
    }

    /// Variante avec un `cmd_tx` observable, pour les tests de la télécommande web.
    fn app_state_with_cmd() -> (AppState, tokio::sync::mpsc::Receiver<ritornello_proto::Command>) {
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
            cmd_tx,
        };
        (state, cmd_rx)
    }

    /// Variante avec un `locale_tx` observable et un catalogue chargé en `fr`
    /// depuis une racine temporaire (le TempDir est retourné pour rester vivant).
    fn app_state_fr() -> (AppState, tokio::sync::mpsc::Receiver<String>, tempfile::TempDir) {
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
            cmd_tx,
        };
        (state, locale_rx, dir)
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
    async fn page_statut_rendue_en_francais() {
        let (state, _rx, _dir) = app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Source active"));
        assert!(html.contains("Sortie audio"));
        assert!(!html.contains("Active source"));
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
    async fn page_statut_affiche_la_telecommande() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Remote control"));
        assert!(html.contains("/api/command"));
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
    async fn page_statut_affiche_les_dernieres_erreurs() {
        let state = app_state();
        state.logs.push("WARN plugin cd indisponible".into());
        let app = router(state);
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("plugin cd indisponible"));
    }

    #[tokio::test]
    async fn page_statut_lien_admin_interne() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("href=\"/plugins/radio/\""));
        assert!(!html.contains(":8081"));
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
        write!(w, "WARN plugin radio indisponible\n").unwrap();
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
}
