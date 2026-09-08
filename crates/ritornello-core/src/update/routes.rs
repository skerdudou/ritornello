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
    enqueue(&state, crate::update::Job::Check)
}

/// Puts a job on the worker's queue **without ever waiting for room**.
///
/// `try_send` and not `send().await`, and that is the whole of this function:
/// the queue holds four jobs and the worker holds one for the length of a
/// download plus a two-minute unit, so a fifth request would await for minutes
/// — and no HTTP route in this product blocks. The scheduler's arm in `main`
/// makes the same choice for the same reason.
///
/// `429` and not the `409` the doc above refuses: they say different things.
/// The 409 that was turned down would have meant "one is already running",
/// which `busy` already says; this one means "the queue is full, ask again" —
/// a statement about the queue, not about the gesture, and one the page can
/// act on by retrying.
fn enqueue(state: &AppState, job: crate::update::Job) -> Response {
    match state.update_tx.try_send(job) {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            StatusCode::TOO_MANY_REQUESTS.into_response()
        }
        // The worker is gone, which the core does not recover from on its own.
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Enqueues an install of the named components.
///
/// Answers **202** and validates only the shape: whether a component exists,
/// is installable, or is worth installing is decided by the worker, which is
/// the only place that has read the release. A route that pre-validated would
/// have to hold the same knowledge and could disagree with it.
pub async fn update_install_post(
    State(state): State<AppState>,
    Json(req): Json<InstallReq>,
) -> Response {
    if req.components.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    enqueue(&state, crate::update::Job::Install(req.components))
}

#[derive(serde::Deserialize)]
pub struct InstallReq {
    pub components: Vec<String>,
}

#[cfg(test)]
mod tests {
    use crate::status::tests_support::app_state;
    use crate::status::{router, AppState};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    /// A rig whose job queue is real: `app_state` drops its receiver, which
    /// makes every send fail as `Closed` and hides the two answers that
    /// matter here.
    fn state_with_queue(
        capacity: usize,
    ) -> (AppState, tokio::sync::mpsc::Receiver<crate::update::Job>) {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (AppState { update_tx: tx, ..app_state() }, rx)
    }

    fn install_request() -> Request<Body> {
        Request::post("/api/update/install")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"components":["radio"]}"#))
            .unwrap()
    }

    #[tokio::test]
    async fn an_install_is_enqueued_and_answered_at_once() {
        let (state, mut rx) = state_with_queue(4);
        let resp = router(state).oneshot(install_request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::update::Job::Install(names)) if names == vec!["radio".to_string()]
        ));
    }

    /// The property, and it is the global constraint rather than a nicety:
    /// **no route waits for the worker.** The worker holds a job for the
    /// length of a download plus a two-minute privileged unit, so a `send`
    /// that awaited room would hang the request for minutes.
    ///
    /// Driven from the event: the queue is genuinely full (its receiver is
    /// alive and reads nothing), and the whole call is put under a deadline
    /// far shorter than any real download — a route that waited would trip it
    /// rather than answer.
    #[tokio::test]
    async fn a_full_queue_is_refused_immediately_rather_than_waited_on() {
        let (state, _rx) = state_with_queue(1);
        state.update_tx.try_send(crate::update::Job::Check).unwrap();
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            router(state).oneshot(install_request()),
        )
        .await
        .expect("the route answered rather than waiting for room");
        assert_eq!(answer.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
    }

    /// The check route shares the same door, and predates it: it used to
    /// `send().await` too.
    #[tokio::test]
    async fn a_full_queue_refuses_a_check_the_same_way() {
        let (state, _rx) = state_with_queue(1);
        state.update_tx.try_send(crate::update::Job::Check).unwrap();
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            router(state).oneshot(Request::post("/api/update/check").body(Body::empty()).unwrap()),
        )
        .await
        .expect("the route answered rather than waiting for room");
        assert_eq!(answer.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn an_empty_component_list_is_refused_before_the_queue() {
        let (state, mut rx) = state_with_queue(4);
        let resp = router(state)
            .oneshot(
                Request::post("/api/update/install")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"components":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(rx.try_recv().is_err(), "nothing was enqueued");
    }
}
