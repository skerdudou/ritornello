//! `GET /api/update`, and the two actions that write.
//!
//! Both actions answer **202 immediately** and work in a background task. The
//! admin protocol is serial with a five-second cap, and an I/O that hangs has
//! already made a page *disappear* in this product. Same shape as
//! `/api/command`, whose 204 means "enqueued".

use crate::status::AppState;
use crate::update::catalogue::{self, Catalogue};
use crate::update::sources;
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
/// Kept for the life of the session, keyed by the tag-qualified URL it came
/// from, so a dialog opened twice asks nothing the second time.
///
/// An empty `components` (**200**) is the honest answer for a release that
/// publishes no catalogue, which is every release published before this
/// chantier. A fetch that **failed** is a different fact and answers
/// **503**, not 200 — see below.
pub async fn update_catalogue_json(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<CatalogueQuery>,
) -> Response {
    if let Some(repo) = query.repo {
        return source_catalogue_json(&state, &repo).await;
    }
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
    // Cache on success only. A failure here — no client, a transport error, a
    // GitHub rate-limit page, an unreadable body — must leave the cache
    // untouched: writing the empty default under `url` would memorise a
    // transient outage as "this release publishes nothing" for the life of
    // the core session, which `catalogue::parse`'s own test says is a
    // different fact from an empty catalogue.
    //
    // N2: a failure also answers a **different status** from the "no
    // catalogue at all" branch above, rather than the same 200 empty body.
    // Caching only on success (the fix above) stops the *core* from
    // remembering the wrong fact, but the page has a latch of its own
    // (`InstallablesDialog.vue`'s `asked`) that only resets in its `catch`
    // path — a 200 never reaches it, so a byte-identical "no catalogue"
    // answer would still freeze the failure for the page's life, cured only
    // by a reload. 503 routes a transient failure through the retry
    // machinery that already exists on the page, rather than through the
    // one meant for "this release genuinely publishes nothing".
    match fetch_catalogue(&url).await {
        Some(fresh) => {
            *state.update_catalogue_cache.write().await = Some((url, fresh.clone()));
            Json(fresh).into_response()
        }
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct CatalogueQuery {
    /// `owner/repo` of a source, for that source's own catalogue.
    pub repo: Option<String>,
}

/// One source catalogue as fetched: `(repo, url)` is the key, the catalogue
/// is kept whole and filtered on every read.
#[derive(Debug, Clone)]
pub struct SourceCatalogueCached {
    pub repo: String,
    pub url: String,
    pub catalogue: Catalogue,
}

/// `GET /api/update/catalogue?repo=owner/repo` — what a **source** says about
/// the plugins it is offered fresh for (spec §6).
///
/// **The query selects, it never addresses.** The URL fetched is the one the
/// last check read off that source's own answer (`UpdateState::
/// source_catalogues`); a repository that is not among them — never asked,
/// offering nothing fresh, our own — answers 404 without a socket, so no
/// request can make the core fetch an address of its choosing.
///
/// **Filtered to that source's fresh names**, on every read: a stranger's
/// catalogue that also describes `radio`, an installed plugin or a name
/// another source contests has those entries dropped here, so it can never
/// describe what it does not legitimately offer.
///
/// Otherwise the official route's contract, for the same reasons: the same
/// bounded fetch (`fetch_catalogue`), cached on success only, keyed by
/// `(repo, url)`, and a failure answers 503 rather than an empty 200.
async fn source_catalogue_json(state: &AppState, repo: &str) -> Response {
    let Some(repo) = sources::normalize_repo(repo) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let current = state.update.read().await.source_catalogues.clone();
    let Some(entry) = current.iter().find(|c| c.repo == repo) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let only_its_names = |catalogue: &Catalogue| Catalogue {
        components: catalogue
            .components
            .iter()
            .filter(|(name, _)| entry.names.contains(name))
            .map(|(name, e)| (name.clone(), e.clone()))
            .collect(),
    };
    let Some(url) = entry.url.clone() else {
        return Json(Catalogue::default()).into_response();
    };
    if let Some(cached) =
        state.update_source_catalogue_cache.read().await.iter().find(|c| c.repo == repo && c.url == url)
    {
        return Json(only_its_names(&cached.catalogue)).into_response();
    }
    let Some(fresh) = fetch_catalogue(&url).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let answer = only_its_names(&fresh);
    let mut cache = state.update_source_catalogue_cache.write().await;
    // Only what the last check still names, so the cache is bounded by the
    // sources list rather than growing with every release a source makes.
    cache.retain(|c| {
        (c.repo != repo || c.url != url)
            && current.iter().any(|s| s.repo == c.repo && s.url.as_deref() == Some(c.url.as_str()))
    });
    cache.push(SourceCatalogueCached { repo, url, catalogue: fresh });
    drop(cache);
    Json(answer).into_response()
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

/// The announcements the sources view is built from: one `(plugin, repository)`
/// pair per **plugin**, in the order the status lines hold.
///
/// That order is `plugins.toml` order: `resequence_plugin_lines` keeps the
/// lines in manifest order and puts names the manifest does not mention
/// last, so nothing here reads a file. A plugin announcing two kinds has two
/// lines, hence the dedupe by name; the first repository it announced wins.
fn announcements(status: &crate::status::StatusState) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for line in &status.plugins {
        match out.iter_mut().find(|(name, _)| *name == line.name) {
            Some((_, repo)) => {
                if repo.is_none() {
                    *repo = line.repository.clone();
                }
            }
            None => out.push((line.name.clone(), line.repository.clone())),
        }
    }
    out
}

/// The sources the installed third-party language packs name, from the
/// registry's last sweep — memory, never a directory walk. Its guard is
/// dropped before the caller takes any other lock.
async fn pack_sources(state: &AppState) -> Vec<String> {
    sources::pack_sources(state.registry.read().await.installed_packs())
}

/// `GET /api/update/sources` — the union view (`update::sources`), computed
/// from in-memory handles only: no file, no network.
pub async fn sources_json(State(state): State<AppState>) -> Response {
    let announced = announcements(&*state.status.read().await);
    let packs = pack_sources(&state).await;
    let added = state.update_sources.read().await.clone();
    let reports = state.update.read().await.source_reports.clone();
    Json(sources::source_rows(&announced, &packs, &added, &reports)).into_response()
}

#[derive(serde::Deserialize)]
pub struct SourceAddReq {
    pub repo: String,
}

async fn error_body(state: &AppState, status: StatusCode, key: &str) -> Response {
    let message = state
        .catalog
        .read()
        .await
        .get(key)
        .replace("{max}", &sources::SOURCES_MAX.to_string());
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// Hands a new stored list to the core loop **without waiting for room**
/// (`enqueue`'s rule). `Err` is the status to answer.
fn send_sources(state: &AppState, list: Vec<String>) -> Result<(), StatusCode> {
    match state.update_sources_tx.try_send(list) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Err(StatusCode::TOO_MANY_REQUESTS),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// `POST /api/update/sources` — adds one repository.
///
/// The core loop is told **first** and the shared handle is written only once
/// that succeeded (the order `locale_put` documents): the other way round, a
/// refused send would leave the page showing a source the core never stored.
/// The write lock is held across the check and the send so two concurrent
/// additions cannot both pass the limit; `try_send` does not wait, so holding
/// it costs nothing.
pub async fn sources_post(State(state): State<AppState>, Json(req): Json<SourceAddReq>) -> Response {
    // Read before the write lock is taken, so no other lock is ever waited
    // on while it is held but `status`'s, in the order already documented.
    let packs = pack_sources(&state).await;
    let mut handle = state.update_sources.write().await;
    let announced = announcements(&*state.status.read().await);
    let rows = sources::source_rows(&announced, &packs, &handle, &[]);
    let repo = match sources::check_add(&req.repo, &rows) {
        Ok(repo) => repo,
        Err(refusal) => {
            let (status, key) = match refusal {
                sources::AddRefusal::Invalid => (StatusCode::UNPROCESSABLE_ENTITY, "update_source_invalid"),
                sources::AddRefusal::Full => (StatusCode::UNPROCESSABLE_ENTITY, "update_source_full"),
                sources::AddRefusal::Official => (StatusCode::CONFLICT, "update_source_official"),
                sources::AddRefusal::AlreadyListed => (StatusCode::CONFLICT, "update_source_already_listed"),
            };
            drop(handle);
            return error_body(&state, status, key).await;
        }
    };
    let mut list = handle.clone();
    list.push(repo);
    if let Err(status) = send_sources(&state, list.clone()) {
        return status.into_response();
    }
    *handle = list;
    StatusCode::NO_CONTENT.into_response()
}

/// `DELETE /api/update/sources/{owner}/{repo}` — removes an entry the operator
/// stored. An official row, or one only a plugin announces, is not stored and
/// answers 404: there is nothing here to remove.
pub async fn sources_delete(
    State(state): State<AppState>,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
) -> Response {
    let wanted = sources::normalize_repo(&format!("{owner}/{repo}"));
    let mut handle = state.update_sources.write().await;
    let is_wanted = |entry: &String| wanted.is_some() && sources::normalize_repo(entry) == wanted;
    if !handle.iter().any(is_wanted) {
        drop(handle);
        return error_body(&state, StatusCode::NOT_FOUND, "update_source_not_removable").await;
    }
    let list: Vec<String> = handle.iter().filter(|e| !is_wanted(e)).cloned().collect();
    if let Err(status) = send_sources(&state, list.clone()) {
        return status.into_response();
    }
    *handle = list;
    StatusCode::NO_CONTENT.into_response()
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

/// `POST /api/languages/{language}` — installs every pack that language is
/// offered in.
///
/// **202 and a queue, like every other write in this module.** Validates the
/// shape of the code and nothing else: whether a pack exists for it, and
/// whether it is worth installing, is the worker's to decide because the
/// worker is the only thing that has read the release.
pub async fn language_install_post(
    State(state): State<AppState>,
    axum::extract::Path(language): axum::extract::Path<String>,
) -> Response {
    if !crate::status::valid_locale(&language) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    enqueue(&state, crate::update::Job::InstallLanguage(language))
}

/// `DELETE /api/languages/{language}` — same contract as the install route
/// above, for the opposite gesture.
pub async fn language_remove_delete(
    State(state): State<AppState>,
    axum::extract::Path(language): axum::extract::Path<String>,
) -> Response {
    if !crate::status::valid_locale(&language) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    enqueue(&state, crate::update::Job::RemoveLanguage(language))
}

#[derive(serde::Deserialize)]
pub struct PreferenceReq {
    pub module: String,
    /// A pack id, or `null` to forget the preference.
    pub pack: Option<String>,
}

/// The preference route's `{error}` body: the catalog message for `key`, its
/// `{max}` being the ceiling on stored preferences.
async fn preference_error(state: &AppState, status: StatusCode, key: &str) -> Response {
    let message =
        state.catalog.read().await.get(key).replace("{max}", &crate::state::PACK_PREFERENCES_MAX.to_string());
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// `PUT /api/languages/{language}/preference` — which installed pack speaks
/// for one module in this language (`Registry::ordered_packs`).
///
/// Validates the shape, then refuses a **new** choice that does not name an
/// installed pack carrying the module in the language, and a list that would
/// grow past `PACK_PREFERENCES_MAX` (both 422). A removal is always accepted.
/// An entry already stored outlives its pack: it is simply not honoured while
/// the pack is gone (`ordered_packs`), and a pack put back finds it again.
///
/// **No I/O and no wait.** The new list is computed from the registry's copy
/// and handed to the core loop first, which persists it and retranslates
/// (`Core::set_pack_preferences`); only once that send succeeded is the same
/// list written into the registry, so a `GET /api/locale` that follows the
/// `204` already reports it — the order `locale_put` documents. The registry's
/// write guard is held across the computation and the send so two concurrent
/// requests cannot each start from the list the other is replacing;
/// `try_send` does not wait, so holding it costs nothing.
pub async fn language_preference_put(
    State(state): State<AppState>,
    axum::extract::Path(language): axum::extract::Path<String>,
    Json(req): Json<PreferenceReq>,
) -> Response {
    let pack_ok = req.pack.as_deref().is_none_or(ritornello_i18n::valid_pack_id);
    if !crate::status::valid_locale(&language) || !ritornello_i18n::valid_pack_name(&req.module) || !pack_ok {
        return preference_error(&state, StatusCode::UNPROCESSABLE_ENTITY, "language_preference_invalid").await;
    }
    let mut registry = state.registry.write().await;
    let list = crate::state::with_pack_preference(registry.preferences(), &language, &req.module, req.pack.as_deref());
    // Two refusals keep an unauthenticated client from growing the stored
    // list without bound — every entry is rewritten into `state.json` on each
    // change and read by every resolution. A removal (`null`) passes both.
    // A **new** choice must name an installed pack that carries this module in
    // this language — the predicate `ordered_packs` keeps a pack by. An entry
    // already stored is never refused for its pack having gone since: a pack
    // put back finds its preference again.
    let refusal = if req.pack.as_deref().is_some_and(|p| !registry.pack_carries(p, &req.module, &language)) {
        Some("language_preference_not_installed")
    } else if list.len() > crate::state::PACK_PREFERENCES_MAX && list.len() > registry.preferences().len() {
        // Only a list that grows past the ceiling: replacing a stored key, or
        // a removal, is always accepted, even on a list longer than it.
        Some("language_preference_full")
    } else {
        None
    };
    if let Some(key) = refusal {
        drop(registry);
        return preference_error(&state, StatusCode::UNPROCESSABLE_ENTITY, key).await;
    }
    if state.pack_preferences_tx.try_send(list.clone()).is_err() {
        drop(registry);
        // Full: the core is busy, asking again succeeds. Closed: the core is
        // gone, which it does not recover from. Both leave nothing changed.
        return preference_error(&state, StatusCode::SERVICE_UNAVAILABLE, "language_preference_not_saved").await;
    }
    registry.set_preferences(list);
    StatusCode::NO_CONTENT.into_response()
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

    /// A minimal HTTP/1.1 200 response wrapping `body`, for the raw TCP rig
    /// below — same idiom as `download.rs`'s own test server.
    fn http_ok_json(body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// Closes the first connection at once, without writing a byte — the
    /// client sees a transport error, exactly `fetch_catalogue`'s "no client,
    /// a transport error, a non-200, an unreadable body" collapse — then
    /// answers `response` for real on the second. Proves Major A: a first,
    /// failing fetch must not poison the one that follows it.
    async fn serve_fail_then_ok(response: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored).await;
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://127.0.0.1:{port}/catalogue.json")
    }

    /// The failing test for Major A: before the fix, the first (failing)
    /// answer was written into `update_catalogue_cache` under the URL, so the
    /// second request — same URL, server now healthy — kept reading the
    /// cached empty catalogue instead of asking again.
    #[tokio::test]
    async fn a_failed_fetch_does_not_poison_a_later_successful_one() {
        let body = br#"{"components":{"radio":{"kinds":["source"],"description":"Stations"}}}"#;
        let url = serve_fail_then_ok(http_ok_json(body)).await;
        let (state, _rx) = state_with_queue(4);
        state.update.write().await.catalogue_url = Some(url);
        let cache = state.update_catalogue_cache.clone();
        let app = router(state);

        // First request: the transport fails, the page is told so distinctly
        // (N2: 503, not the 200 empty body a genuine "no catalogue" release
        // answers with — see `a_fetch_failure_answers_service_unavailable_
        // not_the_no_catalogue_200`), and — this is the assertion the old
        // code fails — the cache must stay untouched.
        let resp = app
            .clone()
            .oneshot(Request::get("/api/update/catalogue").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(cache.read().await.is_none(), "a failed fetch must not be cached");

        // Second request, same URL: the server answers for real this time,
        // and nothing stale stands in the way of it.
        let resp = app
            .oneshot(Request::get("/api/update/catalogue").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body2 = resp.into_body().collect().await.unwrap().to_bytes();
        let v2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert_eq!(v2["components"]["radio"]["description"], "Stations");
    }

    /// N2: before this fix, a fetch failure answered the same 200 empty body
    /// as "this release genuinely publishes nothing", which is what let the
    /// page's own `asked` latch (`InstallablesDialog.vue`) freeze the failure
    /// for the rest of its life — cured only by a reload, never by a retry.
    /// A distinct status is what the page's existing `catch` path needs to
    /// tell the two facts apart on the wire, not only in the core's cache.
    #[tokio::test]
    async fn a_fetch_failure_answers_service_unavailable_not_the_no_catalogue_200() {
        let (state, _rx) = state_with_queue(4);
        // Nothing listens here: the connection is refused at once, the same
        // shape of failure as a dropped Wi-Fi link or GitHub unreachable.
        state.update.write().await.catalogue_url =
            Some("http://127.0.0.1:1/catalogue.json".to_string());
        let app = router(state);
        let resp = app
            .oneshot(Request::get("/api/update/catalogue").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // --- GET /api/update/catalogue?repo= ---

    fn zed_source(url: Option<&str>) -> crate::update::sources::SourceCatalogue {
        crate::update::sources::SourceCatalogue {
            repo: "z/zed".into(),
            url: url.map(str::to_string),
            names: vec!["zed".into()],
        }
    }

    fn two_entries() -> crate::update::catalogue::Catalogue {
        use crate::update::catalogue::{Catalogue, Entry};
        let mut components = std::collections::BTreeMap::new();
        components.insert("zed".to_string(), Entry { kinds: vec!["display".into()], description: "Zed".into() });
        components.insert("radio".to_string(), Entry { kinds: vec!["source".into()], description: "Not yours".into() });
        Catalogue { components }
    }

    async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
    }

    /// The query only selects among what the last check left: a repository
    /// that is not a source with a fresh offer — a stranger, our own, one
    /// not spelt as a repository — answers 404, and nothing is fetched (the
    /// one URL the state holds is unreachable, so a fetch would answer 503).
    /// **[MUTATION]** look the URL up without the repository match (first
    /// entry): red.
    #[tokio::test]
    async fn a_repository_that_is_not_a_current_source_is_never_fetched() {
        let (state, _rx) = state_with_queue(4);
        state.update.write().await.source_catalogues = vec![zed_source(Some("http://127.0.0.1:1/catalogue.json"))];
        let app = router(state);
        for uri in [
            "/api/update/catalogue?repo=evil%2Fother",
            "/api/update/catalogue?repo=skerdudou%2Fritornello",
            "/api/update/catalogue?repo=not%20a%20repo",
        ] {
            let (status, _) = get_json(app.clone(), uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }

    /// A source whose releases publish no catalogue answers the empty one,
    /// as the official route does: the page then shows name, source, version.
    #[tokio::test]
    async fn a_source_without_a_catalogue_answers_an_empty_one() {
        let (state, _rx) = state_with_queue(4);
        state.update.write().await.source_catalogues = vec![zed_source(None)];
        let (status, v) = get_json(router(state), "/api/update/catalogue?repo=z%2Fzed").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v, serde_json::json!({"components": {}}));
    }

    /// A stranger's catalogue describes only what that stranger is offered
    /// fresh: its entry for `radio` is dropped. Served from the cache, under
    /// a URL nothing serves, and the repository matched case-insensitively.
    /// **[MUTATION]** drop the `names` filter: red on `radio`.
    #[tokio::test]
    async fn a_source_catalogue_never_describes_a_name_that_source_does_not_offer() {
        let (state, _rx) = state_with_queue(4);
        let url = "http://127.0.0.1:1/catalogue.json";
        state.update.write().await.source_catalogues = vec![zed_source(Some(url))];
        *state.update_source_catalogue_cache.write().await =
            vec![super::SourceCatalogueCached { repo: "z/zed".into(), url: url.into(), catalogue: two_entries() }];
        let (status, v) = get_json(router(state), "/api/update/catalogue?repo=Z%2FZed").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["components"]["zed"]["description"], "Zed");
        assert!(v["components"].get("radio").is_none(), "{v}");
    }

    /// The cache key is `(repo, url)`: another repository's entry under the
    /// same URL is not this one's answer. Nothing serves the URL, so the
    /// request must go to the network and fail.
    /// **[MUTATION]** drop `c.repo == repo` from the cache lookup: red (200).
    /// **[MUTATION]** drop `c.url == url`: red with the second case (stale URL).
    #[tokio::test]
    async fn the_source_cache_is_keyed_by_repository_and_url() {
        let url = "http://127.0.0.1:1/catalogue.json";
        for (repo, cached_url) in [("other/one", url), ("z/zed", "http://127.0.0.1:1/older/catalogue.json")] {
            let (state, _rx) = state_with_queue(4);
            state.update.write().await.source_catalogues = vec![zed_source(Some(url))];
            *state.update_source_catalogue_cache.write().await = vec![super::SourceCatalogueCached {
                repo: repo.into(),
                url: cached_url.into(),
                catalogue: two_entries(),
            }];
            let (status, _) = get_json(router(state), "/api/update/catalogue?repo=z%2Fzed").await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{repo} {cached_url}");
        }
    }

    /// Fetched from the URL the check read, filtered, cached on success only
    /// and under `(repo, url)`, with entries the last check no longer names
    /// pruned on that write.
    #[tokio::test]
    async fn a_source_catalogue_is_fetched_cached_on_success_and_the_cache_pruned() {
        let body = br#"{"components":{"zed":{"kinds":["display"],"description":"Zed"},"radio":{"kinds":["source"],"description":"x"}}}"#;
        let url = serve_fail_then_ok(http_ok_json(body)).await;
        let (state, _rx) = state_with_queue(4);
        state.update.write().await.source_catalogues = vec![zed_source(Some(&url))];
        *state.update_source_catalogue_cache.write().await = vec![super::SourceCatalogueCached {
            repo: "gone/away".into(),
            url: "https://gone/catalogue.json".into(),
            catalogue: two_entries(),
        }];
        let cache = state.update_source_catalogue_cache.clone();
        let app = router(state);

        let (status, _) = get_json(app.clone(), "/api/update/catalogue?repo=z%2Fzed").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(cache.read().await.len(), 1, "a failure is not cached");

        let (status, v) = get_json(app, "/api/update/catalogue?repo=z%2Fzed").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["components"]["zed"]["description"], "Zed");
        assert!(v["components"].get("radio").is_none(), "{v}");
        let cached: Vec<(String, String)> = cache.read().await.iter().map(|c| (c.repo.clone(), c.url.clone())).collect();
        assert_eq!(cached, vec![("z/zed".to_string(), url)]);
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

    #[tokio::test]
    async fn installing_a_language_is_enqueued_and_answered_at_once() {
        let (state, mut rx) = state_with_queue(4);
        let resp = router(state)
            .oneshot(Request::post("/api/languages/fr").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(matches!(rx.try_recv(), Ok(crate::update::Job::InstallLanguage(l)) if l == "fr"));
    }

    #[tokio::test]
    async fn removing_a_language_is_enqueued_and_answered_at_once() {
        let (state, mut rx) = state_with_queue(4);
        let resp = router(state)
            .oneshot(Request::delete("/api/languages/fr").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(matches!(rx.try_recv(), Ok(crate::update::Job::RemoveLanguage(l)) if l == "fr"));
    }

    /// The shape check the route DOES do, and the only one: a language code
    /// that could become a path is refused before the queue, exactly as the
    /// install route refuses an empty component list.
    ///
    /// **Both doors, not only `POST`.** A first version of this test drove
    /// `POST` alone and stayed green even with `language_remove_delete`'s own
    /// `valid_locale` check mutated to `if false` (measured while writing
    /// this task) — the two routes share the same guard in source but each
    /// has its own call site, and a broken one leaves nothing red unless
    /// something actually exercises it.
    #[tokio::test]
    async fn a_language_that_is_not_a_bare_code_is_refused_before_the_queue() {
        let (state, mut rx) = state_with_queue(4);
        let app = router(state);
        for bad in ["..", "fr%2F..", "a-language-code-far-too-long-to-be-one"] {
            let resp = app
                .clone()
                .oneshot(Request::post(format!("/api/languages/{bad}")).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "POST {bad}");
            let resp = app
                .clone()
                .oneshot(Request::delete(format!("/api/languages/{bad}")).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "DELETE {bad}");
        }
        assert!(rx.try_recv().is_err(), "nothing was enqueued");
    }

    /// The global constraint, restated for the two new doors: no route waits
    /// for the worker.
    #[tokio::test]
    async fn a_full_queue_refuses_a_language_install_immediately() {
        let (state, _rx) = state_with_queue(1);
        state.update_tx.try_send(crate::update::Job::Check).unwrap();
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            router(state).oneshot(Request::post("/api/languages/fr").body(Body::empty()).unwrap()),
        )
        .await
        .expect("the route answered rather than waiting for room");
        assert_eq!(answer.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
    }

    /// A rig whose sources channel is real, so what the core loop would be
    /// told can be read back, and whose handle starts empty.
    fn state_with_sources(
        capacity: usize,
    ) -> (AppState, tokio::sync::mpsc::Receiver<Vec<String>>) {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (AppState { update_sources_tx: tx, ..app_state() }, rx)
    }

    fn post_source(repo: &str) -> Request<Body> {
        Request::post("/api/update/sources")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "repo": repo }).to_string()))
            .unwrap()
    }

    async fn error_of(resp: axum::response::Response) -> String {
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["error"].as_str().unwrap_or_default().to_string()
    }

    async fn rows_of(app: axum::Router) -> Vec<serde_json::Value> {
        let resp = app.oneshot(Request::get("/api/update/sources").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn the_sources_list_starts_with_the_official_row() {
        let (state, _rx) = state_with_sources(4);
        let rows = rows_of(router(state)).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["repo"], "skerdudou/ritornello");
        assert_eq!(rows[0]["kind"], "official");
    }

    /// An installed third-party language pack puts its source on the list,
    /// read-only, as a plugin's announcement does — and adding that source
    /// again is refused as already listed (both routes read the packs).
    #[tokio::test]
    async fn an_installed_third_party_pack_announces_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = ritornello_i18n::PackManifest {
            language: "fr".into(),
            version: "1.0.0".into(),
            source: "https://github.com/Z/Zed".into(),
            modules: vec!["core".into()],
        };
        let files = vec![("core.toml".to_string(), b"k = \"v\"\n".to_vec())];
        let layers = ritornello_i18n::validate(&manifest, &files).unwrap();
        let id = crate::langpack::store::third_party_pack_id("fr", "z/zed");
        let contents = crate::langpack::archive::PackContents { manifest, layers, files };
        crate::langpack::store::install(dir.path(), &id, &contents).unwrap();
        let (state, _rx) = state_with_sources(4);
        let registry = crate::i18n::seeded_registry(dir.path().to_path_buf());
        let state = AppState { registry: std::sync::Arc::new(tokio::sync::RwLock::new(registry)), ..state };

        let rows = rows_of(router(state.clone())).await;
        let got: Vec<(&str, &str)> =
            rows.iter().map(|r| (r["repo"].as_str().unwrap(), r["kind"].as_str().unwrap())).collect();
        assert_eq!(got, vec![("skerdudou/ritornello", "official"), ("z/zed", "announced")]);
        let resp = router(state).oneshot(post_source("Z/Zed")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "already listed, by the pack");
    }

    /// What the last check left in `UpdateState.source_reports` reaches the
    /// row it describes, and only that row.
    #[tokio::test]
    async fn the_last_check_s_report_is_attached_to_its_row() {
        let (state, _rx) = state_with_sources(4);
        *state.update_sources.write().await = vec!["b/bee".to_string()];
        state.update.write().await.source_reports = vec![(
            "b/bee".to_string(),
            crate::update::sources::SourceReport { answered: true, plugins: vec!["zed".into()], languages: vec![] },
        )];
        let rows = rows_of(router(state)).await;
        assert_eq!(rows[0]["report"], serde_json::Value::Null, "the official row was not in this report");
        assert_eq!(rows[1]["report"]["answered"], true);
        assert_eq!(rows[1]["report"]["plugins"], serde_json::json!(["zed"]));
    }

    #[tokio::test]
    async fn adding_a_source_stores_it_lowercased_and_tells_the_core() {
        let (state, mut rx) = state_with_sources(4);
        let handle = state.update_sources.clone();
        let app = router(state);
        let resp = app.clone().oneshot(post_source("Z/Zed")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(*handle.read().await, vec!["z/zed".to_string()]);
        assert_eq!(rx.try_recv().unwrap(), vec!["z/zed".to_string()]);
        let rows = rows_of(app).await;
        assert_eq!((rows[1]["repo"].clone(), rows[1]["kind"].clone(), rows[1]["stored"].clone()),
            ("z/zed".into(), "added".into(), true.into()));
    }

    #[tokio::test]
    async fn a_refused_addition_names_its_reason_and_changes_nothing() {
        let (state, mut rx) = state_with_sources(4);
        let handle = state.update_sources.clone();
        let app = router(state);
        assert_eq!(app.clone().oneshot(post_source("Z/Zed")).await.unwrap().status(), StatusCode::NO_CONTENT);
        let again = app.clone().oneshot(post_source("z/zed")).await.unwrap();
        assert_eq!(again.status(), StatusCode::CONFLICT);
        assert!(!error_of(again).await.is_empty());
        let official = app.clone().oneshot(post_source("skerdudou/ritornello")).await.unwrap();
        assert_eq!(official.status(), StatusCode::CONFLICT);
        assert!(!error_of(official).await.is_empty());
        let nope = app.clone().oneshot(post_source("nope")).await.unwrap();
        assert_eq!(nope.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!error_of(nope).await.is_empty());
        assert_eq!(handle.read().await.len(), 1);
        assert_eq!(rx.try_recv().unwrap().len(), 1);
        assert!(rx.try_recv().is_err(), "a refusal tells the core nothing");
    }

    #[tokio::test]
    async fn the_limit_answers_422_with_the_number_in_the_message() {
        let (state, _rx) = state_with_sources(32);
        let app = router(state);
        for i in 0..16 {
            let r = app.clone().oneshot(post_source(&format!("o/r{i}"))).await.unwrap();
            assert_eq!(r.status(), StatusCode::NO_CONTENT, "{i}");
        }
        let r = app.oneshot(post_source("o/one-more")).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(error_of(r).await.contains("16"));
    }

    #[tokio::test]
    async fn a_removal_is_case_insensitive_and_only_for_stored_rows() {
        let (state, mut rx) = state_with_sources(4);
        let handle = state.update_sources.clone();
        let app = router(state);
        app.clone().oneshot(post_source("z/zed")).await.unwrap();
        rx.try_recv().unwrap();
        let gone = app
            .clone()
            .oneshot(Request::delete("/api/update/sources/Z/ZED").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(gone.status(), StatusCode::NO_CONTENT);
        assert!(handle.read().await.is_empty());
        assert!(rx.try_recv().unwrap().is_empty());
        for unknown in ["/api/update/sources/z/zed", "/api/update/sources/skerdudou/ritornello"] {
            let r = app.clone().oneshot(Request::delete(unknown).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(r.status(), StatusCode::NOT_FOUND, "{unknown}");
            assert!(!error_of(r).await.is_empty());
        }
    }

    /// P8: the core is told first, the handle written only on success. A
    /// closed channel must leave the handle as it was and answer 500.
    #[tokio::test]
    async fn a_core_that_cannot_be_told_leaves_the_list_untouched() {
        let (state, rx) = state_with_sources(4);
        drop(rx);
        let handle = state.update_sources.clone();
        let r = router(state).oneshot(post_source("z/zed")).await.unwrap();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(handle.read().await.is_empty());
    }

    /// The same, for a removal.
    #[tokio::test]
    async fn a_removal_the_core_cannot_be_told_of_keeps_the_entry() {
        let (state, rx) = state_with_sources(4);
        drop(rx);
        *state.update_sources.write().await = vec!["z/zed".to_string()];
        let handle = state.update_sources.clone();
        let r = router(state)
            .oneshot(Request::delete("/api/update/sources/z/zed").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(*handle.read().await, vec!["z/zed".to_string()]);
    }

    /// A plugin announcing two kinds has two status lines and is one
    /// announcer; and an announced row is read-only until the operator also
    /// stores it.
    #[tokio::test]
    async fn a_plugin_with_two_kinds_is_listed_once_and_its_row_is_read_only() {
        let (state, _rx) = state_with_sources(4);
        {
            let mut status = state.status.write().await;
            let mut a = crate::status::PluginStatus::startup("zed");
            a.kind = "source".into();
            a.repository = Some("https://github.com/Z/Zed".into());
            let mut b = a.clone();
            b.kind = "display".into();
            status.plugins = vec![a, b];
        }
        let app = router(state);
        let rows = rows_of(app.clone()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["repo"], "z/zed");
        assert_eq!(rows[1]["kind"], "announced");
        assert_eq!(rows[1]["announced_by"], serde_json::json!(["zed"]));
        assert_eq!(rows[1]["stored"], false);
        let r = app
            .oneshot(Request::delete("/api/update/sources/z/zed").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    /// The order comes from the status lines, which are kept in
    /// `plugins.toml` order: no file is read to build the view.
    #[tokio::test]
    async fn announced_rows_follow_the_status_line_order() {
        let (state, _rx) = state_with_sources(4);
        {
            let mut status = state.status.write().await;
            let mut b = crate::status::PluginStatus::startup("second");
            b.repository = Some("https://github.com/b/bee".into());
            let mut a = crate::status::PluginStatus::startup("first");
            a.repository = Some("https://github.com/z/zed".into());
            status.plugins = vec![a, b];
        }
        let rows = rows_of(router(state)).await;
        let repos: Vec<&str> = rows.iter().map(|r| r["repo"].as_str().unwrap()).collect();
        assert_eq!(repos, vec!["skerdudou/ritornello", "z/zed", "b/bee"]);
    }

    /// A hand-edited entry that does not read must not be swept away by a
    /// removal whose own path does not read either (`None == None`).
    #[tokio::test]
    async fn an_unreadable_path_removes_nothing_even_next_to_an_unreadable_entry() {
        let (state, mut rx) = state_with_sources(4);
        *state.update_sources.write().await = vec!["junk".to_string()];
        let handle = state.update_sources.clone();
        let r = router(state)
            .oneshot(Request::delete("/api/update/sources/a%20b/c").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(*handle.read().await, vec!["junk".to_string()]);
        assert!(rx.try_recv().is_err());
    }

    /// A stored entry written in mixed case by hand is still removable.
    #[tokio::test]
    async fn a_hand_edited_mixed_case_entry_is_removable() {
        let (state, _rx) = state_with_sources(4);
        *state.update_sources.write().await = vec!["Z/Zed".to_string()];
        let handle = state.update_sources.clone();
        let r = router(state)
            .oneshot(Request::delete("/api/update/sources/z/zed").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert!(handle.read().await.is_empty());
    }

    // --- PUT /api/languages/{language}/preference ---

    type Prefs = Vec<crate::state::PackPreference>;

    /// A rig whose preference channel is real and whose registry holds one
    /// installed third-party French pack carrying `radio` and `core`. Returns
    /// that pack's id; the directory lives as long as the returned guard.
    fn state_with_preferences(
        capacity: usize,
    ) -> (AppState, tokio::sync::mpsc::Receiver<Prefs>, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let packs_root = dir.path().join("packs");
        let id = crate::langpack::store::write_test_pack(
            &packs_root,
            "fr",
            Some("z/zed"),
            None,
            &[("core", "k = \"v\"\n"), ("radio", "k = \"v\"\n")],
        );
        let registry = crate::i18n::seeded_registry(packs_root);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let state = AppState {
            registry: std::sync::Arc::new(tokio::sync::RwLock::new(registry)),
            pack_preferences_tx: tx,
            ..app_state()
        };
        (state, rx, id, dir)
    }

    fn put_preference(language: &str, body: serde_json::Value) -> Request<Body> {
        Request::put(format!("/api/languages/{language}/preference"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn pref(language: &str, module: &str, pack: &str) -> crate::state::PackPreference {
        crate::state::PackPreference { language: language.into(), module: module.into(), pack: pack.into() }
    }

    /// What the catalog says for `key`, `{max}` filled as the route fills it.
    async fn catalog_message(state: &AppState, key: &str) -> String {
        state.catalog.read().await.get(key).replace("{max}", &crate::state::PACK_PREFERENCES_MAX.to_string())
    }

    /// The core is told the whole new list, and the registry holds the same
    /// list once the route has answered.
    /// **[MUTATION]** drop `registry.set_preferences` from the route: red on
    /// the registry assertion.
    #[tokio::test]
    async fn a_preference_is_told_to_the_core_and_kept_in_the_registry() {
        let (state, mut rx, id, _dir) = state_with_preferences(4);
        let registry = state.registry.clone();
        let r = router(state)
            .oneshot(put_preference("fr", serde_json::json!({ "module": "radio", "pack": id })))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(rx.recv().await.unwrap(), vec![pref("fr", "radio", &id)]);
        assert_eq!(registry.read().await.preferences(), [pref("fr", "radio", &id)]);
        assert!(rx.recv().await.is_none(), "one message, and the router held the only sender");
    }

    /// `null` forgets the preference of that module in that language, and
    /// only that one — whatever the packs it names (none of them installed
    /// here): a removal is never refused.
    #[tokio::test]
    async fn a_null_pack_forgets_the_preference() {
        let (state, mut rx, _id, _dir) = state_with_preferences(4);
        state.registry.write().await.set_preferences(vec![pref("fr", "radio", "a"), pref("fr", "core", "b")]);
        let registry = state.registry.clone();
        let r = router(state)
            .oneshot(put_preference("fr", serde_json::json!({ "module": "radio", "pack": null })))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(rx.recv().await.unwrap(), vec![pref("fr", "core", "b")]);
        assert_eq!(registry.read().await.preferences(), [pref("fr", "core", "b")]);
    }

    /// 422 with **the shape message**, and nothing told or kept, for each part
    /// of the request that is not a bare name. The exact message matters: the
    /// installed-pack check would refuse these too, with another one.
    /// **[MUTATION]** drop each of the three operands of the shape check in
    /// turn: the matching case below answers another message, red.
    #[tokio::test]
    async fn a_preference_that_is_not_bare_names_is_refused_and_changes_nothing() {
        let id = crate::langpack::store::third_party_pack_id("fr", "z/zed");
        for (language, body) in [
            ("fr", serde_json::json!({ "module": "Radio", "pack": id })),
            ("fr", serde_json::json!({ "module": "radio", "pack": "../etc" })),
            ("f%20r", serde_json::json!({ "module": "radio", "pack": id })),
        ] {
            let (state, mut rx, _id, _dir) = state_with_preferences(4);
            let expected = catalog_message(&state, "language_preference_invalid").await;
            let registry = state.registry.clone();
            let r = router(state).oneshot(put_preference(language, body.clone())).await.unwrap();
            assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY, "{language} {body}");
            assert_eq!(error_of(r).await, expected, "{language} {body}");
            assert_ne!(expected, "language_preference_invalid", "a catalog message, not its key");
            assert!(rx.recv().await.is_none(), "nothing told to the core: {body}");
            assert!(registry.read().await.preferences().is_empty());
        }
    }

    /// A new choice must name an installed pack carrying that module in that
    /// language (Important 1, part a): anything else would let a client grow
    /// the stored list with names nothing on the device answers to.
    /// **[MUTATION]** drop each of the three operands of `Registry::carries`
    /// / `pack_carries` (installed id, language, module): the matching case
    /// below answers 204, red.
    #[tokio::test]
    async fn a_new_preference_must_name_an_installed_pack_carrying_its_module_in_its_language() {
        let ours = crate::langpack::store::pack_id("fr");
        for (language, module, pack, why) in [
            ("fr", "radio", ours.as_str(), "not installed"),
            ("de", "radio", "", "installed, another language"),
            ("fr", "mpd", "", "installed, another module"),
        ] {
            let (state, mut rx, theirs, _dir) = state_with_preferences(4);
            let pack = if pack.is_empty() { theirs.as_str() } else { pack };
            let expected = catalog_message(&state, "language_preference_not_installed").await;
            let registry = state.registry.clone();
            let r = router(state)
                .oneshot(put_preference(language, serde_json::json!({ "module": module, "pack": pack })))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY, "{why}");
            assert_eq!(error_of(r).await, expected, "{why}");
            assert!(rx.recv().await.is_none(), "nothing told to the core: {why}");
            assert!(registry.read().await.preferences().is_empty(), "{why}");
        }
    }

    /// A stored list already past the ceiling (hand-edited, say): a new key
    /// is refused with its own message; replacing a stored key, and a
    /// removal, are still accepted (Important 1, part b).
    /// **[MUTATION]** drop the ceiling check: the new key answers 204, red.
    /// **[MUTATION]** drop "the list grows": the replacement answers 422, red.
    #[tokio::test]
    async fn past_the_ceiling_a_new_key_is_refused_but_a_replacement_and_a_removal_are_not() {
        let max = crate::state::PACK_PREFERENCES_MAX;
        let filled = |theirs: &str| -> Prefs {
            let mut v: Prefs = (0..max).map(|i| pref("fr", &format!("m{i}"), "gone")).collect();
            v.push(pref("fr", "core", theirs));
            v
        };

        let (state, mut rx, theirs, _dir) = state_with_preferences(4);
        state.registry.write().await.set_preferences(filled(&theirs));
        let expected = catalog_message(&state, "language_preference_full").await;
        assert!(expected.contains(&max.to_string()), "the message names the ceiling: {expected}");
        let registry = state.registry.clone();
        let r = router(state)
            .oneshot(put_preference("fr", serde_json::json!({ "module": "radio", "pack": theirs })))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error_of(r).await, expected);
        assert!(rx.recv().await.is_none());
        assert_eq!(registry.read().await.preferences().len(), max + 1, "nothing added");

        for body in [
            serde_json::json!({ "module": "core", "pack": theirs }),
            serde_json::json!({ "module": "m0", "pack": null }),
        ] {
            let (state, mut rx, theirs, _dir) = state_with_preferences(4);
            state.registry.write().await.set_preferences(filled(&theirs));
            let r = router(state).oneshot(put_preference("fr", body.clone())).await.unwrap();
            assert_eq!(r.status(), StatusCode::NO_CONTENT, "{body}");
            assert!(rx.recv().await.is_some(), "{body}");
        }
    }

    /// The core is told first: a core that cannot be told leaves the registry
    /// as it was.
    /// **[MUTATION]** write the registry before the send: red.
    #[tokio::test]
    async fn a_core_that_cannot_be_told_leaves_the_preferences_untouched() {
        let (state, rx, theirs, _dir) = state_with_preferences(4);
        drop(rx);
        let registry = state.registry.clone();
        let r = router(state)
            .oneshot(put_preference("fr", serde_json::json!({ "module": "radio", "pack": theirs })))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!error_of(r).await.is_empty());
        assert!(registry.read().await.preferences().is_empty());
    }
}
