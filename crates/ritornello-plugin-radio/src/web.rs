use crate::config::Stations;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct WebState {
    pub stations_path: PathBuf,
    pub stations: Arc<RwLock<Stations>>,
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route("/api/stations", get(get_stations).put(put_stations))
        .with_state(state)
}

async fn get_stations(State(st): State<WebState>) -> Json<Stations> {
    Json(st.stations.read().await.clone())
}

async fn put_stations(State(st): State<WebState>, Json(stations): Json<Stations>) -> StatusCode {
    if stations.validate().is_err() {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }
    if stations.save(&st.stations_path).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *st.stations.write().await = stations;
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Station;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn setup() -> (Router, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stations.toml");
        let stations = Stations { stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }] };
        stations.save(&path).unwrap();
        let app = router(WebState { stations_path: path.clone(), stations: Arc::new(RwLock::new(stations)) });
        (app, path, dir)
    }

    #[tokio::test]
    async fn get_stations_renvoie_le_toml_en_json() {
        let (app, _p, _d) = setup();
        let resp = app.oneshot(Request::get("/api/stations").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let s: Stations = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn put_stations_sauvegarde_et_met_a_jour_letat_partage() {
        let (app, path, _d) = setup();
        let new = Stations { stations: vec![Station { name: "Inter".into(), url: "http://inter".into(), preset: 2 }] };
        let resp = app
            .oneshot(
                Request::put("/api/stations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&new).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);
        assert_eq!(Stations::load(&path).unwrap(), new);
    }

    #[tokio::test]
    async fn put_stations_invalide_renvoie_422() {
        let (app, path, _d) = setup();
        let bad = Stations { stations: vec![Station { name: "X".into(), url: "http://x".into(), preset: 12 }] };
        let resp = app
            .oneshot(
                Request::put("/api/stations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&bad).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(Stations::load(&path).unwrap().stations[0].name, "FIP");
    }
}
