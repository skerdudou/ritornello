//! `POST /api/lock-sound/preview`: the config page lets the owner hear a lock
//! sound. The route only queues the choice on a channel (`main` calls
//! `Core::preview_lock_sound` from its `select!` loop); it never awaits the
//! core, and it is not a wire `Command`. A full queue answers 429 at once.

use super::AppState;
use crate::lock_sound::{LockSound, LockVolume};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tokio::sync::mpsc::error::TrySendError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewRequest {
    sound: LockSound,
    /// The level on screen, 1 to 4. Absent: the reference level (2).
    #[serde(default)]
    volume: Option<u8>,
}

/// The 422 `lock_sound_invalid` of an unknown sound, shared with
/// `PUT /api/settings` (`settings_put`).
pub(super) async fn refuse(state: &AppState) -> Response {
    let msg = state.catalog.read().await.get("lock_sound_invalid").to_string();
    (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// The 422 `lock_sound_volume_invalid` of an unknown level, shared with
/// `PUT /api/settings` (`settings_put`).
pub(super) async fn refuse_volume(state: &AppState) -> Response {
    let msg = state.catalog.read().await.get("lock_sound_volume_invalid").to_string();
    (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response()
}

pub(super) async fn lock_sound_preview(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let req = match serde_json::from_value::<PreviewRequest>(body) {
        Ok(req) => req,
        Err(_) => return refuse(&state).await,
    };
    let volume = match req.volume.map_or(Ok(LockVolume::default()), LockVolume::try_from) {
        Ok(volume) => volume,
        Err(_) => return refuse_volume(&state).await,
    };
    if req.sound == LockSound::None {
        return StatusCode::NO_CONTENT.into_response();
    }
    match state.lock_sound_preview_tx.try_send((req.sound, volume)) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(TrySendError::Full(_)) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(TrySendError::Closed(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::app_state;
    use super::super::{router, AppState};
    use crate::lock_sound::{LockSound, LockVolume};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    fn rig() -> (AppState, tokio::sync::mpsc::Receiver<(LockSound, LockVolume)>) {
        let (lock_sound_preview_tx, rx) = tokio::sync::mpsc::channel(1);
        (AppState { lock_sound_preview_tx, ..app_state() }, rx)
    }

    async fn post(state: &AppState, body: &str) -> StatusCode {
        router(state.clone())
            .oneshot(
                Request::post("/api/lock-sound/preview")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn preview_queues_the_sound() {
        let (state, mut rx) = rig();
        assert_eq!(post(&state, r#"{"sound":"e"}"#).await, StatusCode::NO_CONTENT);
        assert_eq!(rx.try_recv().unwrap(), (LockSound::E, LockVolume::Two));
    }

    #[tokio::test]
    async fn preview_queues_the_level_on_screen() {
        let (state, mut rx) = rig();
        assert_eq!(post(&state, r#"{"sound":"b","volume":4}"#).await, StatusCode::NO_CONTENT);
        assert_eq!(rx.try_recv().unwrap(), (LockSound::B, LockVolume::Four));
    }

    #[tokio::test]
    async fn an_unknown_level_is_refused_with_its_own_message() {
        let (state, mut rx) = rig();
        assert_eq!(post(&state, r#"{"sound":"b","volume":5}"#).await, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn preview_of_none_queues_nothing() {
        let (state, mut rx) = rig();
        assert_eq!(post(&state, r#"{"sound":"none"}"#).await, StatusCode::NO_CONTENT);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn an_unknown_sound_is_refused() {
        let (state, mut rx) = rig();
        assert_eq!(post(&state, r#"{"sound":"z"}"#).await, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(post(&state, "{}").await, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_full_queue_answers_at_once() {
        let (state, _rx) = rig();
        state.lock_sound_preview_tx.try_send((LockSound::A, LockVolume::Two)).unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(1), post(&state, r#"{"sound":"b"}"#))
            .await
            .expect("the route must not wait for the core");
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    }
}
