//! `GET/PUT /api/equalizer`. Same shape as `/api/audio-output`: the route
//! validates, writes the shared copy and sends the change on a channel; it
//! never touches mpv (no HTTP route may block), and `main` calls
//! `Core::set_equalizer` from its `select!` loop.

use super::AppState;
use crate::equalizer::{
    validate, EqualizerError, EqualizerSettings, UserPreset, BANDS_HZ, BUILTIN_PRESETS,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Serialize)]
pub(super) struct BuiltinView {
    id: &'static str,
    name: String,
    gains: [i8; 10],
}

#[derive(Serialize)]
pub(super) struct EqualizerResponse {
    settings: EqualizerSettings,
    bands_hz: [u32; 10],
    builtins: Vec<BuiltinView>,
}

pub(super) async fn equalizer_json(State(state): State<AppState>) -> Json<EqualizerResponse> {
    let settings = state.eq_current.read().await.clone();
    let catalog = state.catalog.read().await;
    let builtins = BUILTIN_PRESETS
        .iter()
        .map(|b| BuiltinView {
            id: b.id,
            name: catalog.get(&format!("eq_preset_{}", b.id)).to_string(),
            gains: b.gains,
        })
        .collect();
    Json(EqualizerResponse { settings, bands_hz: BANDS_HZ, builtins })
}

/// An `Option` that must be present in the body (`null` is fine): serde
/// turns a missing `Option` field into `None` by itself, which would let a
/// body without `preset` silently deselect the preset.
fn required<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

/// The request body. Every field is required, with no serde default:
/// `EqualizerSettings` itself defaults missing fields (that is right for a
/// hand-edited `state.json`, wrong for a full replacement, where a body
/// without `user_presets` would wipe the user's presets).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EqualizerRequest {
    enabled: bool,
    auto_preamp: bool,
    gains: [i8; 10],
    #[serde(deserialize_with = "required")]
    preset: Option<String>,
    user_presets: Vec<UserPreset>,
}

impl EqualizerRequest {
    /// Names are trimmed here, and the `user:<name>` selection with them, so
    /// that what is stored is what the selection names.
    fn into_settings(self) -> EqualizerSettings {
        let preset = self.preset.map(|p| match p.strip_prefix("user:") {
            Some(name) => format!("user:{}", name.trim()),
            None => p,
        });
        EqualizerSettings {
            enabled: self.enabled,
            auto_preamp: self.auto_preamp,
            gains: self.gains,
            preset,
            user_presets: self
                .user_presets
                .into_iter()
                .map(|p| UserPreset { name: p.name.trim().to_string(), gains: p.gains })
                .collect(),
        }
    }
}

async fn refuse(state: &AppState, e: EqualizerError) -> Response {
    let msg = e.message(&*state.catalog.read().await);
    (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response()
}

pub(super) async fn equalizer_put(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let settings = match serde_json::from_value::<EqualizerRequest>(body) {
        Ok(req) => req.into_settings(),
        Err(_) => return refuse(&state, EqualizerError::Malformed).await,
    };
    if let Err(e) = validate(&settings) {
        return refuse(&state, e).await;
    }
    *state.eq_current.write().await = settings.clone();
    if state.eq_tx.send(settings).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::app_state;
    use super::super::{router, AppState};
    use crate::equalizer::{EqualizerSettings, UserPreset};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn rig() -> (AppState, tokio::sync::mpsc::Receiver<EqualizerSettings>) {
        let (eq_tx, eq_rx) = tokio::sync::mpsc::channel(4);
        (AppState { eq_tx, ..app_state() }, eq_rx)
    }

    async fn get_json(state: &AppState) -> serde_json::Value {
        let resp = router(state.clone())
            .oneshot(Request::get("/api/equalizer").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    async fn put(state: &AppState, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let resp = router(state.clone())
            .oneshot(
                Request::put("/api/equalizer")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    fn body(gains: [i64; 10], user_presets: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "enabled": true, "auto_preamp": true, "gains": gains,
            "preset": null, "user_presets": user_presets,
        })
    }

    #[tokio::test]
    async fn equalizer_get_lists_settings_bands_and_translated_builtins() {
        let (state, _rx) = rig();
        let v = get_json(&state).await;
        assert_eq!(v["settings"]["enabled"], false);
        assert_eq!(v["settings"]["gains"], serde_json::json!(vec![0; 10]));
        assert_eq!(v["bands_hz"], serde_json::json!([31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]));
        let builtins = v["builtins"].as_array().unwrap();
        assert_eq!(builtins.len(), 7);
        assert_eq!(builtins[0]["id"], "flat");
        assert_eq!(builtins[0]["name"], "Flat");
        assert_eq!(builtins[5]["name"], "Electronic");
        assert_eq!(builtins[2]["gains"], serde_json::json!([5, 4, 3, 1, -1, -1, 1, 3, 4, 5]));
        for b in builtins {
            let name = b["name"].as_str().unwrap();
            assert!(!name.starts_with("eq_preset_"), "untranslated key: {name}");
        }
    }

    #[tokio::test]
    async fn equalizer_put_valid_is_sent_and_visible() {
        let (state, mut rx) = rig();
        let (status, _) = put(&state, body([0, 0, 0, 0, 0, 6, 0, 0, 0, 0], serde_json::json!([]))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let sent = rx.recv().await.unwrap();
        assert!(sent.enabled);
        assert_eq!(sent.gains[5], 6);
        let v = get_json(&state).await;
        assert_eq!(v["settings"]["gains"][5], 6);
        assert_eq!(v["settings"]["enabled"], true);
    }

    #[tokio::test]
    async fn equalizer_put_invalid_is_422_and_changes_nothing() {
        let (state, mut rx) = rig();
        let user = |g: i64| serde_json::json!([{ "name": "Salon", "gains": [0, 0, 0, 0, 0, 0, 0, 0, 0, g] }]);
        for bad in [
            body([0, 0, 0, 0, 0, 0, 0, 0, 0, 13], serde_json::json!([])),
            body([0; 10], user(21)),
        ] {
            let (status, v) = put(&state, bad).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            let msg = v["error"].as_str().unwrap();
            assert!(!msg.is_empty());
            assert!(!msg.starts_with("eq_error_"), "untranslated key: {msg}");
        }
        assert!(rx.try_recv().is_err(), "nothing must go out on the channel");
        let v = get_json(&state).await;
        assert_eq!(v["settings"]["enabled"], false);
        assert_eq!(v["settings"]["gains"], serde_json::json!(vec![0; 10]));
    }

    #[tokio::test]
    async fn equalizer_put_missing_or_mistyped_field_is_422_and_presets_survive() {
        let (state, mut rx) = rig();
        *state.eq_current.write().await = EqualizerSettings {
            user_presets: vec![UserPreset { name: "Salon".into(), gains: [1; 10] }],
            ..Default::default()
        };
        let full = body([0; 10], serde_json::json!([]));
        let mut cases = Vec::new();
        for field in ["enabled", "auto_preamp", "gains", "preset", "user_presets"] {
            let mut b = full.clone();
            b.as_object_mut().unwrap().remove(field);
            cases.push((format!("missing {field}"), b));
        }
        let mut wrong = full.clone();
        wrong["user_presets"] = serde_json::json!("none");
        cases.push(("user_presets of the wrong type".into(), wrong));
        let mut extra = full.clone();
        extra["volume"] = serde_json::json!(3);
        cases.push(("unknown field".into(), extra));
        for (what, b) in cases {
            let (status, v) = put(&state, b).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{what}");
            let msg = v["error"].as_str().unwrap();
            assert!(!msg.is_empty() && !msg.starts_with("eq_error_"), "{what}: {msg}");
        }
        assert!(rx.try_recv().is_err(), "nothing must go out on the channel");
        let v = get_json(&state).await;
        assert_eq!(v["settings"]["user_presets"][0]["name"], "Salon", "presets wiped");
    }

    #[tokio::test]
    async fn equalizer_put_trims_preset_names_and_the_user_selection() {
        let (state, mut rx) = rig();
        let mut b = body([0; 10], serde_json::json!([{ "name": " Salon ", "gains": vec![0; 10] }]));
        b["preset"] = serde_json::json!("user: Salon ");
        let (status, _) = put(&state, b).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let sent = rx.recv().await.unwrap();
        assert_eq!(sent.user_presets[0].name, "Salon");
        assert_eq!(sent.preset.as_deref(), Some("user:Salon"));
        let v = get_json(&state).await;
        assert_eq!(v["settings"]["user_presets"][0]["name"], "Salon");
        assert_eq!(v["settings"]["preset"], "user:Salon");
    }
}
