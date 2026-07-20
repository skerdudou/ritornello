use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
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
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status_page))
        .route("/api/status", get(status_json))
        .with_state(state)
}

async fn status_json(State(state): State<AppState>) -> Json<StatusState> {
    Json(state.status.read().await.clone())
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
    Html(format!(
        "<!doctype html><html lang=\"fr\"><meta charset=\"utf-8\"><title>radio-pi — statut</title>\
         <h1>radio-pi</h1><p>Source active : {}</p>\
         <table border=\"1\"><tr><th>Plugin</th><th>Genre</th><th>État</th><th>Admin</th></tr>{}</table>\
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
        AppState { status: Arc::new(tokio::sync::RwLock::new(sample())), logs: Arc::new(LogBuffer::new(50)) }
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
