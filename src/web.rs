use crate::config::Stations;
use crate::types::Command;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct WebState {
    pub stations_path: PathBuf,
    pub cmd_tx: mpsc::Sender<Command>,
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route("/api/stations", get(get_stations).put(put_stations))
        .with_state(state)
}

async fn get_stations(State(st): State<WebState>) -> Result<Json<Stations>, StatusCode> {
    Stations::load(&st.stations_path)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn put_stations(
    State(st): State<WebState>,
    Json(stations): Json<Stations>,
) -> StatusCode {
    if stations.validate().is_err() {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }
    if stations.save(&st.stations_path).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    let _ = st.cmd_tx.send(Command::ReloadStations).await;
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Station, Stations};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn setup() -> (axum::Router, tokio::sync::mpsc::Receiver<crate::types::Command>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stations.toml");
        Stations {
            stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }],
        }
        .save(&path)
        .unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let app = router(WebState { stations_path: path, cmd_tx: tx });
        (app, rx, dir)
    }

    #[tokio::test]
    async fn get_stations_renvoie_le_toml_en_json() {
        let (app, _rx, _d) = setup();
        let resp = app
            .oneshot(Request::get("/api/stations").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let s: Stations = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn put_stations_sauvegarde_et_notifie() {
        let (app, mut rx, dir) = setup();
        let new = Stations {
            stations: vec![Station { name: "Inter".into(), url: "http://inter".into(), preset: 2 }],
        };
        let resp = app
            .oneshot(
                Request::put("/api/stations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&new).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let on_disk = Stations::load(&dir.path().join("stations.toml")).unwrap();
        assert_eq!(on_disk, new);
        assert_eq!(rx.recv().await.unwrap(), crate::types::Command::ReloadStations);
    }

    #[tokio::test]
    async fn put_stations_invalide_renvoie_422() {
        let (app, _rx, dir) = setup();
        let bad = Stations {
            stations: vec![Station { name: "X".into(), url: "http://x".into(), preset: 12 }],
        };
        let resp = app
            .oneshot(
                Request::put("/api/stations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&bad).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // le fichier n'a pas bougé
        let on_disk = Stations::load(&dir.path().join("stations.toml")).unwrap();
        assert_eq!(on_disk.stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn page_racine_servie() {
        let (app, _rx, _d) = setup();
        let resp = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
