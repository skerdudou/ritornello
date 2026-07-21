use crate::status::AppState;
use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::Json;

/// Abstraction des opérations d'admin dont les routes du cœur ont besoin.
/// Implémentée par `AdminClient` (IPC réel) ; un faux l'implémente en test.
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn page(&self) -> Result<String>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>;
}

#[async_trait::async_trait]
impl AdminBackend for ritornello_plugin_sdk::AdminClient {
    async fn page(&self) -> Result<String> {
        self.get_page().await
    }
    async fn get_data(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_data(self).await
    }
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        ritornello_plugin_sdk::AdminClient::set_data(self, data).await
    }
}

pub async fn admin_page(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.page().await {
            Ok(html) => Html(html).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response(),
        },
    }
}

pub async fn admin_get_data(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.get_data().await {
            Ok(value) => Json(value).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response(),
        },
    }
}

pub async fn admin_put_data(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(data): Json<serde_json::Value>,
) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.set_data(data).await {
            Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
            Ok(Err(msg)) => (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{router, AppState, LogBuffer, StatusState};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    struct Fake {
        reject: bool,
        down: bool,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Fake {
        async fn page(&self) -> Result<String> {
            if self.down { anyhow::bail!("down") }
            Ok("<h1>radio</h1>".into())
        }
        async fn get_data(&self) -> Result<serde_json::Value> {
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "stations": [] }))
        }
        async fn set_data(&self, _data: serde_json::Value) -> Result<Result<(), String>> {
            if self.down { anyhow::bail!("down") }
            Ok(if self.reject { Err("présélection en double".into()) } else { Ok(()) })
        }
    }

    fn state_with(fake: Fake) -> AppState {
        let (audio_tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut backends: HashMap<String, Arc<dyn AdminBackend>> = HashMap::new();
        backends.insert("radio".into(), Arc::new(fake));
        AppState {
            status: Arc::new(tokio::sync::RwLock::new(StatusState { plugins: vec![], active_source: "radio".into() })),
            logs: Arc::new(LogBuffer::new(10)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
            admin_backends: Arc::new(backends),
        }
    }

    #[tokio::test]
    async fn get_page_sert_le_html() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app.oneshot(Request::get("/plugins/radio/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("radio"));
    }

    #[tokio::test]
    async fn get_data_relaie_le_json() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["stations"].is_array());
    }

    #[tokio::test]
    async fn put_data_valide_renvoie_204() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"stations":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn put_data_invalide_renvoie_422_avec_message() {
        let app = router(state_with(Fake { reject: true, down: false }));
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"stations":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "présélection en double");
    }

    #[tokio::test]
    async fn plugin_inconnu_renvoie_404() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app.oneshot(Request::get("/plugins/inconnu/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn plugin_injoignable_renvoie_502() {
        let app = router(state_with(Fake { reject: false, down: true }));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
