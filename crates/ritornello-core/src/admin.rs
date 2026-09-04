use crate::status::AppState;
use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

/// Abstraction of the admin operations the core's routes need.
/// Implemented by `AdminClient` (real IPC); a fake implements it in tests.
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>>;
    /// `lang = None`: the plugin's current language. `Some(l)`: that language
    /// explicitly, rebuilt by the plugin regardless of its current one.
    async fn catalog(&self, lang: Option<&str>) -> Result<serde_json::Value>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>;
    /// Probe at 500 ms, without a lock on the plugin side: `Err(Timeout)` =
    /// busy, `Err(Closed)` = dead.
    async fn ping(&self) -> Result<()>;
}

#[async_trait::async_trait]
impl AdminBackend for ritornello_plugin_sdk::AdminClient {
    async fn ping(&self) -> Result<()> {
        ritornello_plugin_sdk::AdminClient::ping(self).await
    }
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
        self.get_asset(path).await
    }
    async fn catalog(&self, lang: Option<&str>) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_catalog(self, lang).await
    }
    async fn get_data(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_data(self).await
    }
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        ritornello_plugin_sdk::AdminClient::set_data(self, data).await
    }
}

/// Reachable admin pages, by plugin name.
///
/// Under a lock, and no longer frozen at startup: a plugin may announce itself
/// **after** the rendezvous (see `register`), and its page must then appear
/// without restarting the core. The `RwLock` is tokio's, like the rest of the
/// state shared with the router.
///
/// The routes never hold the lock across an IPC round trip: they clone the
/// backend's `Arc` and release at once.
pub type AdminBackends =
    std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<dyn AdminBackend>>>>;

/// UI assets already fetched, by `(plugin, path)` → `(mime, body, etag)`. A
/// bundle is immutable for the lifetime of the plugin's process: it is not
/// re-read over IPC at every page reload.
///
/// **"For the lifetime of the plugin's process" is an invariant, not a
/// remark**, and nobody upheld it: nothing purged this cache when that process
/// stopped. A plugin relaunched by hand with its rebuilt `ui.js` therefore
/// served the old one until the core restarted — which stings mostly in
/// development, precisely where that is the common gesture. `forget_page` is
/// what upholds it now.
pub type AssetCache = tokio::sync::RwLock<
    std::collections::HashMap<(String, String), (String, String, String)>,
>;

/// Plugin catalogs already fetched, by `(plugin, lang)`. Values are the
/// flattened JSON catalog, exactly as the plugin returned it: no ETag, no
/// mime, unlike `AssetCache` — the whole response is small JSON,
/// re-serializing it costs nothing worth caching separately.
///
/// Keyed by language and not merely by plugin, on purpose: the whole point of
/// serving these under `immutable` is that the URL fully determines the
/// content, and a URL that names `fr` must never answer with what was
/// fetched for `en`.
///
/// **No entry for "no language requested".** That request means "whatever
/// language the plugin is currently in", and that is mutable within a run —
/// `Core::set_locale` pushes `SetLocale` to every plugin. Caching it here,
/// under some placeholder key, would freeze a stale language until the
/// plugin's process restarts, with nothing in this cache able to invalidate
/// it. Every entry that *does* exist here is therefore for a `lang` the
/// caller named explicitly, which is exactly the case where the answer truly
/// cannot change during this run.
///
/// Bounded by construction: `admin_i18n` only ever inserts a `lang` that
/// passed `list_locales` (the installed packs), so the key space is a
/// handful of entries per plugin — `valid_locale`'s charset alone (16
/// characters from a 64-character alphabet) would otherwise admit far more
/// than that, each a potential cache entry and a potential IPC round trip to
/// the plugin — see `admin_i18n`'s doc.
pub type CatalogCache = tokio::sync::RwLock<std::collections::HashMap<(String, String), serde_json::Value>>;

/// Forgets everything the core keeps of the admin page of `name`: its backend
/// and its cached assets and catalogs.
///
/// **A single purge point, called everywhere the plugin's process stops** —
/// death observed by supervision, death inferred from the sockets closing,
/// requested shutdown, and re-announcement (which is the end of one process
/// followed by the start of another). It is deliberately a function and not
/// two copied lines: both registries must fall *together*, and an invariant
/// whose correctness depends on four purge sites ends up lying at one of them.
///
/// What removing the backend buys: `/api/admin/<name>` answers a frank 404 —
/// "unknown plugin" — instead of an IPC round trip on a closed socket. The
/// failure there was fast (writing to a socket whose peer closed returns
/// `EPIPE` right away), so the gain is not latency except in a narrow race: if
/// the write enters the buffer before the close is processed, the answer never
/// arrives and the request's whole budget elapses. The real gain is telling the
/// truth.
pub async fn forget_page(backends: &AdminBackends, assets: &AssetCache, catalogs: &CatalogCache, name: &str) {
    backends.write().await.remove(name);
    // `retain` and not `remove`: the key carries the asset path (resp. the
    // language), so a plugin has as many entries as files it served (resp.
    // languages it was asked in).
    assets.write().await.retain(|(plugin, _), _| plugin != name);
    catalogs.write().await.retain(|(plugin, _), _| plugin != name);
}

fn etag_of(body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    format!("\"{:x}\"", h.finish())
}

/// Response to a failure of the admin dialog with a plugin.
///
/// In a single place because the four admin routes did the same thing in the
/// same faulty way: log the cause, then return a 502 whose body was the raw
/// text "plugin unreachable". The web client only reads `{"error": …}`; a raw
/// text body made it fall back on "HTTP 502", a bare code on screen for a
/// failure whose cause was known one line above.
async fn plugin_refusal(st: &AppState, name: &str, context: &str, e: &anyhow::Error) -> Response {
    // The log keeps the **whole** cause, in English: it is what serves remote
    // diagnosis, and it is often more precise than the displayed sentence.
    tracing::warn!("plugin {name} admin unreachable ({context}): {e}");
    // The HTTP code follows the cause, like the message: 504 when time ran
    // out, 502 when the plugin did.
    let (code, key) = match e.downcast_ref::<ritornello_plugin_sdk::AdminIpcError>() {
        // Alive but too slow: saying "unreachable" would send one to restart a
        // running process, instead of looking at the network.
        Some(ritornello_plugin_sdk::AdminIpcError::Timeout) => (StatusCode::GATEWAY_TIMEOUT, "plugin_timeout"),
        _ => (StatusCode::BAD_GATEWAY, "plugin_unreachable"),
    };
    let msg = st.catalog.read().await.get(key).to_string();
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// `ui.js` or `ui.css` of a plugin. The file name comes from the route path,
/// never from a hard-coded list: the core does not know what a plugin exposes.
pub async fn admin_asset(
    State(st): State<AppState>,
    Path((name, file)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    // Cloned then lock released: what follows makes IPC round trips, and
    // holding them under a read lock would delay the insertion of a plugin
    // announcing itself late.
    let Some(backend) = st.admin_backends.read().await.get(&name).cloned() else {
        return (StatusCode::NOT_FOUND, "unknown plugin").into_response();
    };
    let key = (name.clone(), file.clone());
    let cached = st.admin_assets.read().await.get(&key).cloned();
    let (mime, body, etag) = match cached {
        Some(v) => v,
        None => match backend.asset(&file).await {
            Ok(Some((mime, body))) => {
                let etag = etag_of(&body);
                let v = (mime, body, etag);
                st.admin_assets.write().await.insert(key, v.clone());
                v
            }
            Ok(None) => return (StatusCode::NOT_FOUND, "unknown asset").into_response(),
            Err(e) => return plugin_refusal(&st, &name, &format!("asset {file}"), &e).await,
        },
    };
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    // Same rule as the shell's own bundles (`web.rs::cache_control_for`),
    // sharing its header value (`web::IMMUTABLE_CACHE_CONTROL`) though not the
    // check itself: two routers with no state in common, and each decides on
    // its own terms whether this exact URL is stamped. A version in the query
    // means the URL identifies this exact content, so it never needs
    // revalidating.
    let cache_control = if uri.query().is_some_and(|q| q.split('&').any(|p| p.starts_with("v="))) {
        crate::web::IMMUTABLE_CACHE_CONTROL
    } else {
        "no-cache"
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, mime.as_str()),
            (axum::http::header::CACHE_CONTROL, cache_control),
            (axum::http::header::ETAG, etag.as_str()),
        ],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct CatalogQuery {
    /// The plugin's current language when absent — same convention as
    /// `AdminBackend::catalog`. Validated against **two** gates before it is
    /// trusted for anything (see `admin_i18n`): `valid_locale`, the same
    /// charset/length rule `PUT /api/locale` already enforces, and membership
    /// in `list_locales`, the installed packs.
    lang: Option<String>,
    /// Presence, together with `lang`'s, is what matters: like `admin_asset`'s
    /// own `v`, it says the URL was stamped by a caller who can name a fresh
    /// one when the content changes, so this exact URL never needs
    /// revalidating. But `v` alone does not: see `admin_i18n`.
    v: Option<String>,
}

/// `GET /plugins/<name>/api/i18n[?lang=<l>][&v=<stamp>]`.
///
/// Cached by `(name, lang)`, but **only when `lang` is present** — see
/// `CatalogCache`'s doc for why the plugin's ambient "current language" must
/// never be cached. Marked `immutable` only when **both** `lang` and `v` are
/// present: `immutable` is a promise that can never be withdrawn, so it may
/// only be made about a response the URL fully determines — `v` alone still
/// leaves "whichever language the plugin happens to be in right now"
/// unresolved. The historic, unversioned URL (neither parameter) must remain
/// usable by a client that has not been updated, and must never be told it
/// can cache the answer forever.
///
/// `lang` is refused with `400` **before** touching the plugin or the cache
/// unless it passes both:
/// - `valid_locale` (`crate::status::valid_locale`) — the same rule
///   `PUT /api/locale` already enforces, reused rather than reinvented: a
///   stricter, purpose-built "plain locale" grammar tried here first was
///   *wrong* — it rejected `pt-BR`-shaped codes that are legitimate
///   selectable languages, which would have silently stripped a plugin page
///   of its translations the moment the shell requested one.
/// - membership in `list_locales(&st.locales_root)` — the operator-facing
///   list of what is actually installed. This is what bounds `CatalogCache`:
///   `valid_locale`'s charset alone (16 characters from a 64-character
///   alphabet) admits far more distinct strings than that, each a potential
///   cache entry and a potential IPC round trip to the plugin — an
///   unauthenticated caller on the LAN could otherwise grow the core's memory
///   without bound and monopolize a plugin's admin socket. Restricting to
///   installed packs shrinks the key space to a handful per plugin, and
///   guarantees the core never refuses a language it advertises through
///   `/api/status`'s own `locale` field.
///
/// Refusing outright (rather than falling back to `None` as the SDK's own
/// boundary does, `ritornello_plugin_sdk::is_plain_locale`) costs nothing
/// here: a well-formed, installed `lang` is the shell's own doing, never
/// typed by a person, so anything else only ever means a caller bug or a
/// stale/tampered URL, neither of which deserves a cached answer.
pub async fn admin_i18n(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<CatalogQuery>,
) -> Response {
    if let Some(lang) = &q.lang
        && (!crate::status::valid_locale(lang) || !crate::status::list_locales(&st.locales_root).iter().any(|l| l == lang))
    {
        return (StatusCode::BAD_REQUEST, "invalid or uninstalled language").into_response();
    }
    // The lock is released **before** the IPC round trip: a temporary in the
    // scrutinee of a `match` would live until the end of the match, hence
    // during the call to the plugin.
    let backend = st.admin_backends.read().await.get(&name).cloned();
    let Some(backend) = backend else {
        return (StatusCode::NOT_FOUND, "unknown plugin").into_response();
    };
    let value = match &q.lang {
        // Named language: cacheable, since it cannot change underneath the
        // key — the plugin rebuilds it fresh for exactly this language on
        // every miss, regardless of what it is currently running in.
        Some(lang) => {
            let key = (name.clone(), lang.clone());
            let cached = st.admin_catalogs.read().await.get(&key).cloned();
            match cached {
                Some(v) => v,
                None => match backend.catalog(Some(lang.as_str())).await {
                    Ok(v) => {
                        st.admin_catalogs.write().await.insert(key, v.clone());
                        v
                    }
                    Err(e) => return plugin_refusal(&st, &name, "catalog", &e).await,
                },
            }
        }
        // No language named: "whatever the plugin is currently in", which
        // `Core::set_locale` can change mid-run. Never cached — see
        // `CatalogCache`'s doc.
        None => match backend.catalog(None).await {
            Ok(v) => v,
            Err(e) => return plugin_refusal(&st, &name, "catalog", &e).await,
        },
    };
    // `immutable` only when the URL names *both* the language and a stamp:
    // either alone still leaves part of the answer undetermined by the URL.
    let cache_control = if q.lang.is_some() && q.v.is_some() {
        crate::web::IMMUTABLE_CACHE_CONTROL
    } else {
        "no-cache"
    };
    ([(axum::http::header::CACHE_CONTROL, cache_control)], Json(value)).into_response()
}

pub async fn admin_get_data(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    // The lock is released **before** the IPC round trip: a temporary in the
    // scrutinee of a `match` would live until the end of the match, hence
    // during the call to the plugin.
    let backend = st.admin_backends.read().await.get(&name).cloned();
    match backend {
        None => (StatusCode::NOT_FOUND, "unknown plugin").into_response(),
        Some(backend) => match backend.get_data().await {
            Ok(value) => Json(value).into_response(),
            Err(e) => plugin_refusal(&st, &name, "get_data", &e).await,
        },
    }
}

pub async fn admin_put_data(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(data): Json<serde_json::Value>,
) -> Response {
    // The lock is released **before** the IPC round trip: a temporary in the
    // scrutinee of a `match` would live until the end of the match, hence
    // during the call to the plugin.
    let backend = st.admin_backends.read().await.get(&name).cloned();
    match backend {
        None => (StatusCode::NOT_FOUND, "unknown plugin").into_response(),
        Some(backend) => match backend.set_data(data).await {
            Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
            Ok(Err(msg)) => (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response(),
            Err(e) => plugin_refusal(&st, &name, "set_data", &e).await,
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
        /// The plugin answers, but beyond the 5 s cap. Distinct from `down`
        /// precisely because the returned message must be too.
        slow: bool,
        asset_calls: Arc<std::sync::atomic::AtomicUsize>,
        catalog_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Fake {
        async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            self.asset_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(match path {
                "ui.js" => Some(("text/javascript".to_string(), "export const contract = 1".to_string())),
                _ => None,
            })
        }
        async fn catalog(&self, lang: Option<&str>) -> Result<serde_json::Value> {
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            self.catalog_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // `lang` carried along (rather than only "Enregistrer"): a test
            // proving the cache is keyed by language needs the plugin's
            // answer to actually depend on it, or a defect that serves the
            // wrong entry under the right key would go unnoticed.
            Ok(serde_json::json!({ "btn_save": "Enregistrer", "lang": lang }))
        }
        async fn get_data(&self) -> Result<serde_json::Value> {
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "stations": [] }))
        }
        async fn set_data(&self, _data: serde_json::Value) -> Result<Result<(), String>> {
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(if self.reject { Err("duplicate preset".into()) } else { Ok(()) })
        }
        async fn ping(&self) -> Result<()> {
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(())
        }
    }

    /// Default rig: only `en` is "installed" (see `list_locales`'s fallback
    /// on an absent `core/` directory), which is enough for every test that
    /// does not itself exercise the installed-language bound.
    fn state_with(fake: Fake) -> AppState {
        state_with_locales_root(fake, std::path::PathBuf::from("/nonexistent"))
    }

    /// Variant with a chosen `locales_root`, for the tests that need more
    /// than the always-present `en` to be "installed" — see
    /// `two_languages_are_two_entries`.
    fn state_with_locales_root(fake: Fake, locales_root: std::path::PathBuf) -> AppState {
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
                crate::i18n::EN,
            ))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            locales_root,
            admin_backends: Arc::new(tokio::sync::RwLock::new(backends)),
            admin_assets: Arc::new(Default::default()),
            admin_catalogs: Arc::new(Default::default()),
            session: "test-session".to_string(),
            cmd_tx,
            player: crate::status::tests_support::inert_player(),
            sources_catalog: tokio::sync::watch::channel(ritornello_proto::SourcesCatalog::default()).1,
            theme_current: Arc::new(tokio::sync::RwLock::new(Default::default())),
            theme_tx: tokio::sync::mpsc::channel(4).0,
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
            system: Default::default(),
            covers: Arc::new(crate::cover::CoverCache::new()),
            plugins: Arc::new(crate::status::PluginsControl {
                manifest: std::path::PathBuf::from("/nonexistent"),
                names: Vec::new(),
                tx: tokio::sync::mpsc::channel(1).0,
            }),
        }
    }

    #[tokio::test]
    async fn ui_js_is_served_with_its_type_and_an_etag() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/javascript");
        assert!(resp.headers().contains_key("etag"));
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("contract"));
    }

    #[tokio::test]
    async fn ui_js_is_cached_after_the_first_access() {
        // A bundle is immutable for the lifetime of the plugin's process:
        // re-reading it over IPC at every page reload would be waste.
        let fake = Fake::default();
        let calls = fake.asset_calls.clone();
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
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn forget_page_gives_a_frank_404_and_re_reads_after_a_re_announcement() {
        // Three properties at once, and all through **observed behavior**
        // rather than the content of a table: what matters is not that a key
        // disappeared, it is what the route answers next.
        let fake = Fake::default();
        let calls = fake.asset_calls.clone();
        let state = state_with(fake);
        let app = router(state.clone());

        let get = |app: axum::Router| async move {
            app.oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
                .await
                .unwrap()
        };

        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "cached");

        // 1. Purging **another** plugin takes nothing away here. The cache key
        //    carries `(plugin, path)`, so the purge goes through a `retain`:
        //    getting the wrong half of the key would have emptied the whole
        //    cache.
        forget_page(&state.admin_backends, &state.admin_assets, &state.admin_catalogs, "autre").await;
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "still cached");

        // 2. Once the plugin is forgotten, the route says frankly that there is
        //    nothing there — that is the half of the fix that removes the dead
        //    page from the menu instead of returning an IPC error.
        forget_page(&state.admin_backends, &state.admin_assets, &state.admin_catalogs, "radio").await;
        assert_eq!(get(app.clone()).await.status(), StatusCode::NOT_FOUND);

        // 3. And a re-announcement really re-reads: that is the `hotplug`
        //    sequence — forget, then rewire. Without the asset purge, the
        //    plugin relaunched with a rebuilt `ui.js` still served the old one
        //    until the core restarted.
        state
            .admin_backends
            .write()
            .await
            .insert("radio".into(), Arc::new(Fake { asset_calls: calls.clone(), ..Default::default() }));
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the new process must be re-read, not served from the old one's cache"
        );
    }

    #[tokio::test]
    async fn if_none_match_answers_304() {
        let app = router(state_with(Fake::default()));
        let first = app
            .clone()
            .oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let etag = first.headers()["etag"].to_str().unwrap().to_string();
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
    async fn a_plugin_asset_asked_with_a_version_is_immutable() {
        // Same rule as the shell's own bundles: a versioned URL identifies its
        // content, so it never needs revalidating.
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/ui.js?v=cafe").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let cc = resp.headers()[axum::http::header::CACHE_CONTROL].to_str().unwrap();
        assert!(cc.contains("immutable"), "{cc}");
    }

    #[tokio::test]
    async fn an_asset_unknown_to_the_plugin_answers_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.css").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_plugin_catalog_is_served_flat() {
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
    async fn a_catalog_asked_in_a_language_is_served_immutable() {
        // The URL fully determines the content — that is what `immutable`
        // claims. `en` is always "installed" (see `list_locales`), so this
        // needs no extra fixture.
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?lang=en&v=abc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cc = resp.headers()[axum::http::header::CACHE_CONTROL].to_str().unwrap();
        assert!(cc.contains("immutable"), "{cc}");
    }

    #[tokio::test]
    async fn a_catalog_without_a_language_is_not_frozen() {
        // Historic URL, still reachable: no version, so no `immutable` — a
        // client that has not been updated must not be stuck for good.
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let cc = resp.headers().get(axum::http::header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(!cc.contains("immutable"), "{cc}");
    }

    #[tokio::test]
    async fn a_stamp_without_a_language_is_still_not_immutable() {
        // Ruling (f): `?v=x` alone still leaves "whatever language the
        // plugin is currently in" unresolved by the URL — `immutable` is a
        // promise that can never be withdrawn, so it must not be made about
        // an answer only partly determined by the query string.
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?v=abc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cc = resp.headers().get(axum::http::header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(!cc.contains("immutable"), "{cc}");
    }

    #[tokio::test]
    async fn an_unspecified_language_is_never_cached() {
        // Ruling (e), a regression this task's first cut introduced: with no
        // `lang`, the answer means "whatever language the plugin is
        // currently in", which `Core::set_locale` can change mid-run by
        // pushing `SetLocale` to it. Caching that under a placeholder key,
        // with nothing here able to invalidate it, would leave a plugin page
        // stuck in the old language after a switch until its process
        // restarted — strictly worse than before this task, which always
        // asked the plugin.
        let fake = Fake::default();
        let calls = fake.catalog_calls.clone();
        let app = router(state_with(fake));
        for _ in 0..2 {
            let resp = app
                .clone()
                .oneshot(Request::get("/plugins/radio/api/i18n").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the plugin's current language can change between two calls; caching it would risk serving a stale one"
        );
    }

    #[tokio::test]
    async fn a_catalog_asked_twice_hits_the_plugin_once() {
        // The measured waste: every visit to a plugin page cost a full round
        // trip to its process for a catalog that never changes within a
        // session.
        let fake = Fake::default();
        let calls = fake.catalog_calls.clone();
        let app = router(state_with(fake));
        for _ in 0..2 {
            let resp = app
                .clone()
                .oneshot(Request::get("/plugins/radio/api/i18n?lang=en&v=abc").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn two_languages_are_two_entries() {
        // The cache is keyed by `(plugin, language)`: serving French under
        // the English URL is exactly the lie `immutable` must never tell.
        // `fr` needs an actual installed pack (ruling (b)): the default
        // fixture only advertises `en`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "").unwrap();
        let fake = Fake::default();
        let calls = fake.catalog_calls.clone();
        let app = router(state_with_locales_root(fake, dir.path().to_path_buf()));
        for lang in ["fr", "en"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::get(format!("/plugins/radio/api/i18n?lang={lang}&v=abc"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            // Not just the call count: a cache handing back `fr`'s value
            // under the `en` key (or vice versa) once both are populated
            // would still pass a count-only assertion.
            assert_eq!(v["lang"], lang, "served the wrong language's entry for ?lang={lang}");
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_malformed_language_is_refused_and_never_marked_immutable() {
        // Charset/length violation (`valid_locale`'s own rule, reused rather
        // than a bespoke grammar) — refused outright, before ever touching
        // the plugin, regardless of whether anything is installed under that
        // name.
        let fake = Fake::default();
        let calls = fake.catalog_calls.clone();
        let app = router(state_with(fake));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?lang=..&v=abc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let cc = resp.headers().get(axum::http::header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(!cc.contains("immutable"), "{cc}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "the plugin must never see a bad language");
    }

    #[tokio::test]
    async fn a_locale_outside_the_installed_set_is_refused() {
        // Ruling (b): well-shaped (`pt-BR`-like codes pass `valid_locale`
        // fine) but not among what the operator actually installed — refused
        // just the same, and for the same reason the bound on `CatalogCache`
        // depends on: an unauthenticated LAN caller must not be able to grow
        // the cache, or hammer a plugin's admin socket, with an unbounded set
        // of "valid-shaped" language strings. The default fixture only
        // advertises `en`.
        let fake = Fake::default();
        let calls = fake.catalog_calls.clone();
        let app = router(state_with(fake));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?lang=fr&v=abc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "the plugin must never see an uninstalled language");
    }

    #[tokio::test]
    async fn forget_page_also_purges_the_catalog_cache() {
        // Same defect as the one `forget_page` already fixed for assets: a
        // plugin that dies and comes back with a different catalog must not
        // keep serving the old one from the core's cache.
        let fake = Fake::default();
        let calls = fake.catalog_calls.clone();
        let state = state_with(fake);
        let app = router(state.clone());

        let get = |app: axum::Router| async move {
            app.oneshot(Request::get("/plugins/radio/api/i18n?lang=en&v=abc").body(Body::empty()).unwrap())
                .await
                .unwrap()
        };

        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "cached");

        forget_page(&state.admin_backends, &state.admin_assets, &state.admin_catalogs, "radio").await;
        state
            .admin_backends
            .write()
            .await
            .insert("radio".into(), Arc::new(Fake { catalog_calls: calls.clone(), ..Default::default() }));
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the new process must be re-read, not served from the old one's cache"
        );
    }

    #[tokio::test]
    async fn ui_js_of_an_unknown_plugin_answers_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/inconnu/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_admin_page_remains_served_by_the_spa() {
        // Point of vigilance: the new `/plugins/:name/:file` route must not
        // capture `/plugins/<name>/` (empty final segment), which must keep
        // falling onto the fallback and serve the shell — it is the historical
        // URL, present in the README and in the status page's links.
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
    }

    #[tokio::test]
    async fn get_data_relays_the_json() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["stations"].is_array());
    }

    #[tokio::test]
    async fn valid_put_data_returns_204() {
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
    async fn invalid_put_data_returns_422_with_a_message() {
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
        assert_eq!(v["error"], "duplicate preset");
    }

    // Since Task 10, an unknown plugin name on the *asset* route
    // (`/plugins/<name>/ui.js`) 404s (see `ui_js_of_an_unknown_plugin_answers_404`),
    // and `/plugins/<name>/` (empty final segment, historical URL) falls onto
    // the SPA fallback (see `the_admin_page_remains_served_by_the_spa`), which
    // always returns the shell whatever the name. The *data* (`api/data`)
    // stays strict: an unknown plugin name still 404s there, so as never to
    // mask a typo behind a 200 response.
    #[tokio::test]
    async fn unknown_plugin_serves_the_shell() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/inconnu/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_plugin_on_the_data_api_answers_404() {
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/inconnu/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_unreachable_plugin_says_why_instead_of_a_bare_code() {
        // Reported symptom: the screen showed "HTTP 502". The web client only
        // knows how to read `{"error": …}`; a raw text body made it fall back
        // on the code, while the cause was known.
        let app = router(state_with(Fake { down: true, ..Default::default() }));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        let msg = json["error"].as_str().expect("error field");
        // A sentence, not a catalog key: the key-by-key fallback of
        // `Catalog::get` is silent, and a bare key would be displayed as is.
        assert!(msg.contains(' '), "raw key returned to the screen: {msg}");
    }

    #[tokio::test]
    async fn a_too_slow_plugin_is_not_called_unreachable() {
        // Two distinct failures, two courses of action: a dead plugin calls
        // for a restart, a too slow plugin sends one to look at the network.
        // The core flattened them into a single message.
        let slow = router(state_with(Fake { slow: true, ..Default::default() }));
        let r1 = slow
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::GATEWAY_TIMEOUT);
        let c1 = r1.into_body().collect().await.unwrap().to_bytes();
        let m1 = serde_json::from_slice::<serde_json::Value>(&c1).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();

        let dead = router(state_with(Fake { down: true, ..Default::default() }));
        let r2 = dead
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let c2 = r2.into_body().collect().await.unwrap().to_bytes();
        let m2 = serde_json::from_slice::<serde_json::Value>(&c2).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();

        assert_ne!(m1, m2, "the exceeded timeout and the failure give the same message");
        assert!(m1.contains(' ') && m2.contains(' '), "raw key: {m1} / {m2}");
    }

    #[tokio::test]
    async fn a_too_slow_plugin_gives_504_and_a_dead_plugin_502() {
        let slow = router(state_with(Fake { slow: true, ..Default::default() }));
        let r1 = slow.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r1.status(), StatusCode::GATEWAY_TIMEOUT);
        let dead = router(state_with(Fake { down: true, ..Default::default() }));
        let r2 = dead.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r2.status(), StatusCode::BAD_GATEWAY);
    }
}
