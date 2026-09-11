//! `GET /api/update`, and the two actions that write.
//!
//! Both actions answer **202 immediately** and work in a background task. The
//! admin protocol is serial with a five-second cap, and an I/O that hangs has
//! already made a page *disappear* in this product. Same shape as
//! `/api/command`, whose 204 means "enqueued".

use crate::status::AppState;
use crate::update::catalogue::{self, Catalogue};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub async fn update_json(State(state): State<AppState>) -> Response {
    let snapshot = state.update.read().await.clone();
    Json(snapshot).into_response()
}

/// `GET /api/update/catalogue` — what the last read release says about the
/// components it describes.
///
/// Fetched here and not by the page, for three reasons in order: the check
/// keeps its one document per repository, so the design rule is untouched;
/// the network stays on the core's side, which already holds the client, the
/// User-Agent GitHub requires and the deadlines; and a page calling GitHub
/// itself would meet cross-origin sharing on a release asset, which fails on
/// some browsers only — the worst kind of outage.
///
/// Kept for the life of the session, keyed by the release tag it came from,
/// so a dialog opened twice asks nothing the second time.
///
/// An empty `components` is the honest answer for a release that publishes no
/// catalogue, which is every release published before this chantier.
pub async fn update_catalogue_json(State(state): State<AppState>) -> Response {
    let Some(url) = state.update.read().await.catalogue_url.clone() else {
        return Json(Catalogue::default()).into_response();
    };
    // The URL is tag-qualified (`https://.../<tag>/catalogue.json`), so it is
    // its own cache key: a fresh check that offers the same release, or one
    // that has not run again yet, hits this without a socket.
    if let Some(cached) = state.update_catalogue_cache.read().await.as_ref()
        && cached.0 == url
    {
        return Json(cached.1.clone()).into_response();
    }
    let fresh = fetch_catalogue(&url).await.unwrap_or_default();
    *state.update_catalogue_cache.write().await = Some((url, fresh.clone()));
    Json(fresh).into_response()
}

/// The network half, kept apart from the route so a failure of any kind —
/// no client, a transport error, a non-200, an unreadable body — collapses to
/// one `None`: the page's honest fallback is the same empty catalogue whether
/// GitHub refused the request or answered something this core cannot parse.
async fn fetch_catalogue(url: &str) -> Option<Catalogue> {
    let client = crate::update::download::client().ok()?;
    let (status, body) = crate::update::download::fetch_text(&client, url).await.ok()?;
    if status != 200 {
        return None;
    }
    catalogue::parse(&body).ok()
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
    use http_body_util::BodyExt;
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

    /// The route answers an empty catalogue rather than an error when the
    /// release publishes none — the page then shows names alone and says so.
    #[tokio::test]
    async fn a_release_without_a_catalogue_answers_an_empty_one() {
        let (state, _rx) = state_with_queue(4);
        let app = router(state);
        let resp = app
            .oneshot(Request::get("/api/update/catalogue").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!({"components": {}}));
    }

    /// A cached answer is served without consulting `catalogue_url` again:
    /// the cache is seeded directly here, and the state's own
    /// `catalogue_url` is left pointing at an address nothing serves, which
    /// would fail the request if the cache were bypassed.
    #[tokio::test]
    async fn a_cached_catalogue_is_served_without_a_second_fetch() {
        use crate::update::catalogue::{Catalogue, Entry};
        use std::collections::BTreeMap;

        let (state, _rx) = state_with_queue(4);
        state.update.write().await.catalogue_url =
            Some("https://127.0.0.1:9/unreachable/catalogue.json".to_string());
        let mut components = BTreeMap::new();
        components.insert(
            "radio".to_string(),
            Entry { kinds: vec!["source".to_string()], description: "Stations".to_string() },
        );
        *state.update_catalogue_cache.write().await =
            Some(("https://127.0.0.1:9/unreachable/catalogue.json".to_string(), Catalogue { components }));
        let app = router(state);
        let resp = app
            .oneshot(Request::get("/api/update/catalogue").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["components"]["radio"]["description"], "Stations");
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
