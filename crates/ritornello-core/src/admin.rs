use crate::status::AppState;
use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Abstraction des opérations d'admin dont les routes du cœur ont besoin.
/// Implémentée par `AdminClient` (IPC réel) ; un faux l'implémente en test.
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>>;
    async fn catalog(&self) -> Result<serde_json::Value>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>;
}

#[async_trait::async_trait]
impl AdminBackend for ritornello_plugin_sdk::AdminClient {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
        self.get_asset(path).await
    }
    async fn catalog(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_catalog(self).await
    }
    async fn get_data(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_data(self).await
    }
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        ritornello_plugin_sdk::AdminClient::set_data(self, data).await
    }
}

/// Actifs d'IHM déjà récupérés, par `(plugin, chemin)` → `(mime, corps, etag)`.
/// Un bundle est immuable pour la durée de vie du processus du plugin : on ne
/// le relit pas par IPC à chaque rechargement de page.
pub type AssetCache = tokio::sync::RwLock<
    std::collections::HashMap<(String, String), (String, String, String)>,
>;

fn etag_of(body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    format!("\"{:x}\"", h.finish())
}

/// `ui.js` ou `ui.css` d'un plugin. Le nom du fichier vient du chemin de la
/// route, jamais d'une liste en dur : le cœur ne sait pas ce qu'un plugin
/// expose.
pub async fn admin_asset(
    State(st): State<AppState>,
    Path((name, fichier)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(backend) = st.admin_backends.get(&name) else {
        return (StatusCode::NOT_FOUND, "plugin inconnu").into_response();
    };
    let cle = (name.clone(), fichier.clone());
    let en_cache = st.admin_assets.read().await.get(&cle).cloned();
    let (mime, body, etag) = match en_cache {
        Some(v) => v,
        None => match backend.asset(&fichier).await {
            Ok(Some((mime, body))) => {
                let etag = etag_of(&body);
                let v = (mime, body, etag);
                st.admin_assets.write().await.insert(cle, v.clone());
                v
            }
            Ok(None) => return (StatusCode::NOT_FOUND, "actif inconnu").into_response(),
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (asset {fichier}): {e}");
                return (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response();
            }
        },
    };
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (axum::http::header::CONTENT_TYPE, mime.as_str()),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
            (axum::http::header::ETAG, etag.as_str()),
        ],
        body,
    )
        .into_response()
}

pub async fn admin_i18n(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.catalog().await {
            Ok(v) => Json(v).into_response(),
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (catalog): {e}");
                (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response()
            }
        },
    }
}

pub async fn admin_get_data(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.get_data().await {
            Ok(value) => Json(value).into_response(),
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (get_data): {e}");
                (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response()
            }
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
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (set_data): {e}");
                (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response()
            }
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

    #[derive(Default)]
    struct Fake {
        reject: bool,
        down: bool,
        appels_asset: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Fake {
        async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
            if self.down { anyhow::bail!("down") }
            self.appels_asset.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(match path {
                "ui.js" => Some(("text/javascript".to_string(), "export const contract = 1".to_string())),
                _ => None,
            })
        }
        async fn catalog(&self) -> Result<serde_json::Value> {
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "btn_save": "Enregistrer" }))
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
        let (locale_tx, _locale_rx) = tokio::sync::mpsc::channel(4);
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel(4);
        let mut backends: HashMap<String, Arc<dyn AdminBackend>> = HashMap::new();
        backends.insert("radio".into(), Arc::new(fake));
        AppState {
            status: Arc::new(tokio::sync::RwLock::new(StatusState { plugins: vec![], active_source: "radio".into() })),
            logs: Arc::new(LogBuffer::new(10)),
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
            admin_backends: Arc::new(backends),
            admin_assets: Arc::new(Default::default()),
            cmd_tx,
            player: crate::status::tests_support::player_inerte(),
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
        }
    }

    #[tokio::test]
    async fn ui_js_est_servi_avec_son_type_et_un_etag() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/javascript");
        assert!(resp.headers().contains_key("etag"));
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("contract"));
    }

    #[tokio::test]
    async fn ui_js_est_mis_en_cache_apres_le_premier_acces() {
        // Un bundle est immuable pour la duree de vie du processus du plugin :
        // le relire par IPC a chaque rechargement de page serait du gaspillage.
        let fake = Fake::default();
        let appels = fake.appels_asset.clone();
        let state = state_with(fake);
        let app = router(state);
        for _ in 0..3 {
            let resp = app
                .clone()
                .oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        assert_eq!(appels.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn if_none_match_repond_304() {
        let app = router(state_with(Fake::default()));
        let premier = app
            .clone()
            .oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let etag = premier.headers()["etag"].to_str().unwrap().to_string();
        let second = app
            .oneshot(
                Request::get("/plugins/radio/ui.js")
                    .header("if-none-match", etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn un_actif_inconnu_du_plugin_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.css").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn le_catalogue_du_plugin_est_servi_a_plat() {
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["btn_save"], "Enregistrer");
    }

    #[tokio::test]
    async fn ui_js_dun_plugin_inconnu_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/inconnu/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn la_page_dadmin_reste_servie_par_la_spa() {
        // Point de vigilance : la nouvelle route `/plugins/:name/:fichier` ne
        // doit pas capter `/plugins/<nom>/` (segment final vide), qui doit
        // continuer de tomber sur le repli et servir le shell — c'est l'URL
        // historique, presente dans le README et dans les liens de la page de
        // statut.
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
    }

    #[tokio::test]
    async fn get_data_relaie_le_json() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["stations"].is_array());
    }

    #[tokio::test]
    async fn put_data_valide_renvoie_204() {
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"op":"save","stations":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn put_data_invalide_renvoie_422_avec_message() {
        let app = router(state_with(Fake { reject: true, ..Default::default() }));
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"op":"save","stations":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "présélection en double");
    }

    // Depuis la Task 10, un nom de plugin inconnu sur la route d'*actif*
    // (`/plugins/<nom>/ui.js`) 404 (voir `ui_js_dun_plugin_inconnu_repond_404`),
    // et `/plugins/<nom>/` (segment final vide, URL historique) tombe sur le
    // repli SPA (voir `la_page_dadmin_reste_servie_par_la_spa`), qui rend
    // toujours le shell quel que soit le nom. Les *données*
    // (`api/data`) restent strictes : un nom de plugin inconnu y 404 toujours,
    // pour ne jamais masquer une faute de frappe derrière une réponse 200.
    #[tokio::test]
    async fn plugin_inconnu_sert_le_shell() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/inconnu/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn plugin_inconnu_sur_lapi_de_donnees_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/inconnu/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn plugin_injoignable_renvoie_502() {
        let app = router(state_with(Fake { down: true, ..Default::default() }));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
