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
    pub admin_url: Option<String>,
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
            admin_url: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(StatusState {
            plugins: raw
                .plugins
                .into_iter()
                .map(|p| PluginStatus { name: p.name, kind: p.kind, connected: p.connected, admin_url: p.admin_url })
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
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status_page))
        .route("/api/status", get(status_json))
        .route("/api/audio-output", get(audio_output_json).put(audio_output_put))
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
    let devices = crate::audio_output::list_devices().unwrap_or_default();
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

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn status_page(State(state): State<AppState>) -> Html<String> {
    let s = state.status.read().await;
    let mut rows = String::new();
    for p in &s.plugins {
        let etat = if p.connected { "connecté" } else { "indisponible" };
        let lien = escape_html(p.admin_url.as_deref().unwrap_or("-"));
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{etat}</td><td>{lien}</td></tr>",
            escape_html(&p.name),
            escape_html(&p.kind)
        ));
    }
    let logs: String = state
        .logs
        .snapshot()
        .iter()
        .rev()
        .map(|l| format!("<li>{}</li>", escape_html(l)))
        .collect();
    let devices = crate::audio_output::list_devices().unwrap_or_default();
    let current = state.audio_current.read().await.clone();
    let options: String = devices
        .iter()
        .map(|d| {
            let sel = if Some(d) == current.as_ref() { " selected" } else { "" };
            format!("<option value=\"{}\"{sel}>{}</option>", escape_html(d), escape_html(d))
        })
        .collect();
    Html(format!(
        "<!doctype html><html lang=\"fr\"><meta charset=\"utf-8\"><title>radio-pi — statut</title>\
         <h1>radio-pi</h1><p>Source active : {}</p>\
         <table border=\"1\"><tr><th>Plugin</th><th>Genre</th><th>État</th><th>Admin</th></tr>{}</table>\
         <h2>Sortie audio</h2>\
         <select id=\"audio-device\">{options}</select>\
         <button onclick=\"setAudioOutput()\">Changer</button> <span id=\"audio-msg\"></span>\
         <script>\
         async function setAudioOutput() {{\
           const device = document.getElementById('audio-device').value;\
           const r = await fetch('/api/audio-output', {{method:'PUT', headers:{{'content-type':'application/json'}}, body: JSON.stringify({{device}})}});\
           document.getElementById('audio-msg').textContent = r.ok ? 'OK' : 'Erreur';\
         }}\
         </script>\
         <h2>Dernières erreurs</h2><ul>{}</ul></html>",
        escape_html(&s.active_source), rows, logs
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
                PluginStatus { name: "radio".into(), kind: "source".into(), connected: true, admin_url: Some("http://raspberrypi.local:8081".into()) },
                PluginStatus { name: "cd".into(), kind: "source".into(), connected: false, admin_url: None },
            ],
            active_source: "radio".into(),
        }
    }

    fn app_state() -> AppState {
        let (audio_tx, _audio_rx) = tokio::sync::mpsc::channel(4);
        AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
        }
    }

    fn app_state_with_audio() -> (AppState, tokio::sync::mpsc::Receiver<String>) {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
        let state = AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(Some("default".to_string()))),
            audio_tx,
        };
        (state, audio_rx)
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
}
