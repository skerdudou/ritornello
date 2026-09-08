//! `GET /api/update`, and the two actions that write.
//!
//! Both actions answer **202 immediately** and work in a background task. The
//! admin protocol is serial with a five-second cap, and an I/O that hangs has
//! already made a page *disappear* in this product. Same shape as
//! `/api/command`, whose 204 means "enqueued".

use crate::status::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub async fn update_json(State(state): State<AppState>) -> Response {
    let snapshot = state.update.read().await.clone();
    Json(snapshot).into_response()
}

/// Enqueues a check. Answers 202 even when one is already running: the page
/// polls `GET /api/update` for the outcome, and a 409 here would make it
/// invent a second way of saying "busy" that `busy` already says.
pub async fn update_check_post(State(state): State<AppState>) -> Response {
    if state.update_tx.send(crate::update::Job::Check).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::ACCEPTED.into_response()
}
