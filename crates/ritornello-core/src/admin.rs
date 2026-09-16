use crate::status::AppState;
use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ritornello_proto::Text;
use serde::Deserialize;

/// Abstraction of the admin operations the core's routes need.
/// Implemented by `AdminClient` (real IPC); a fake implements it in tests.
///
/// **No `catalog` method.** Until task 5, `admin_i18n` fetched a plugin's
/// catalogue through this trait, over IPC. It now resolves the same
/// catalogue from the shared `Registry` (task 4), purely in memory — see
/// `admin_i18n`'s doc — so that operation no longer belongs to the set of
/// admin calls this trait exists to abstract. Task 6 went further and
/// removed the wire protocol itself: `AdminReq::GetCatalog`,
/// `AdminClient::get_catalog` and `AdminPlugin::catalog`, in
/// `ritornello-plugin-sdk`, no longer exist. A catalogue is now announced
/// once (`Announcement.catalog`, task 3) and never requested.
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    /// `Err` carries the refusal **unresolved** — see
    /// `ritornello_plugin_sdk::AdminClient::set_data`'s own doc for what
    /// that means and why. `admin_put_data` is what resolves it, the same
    /// way `admin_i18n` resolves a plugin's whole catalog: both read
    /// `AppState.registry`, neither performs any IPC to do it.
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), Text>>;
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
    async fn get_data(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_data(self).await
    }
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), Text>> {
        ritornello_plugin_sdk::AdminClient::set_data(self, data).await
    }
}

/// Resolves an admin refusal's `Text` into the finished string the browser's
/// `PUT` caller still expects in `{"error": ...}` (see `web/kit/src/api.ts`,
/// which reads that field and nothing else).
///
/// **Why server-side and not left to the browser, unlike `FilesAdmin.vue`'s
/// stored explore error (task 9).** `AdminResult::Set.error_text` is a
/// **structured** field on the wire the core already parses — unlike
/// `GetData`'s payload, which the core relays as opaque JSON because it does
/// not know a plugin's own data shape. Resolving here costs one registry
/// lookup, the same one `admin_i18n` already performs for a plugin's whole
/// catalog, and keeps the browser-facing contract of this route completely
/// unchanged: no plugin admin page needed touching for its *save* path to
/// keep working across a language change.
async fn resolve_admin_text(st: &AppState, module: &str, text: &Text) -> String {
    match text {
        Text::Verbatim(s) => s.clone(),
        Text::Keyed { key, params } => {
            let locale = st.locale_current.read().await.clone().unwrap_or_else(|| "en".to_string());
            // The device's own fallback (task 13), not a hardcoded "en": a
            // plugin's save error must honour the same setting the core's
            // own status line and the plugin's admin catalog (`admin_i18n`,
            // below) already do — see `AppState.fallback_current`'s doc.
            let fallback = st.fallback_current.read().await.clone().unwrap_or_else(|| "en".to_string());
            let resolved = {
                let registry = st.registry.read().await;
                registry.chain_for(module, &locale, &fallback).get(key).to_string()
            };
            ritornello_i18n::interpolate(&resolved, params.iter().map(|(name, value)| (name.as_str(), value.as_str())))
        }
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

/// Forgets everything the core keeps of the admin page of `name`: its
/// backend, its cached assets, and its announced translation layers.
///
/// **A single purge point, called everywhere the plugin's process stops** —
/// death observed by supervision, death inferred from the sockets closing,
/// requested shutdown, and re-announcement (which is the end of one process
/// followed by the start of another). It is deliberately a function and not
/// three copied lines: every one of these registries must fall *together*,
/// and an invariant whose correctness depends on remembering to purge it at
/// several call sites ends up lying at one of them (the lesson `main.rs`
/// already records for `kill_triggers`, applied here by never letting that
/// choice exist in the first place).
///
/// **No plugin catalog cache to purge here any more.** Until task 5, this
/// function also emptied `CatalogCache`, a store of already-fetched plugin
/// catalogues keyed by `(plugin, lang)` and deliberately bounded — each entry
/// was a potential IPC round trip to the plugin, and an unauthenticated
/// caller on the LAN could otherwise grow the core's memory without bound
/// and monopolize a plugin's admin socket by naming `lang` values from
/// `valid_locale`'s own alphabet. `admin_i18n` no longer fetches a catalogue
/// over IPC at all — it resolves one from the shared `Registry` (task 4),
/// which this function already keeps current via `registry.forget` below —
/// so that risk, and the cache built to bound it, are both gone. See
/// `admin_i18n`'s own doc for the replacement.
///
/// What removing the backend buys: `/api/admin/<name>` answers a frank 404 —
/// "unknown plugin" — instead of an IPC round trip on a closed socket. The
/// failure there was fast (writing to a socket whose peer closed returns
/// `EPIPE` right away), so the gain is not latency except in a narrow race: if
/// the write enters the buffer before the close is processed, the answer never
/// arrives and the request's whole budget elapses. The real gain is telling the
/// truth.
///
/// The registry's `forget` is unconditional here, even on a path that will
/// `insert_announced` again right afterwards (`hotplug`'s re-announcement):
/// forgetting first and re-inserting only if the new announcement actually
/// carries a catalogue is what keeps a plugin that regressed to an older
/// binary (`catalog: None`) from keeping a stale, no-longer-true layer.
pub async fn forget_page(
    backends: &AdminBackends,
    assets: &AssetCache,
    registry: &crate::i18n::Shared,
    name: &str,
) {
    backends.write().await.remove(name);
    // `retain` and not `remove`: the key carries the asset path, so a plugin
    // has as many entries as files it served.
    assets.write().await.retain(|(plugin, _), _| plugin != name);
    registry.write().await.forget(name);
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
    /// The core's current interface language when absent (`AppState.locale_current`,
    /// falling back to `en`) — see `admin_i18n`. Still validated by
    /// `valid_locale` before use: the value is a query parameter, and
    /// refusing early what is not a language code stays right even though
    /// nothing here can grow unboundedly any more (see `admin_i18n`'s doc).
    lang: Option<String>,
}

/// `GET /plugins/<name>/api/i18n[?lang=<l>]`.
///
/// **Resolved from the shared `Registry` (task 4), with no IPC and no
/// cache.** Until task 5, an absent `lang` meant an IPC round trip asking
/// the plugin its own current language, and a named `lang` meant a round
/// trip cached in `CatalogCache` — bounded on purpose, because each entry was
/// a potential IPC call: an unauthenticated caller on the LAN could
/// otherwise grow the core's memory without bound and monopolize a plugin's
/// admin socket by naming `lang` values from `valid_locale`'s own alphabet.
/// `Registry::chain_for` performs no I/O and holds nothing extra per call —
/// it reads tiers the registry already keeps in memory (see its own doc) —
/// so there is nothing left to bound: any well-shaped `lang` costs one
/// `HashMap` lookup, not a socket round trip, and membership in the
/// installed set is therefore no longer checked here either — it only ever
/// existed to bound the cache this route no longer has. `chain_for` already
/// answers gracefully, falling through to `en`, for a language nobody
/// installed.
///
/// `lang` absent means the core's own current interface language
/// (`AppState.locale_current`, defaulting to `en` exactly like
/// `status_json`'s own `locale` field) rather than "whatever the plugin
/// happens to be running in": a language never crosses the Source wire at
/// all (task 11 of the language-packs chantier retired
/// `SourceReq::SetLocale`), so this is the only language the core can name.
///
/// **Never `immutable`, unlike `admin_asset`.** Until task 5's fix round this
/// route was marked `immutable` when both `lang` and a `v` stamp were
/// present — a promise it could not actually keep: `chain_for` reads the
/// registry's `disk` tier, and `Registry::resweep` (triggered by every real
/// locale change) can replace what that tier holds without the core's
/// `session` stamp ever moving, so the same stamped URL could start
/// answering differently mid-session. The fix is not a longer key: the IPC
/// round trip that once made re-fetching this route costly is gone (see
/// above), so paying `no-cache`'s revalidation on every admin-page visit is
/// close to free, and `no-cache` is honest about what the URL actually
/// determines — nothing invented, nothing withdrawn. `admin_asset` keeps
/// `immutable` because its own justification is untouched: a `ui.js`/`ui.css`
/// bundle is still fetched over IPC once and held in `admin_assets` for the
/// plugin process's whole lifetime, so a versioned asset URL genuinely never
/// changes until that process restarts. See `AppState::session`'s doc for the
/// fuller account.
///
/// `lang`, when present, is refused with `400` before it is used for
/// anything unless it passes `valid_locale` (`crate::status::valid_locale`)
/// — the same rule `PUT /api/locale` already enforces, reused rather than
/// reinvented: a stricter, purpose-built "plain locale" grammar tried here
/// first was *wrong* — it rejected `pt-BR`-shaped codes that are legitimate
/// selectable languages, which would have silently stripped a plugin page of
/// its translations the moment the shell requested one.
pub async fn admin_i18n(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<CatalogQuery>,
) -> Response {
    if let Some(lang) = &q.lang
        && !crate::status::valid_locale(lang)
    {
        return (StatusCode::BAD_REQUEST, "invalid language").into_response();
    }
    if st.admin_backends.read().await.get(&name).is_none() {
        return (StatusCode::NOT_FOUND, "unknown plugin").into_response();
    }
    let chosen = match &q.lang {
        Some(lang) => lang.clone(),
        None => st.locale_current.read().await.clone().unwrap_or_else(|| "en".to_string()),
    };
    // The device's own fallback (task 13), not a hardcoded "en" — a plugin's
    // whole admin catalog must resolve through the same chosen → fallback →
    // English chain the core's own UI does, whether `chosen` came from the
    // query or from `locale_current`.
    let fallback = st.fallback_current.read().await.clone().unwrap_or_else(|| "en".to_string());
    let value = {
        let registry = st.registry.read().await;
        serde_json::json!(registry.chain_for(&name, &chosen, &fallback).entries())
    };
    // Always revalidated, never `immutable` — see this function's own doc.
    ([(axum::http::header::CACHE_CONTROL, "no-cache")], Json(value)).into_response()
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
            Ok(Err(text)) => {
                let msg = resolve_admin_text(&st, &name, &text).await;
                (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response()
            }
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
        /// Panics on **any** admin call. The observable proof that a route
        /// never talks to the plugin at all — see
        /// `the_plugin_catalog_route_serves_from_the_registry_without_any_ipc`,
        /// which is the reason this exists: a test that only checked the
        /// response body would still pass with the old IPC path in place.
        panics: bool,
        asset_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Fake {
        async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
            if self.panics { panic!("admin backend called: asset({path})") }
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            self.asset_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(match path {
                "ui.js" => Some(("text/javascript".to_string(), "export const contract = 1".to_string())),
                _ => None,
            })
        }
        async fn get_data(&self) -> Result<serde_json::Value> {
            if self.panics { panic!("admin backend called: get_data()") }
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "stations": [] }))
        }
        async fn set_data(&self, _data: serde_json::Value) -> Result<Result<(), Text>> {
            if self.panics { panic!("admin backend called: set_data()") }
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(if self.reject { Err(Text::Verbatim("duplicate preset".into())) } else { Ok(()) })
        }
        async fn ping(&self) -> Result<()> {
            if self.panics { panic!("admin backend called: ping()") }
            if self.slow { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(())
        }
    }

    /// Default rig: only `en` is "installed" (see
    /// `Registry::core_languages`'s fallback on an absent `core/`
    /// directory), which is enough for every test that does not itself
    /// exercise the installed-language bound.
    fn state_with(fake: Fake) -> AppState {
        state_with_locales_root(fake, std::path::PathBuf::from("/nonexistent"))
    }

    /// Variant with a chosen `locales_root`, for the tests that need more
    /// than the always-present `en` to be "installed", or a real on-disk
    /// pack for a plugin module — see `two_languages_are_two_entries`.
    ///
    /// The registry is swept from this root, exactly as `main.rs` wires it
    /// (one `Arc<RwLock<Registry>>` built from the same root the process's
    /// own `RITORNELLO_LOCALES` names): a plugin's on-disk pack lives at
    /// `<locales_root>/<plugin>/<lang>.toml`, in the same tree as
    /// `<locales_root>/core`. `AppState` itself no longer carries this path
    /// separately — task 12 removed `AppState.locales_root`, its last
    /// reader replaced by `Registry::core_languages`, which answers from
    /// the swept snapshot instead of a second, independent disk read.
    fn state_with_locales_root(fake: Fake, locales_root: std::path::PathBuf) -> AppState {
        let (audio_tx, _rx) = tokio::sync::mpsc::channel(4);
        let (locale_tx, _locale_rx) = tokio::sync::mpsc::channel(4);
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel(4);
        let mut backends: HashMap<String, Arc<dyn AdminBackend>> = HashMap::new();
        backends.insert("radio".into(), Arc::new(fake));
        AppState {
            status: Arc::new(tokio::sync::RwLock::new(StatusState {
                plugins: vec![],
                active_source: "radio".into(),
                protocol: ritornello_proto::PROTOCOL_VERSION,
            })),
            logs: Arc::new(LogBuffer::new(10)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
            catalog: Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Chain::load_for_tests(
                "core",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::i18n::EN,
            ))),
            registry: Arc::new(tokio::sync::RwLock::new(crate::i18n::Registry::sweep(locales_root))),
            locale_current: Arc::new(tokio::sync::RwLock::new(None)),
            locale_tx,
            fallback_current: Arc::new(tokio::sync::RwLock::new(None)),
            fallback_tx: tokio::sync::mpsc::channel(4).0,
            admin_backends: Arc::new(tokio::sync::RwLock::new(backends)),
            admin_assets: Arc::new(Default::default()),
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
                tx: tokio::sync::mpsc::channel(1).0,
                root: std::path::PathBuf::from("/nonexistent"),
            }),
            update: Arc::new(tokio::sync::RwLock::new(
                crate::update::state::UpdateState::initial(env!("CARGO_PKG_VERSION"), &[]),
            )),
            update_tx: tokio::sync::mpsc::channel(1).0,
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
        forget_page(&state.admin_backends, &state.admin_assets, &state.registry, "autre").await;
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "still cached");

        // 2. Once the plugin is forgotten, the route says frankly that there is
        //    nothing there — that is the half of the fix that removes the dead
        //    page from the menu instead of returning an IPC error.
        forget_page(&state.admin_backends, &state.admin_assets, &state.registry, "radio").await;
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
    async fn the_plugin_catalog_route_never_calls_the_plugin_over_ipc() {
        // The observable proof that `GET /plugins/<name>/api/i18n` no longer
        // performs an IPC round trip: `Fake` panics on *any* admin call, and
        // the plugin still needs to be "known" (present in `admin_backends`)
        // for the route to serve it, so the 200 below can only come from the
        // registry. A test that only checked the response body would also
        // pass with the old IPC path in place — see task 5's brief.
        let app = router(state_with(Fake { panics: true, ..Default::default() }));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?lang=en&v=abc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_plugin_catalog_is_served_flat_from_the_registry() {
        // No IPC any more: the plugin's announced catalogue is what the
        // registry already holds (`main.rs`'s `hotplug` inserts it via
        // `insert_announced`), and `admin_i18n` resolves it the same way the
        // core's own `/api/i18n` resolves its.
        let state = state_with(Fake::default());
        let mut layers = ritornello_i18n::ModuleLayers::new("radio");
        layers.insert(
            "en",
            ritornello_i18n::Layer::from_map([("btn_save".to_string(), "Save".to_string())].into()),
        );
        state.registry.write().await.insert_announced("radio", layers);
        let app = router(state);
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?lang=en").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["btn_save"], "Save");
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
        // Ruling (f): `?v=x` alone still leaves "the core's current
        // interface language" unresolved by the URL — `immutable` is a
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
    async fn an_unspecified_language_follows_the_core_s_current_interface_language() {
        // `lang` absent used to mean "whatever the plugin is currently
        // running in", asked over IPC. It now means the core's own
        // `locale_current` — the same field `status_json`'s `locale` reads —
        // resolved from the registry like every other case.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "greeting = \"Bonjour\"\n").unwrap();
        let state = state_with_locales_root(Fake::default(), dir.path().to_path_buf());
        *state.locale_current.write().await = Some("fr".to_string());
        let app = router(state);
        let resp = app.oneshot(Request::get("/plugins/radio/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["greeting"], "Bonjour");
    }

    /// Task 13 fix round, F-1: `admin_i18n` used to pass a hardcoded `"en"`
    /// as `chain_for`'s fallback tier, so a plugin's whole admin catalog
    /// ignored the device's own fallback setting even though the core's own
    /// `/api/i18n` already honoured it. The key here is defined **only** in
    /// the fallback language ("fr"), never in the chosen one ("de") nor in
    /// English, so a resolution through anything but the real fallback tier
    /// misses it.
    ///
    /// **[MUTATION]**: revert `admin_i18n` to pass a hardcoded `"en"` — this
    /// test fails, the key never appearing in the served catalog at all.
    #[tokio::test]
    async fn admin_i18n_resolves_through_the_devices_fallback_not_a_hardcoded_en() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "greeting = \"Bonjour\"\n").unwrap();
        let state = state_with_locales_root(Fake::default(), dir.path().to_path_buf());
        *state.locale_current.write().await = Some("de".to_string());
        *state.fallback_current.write().await = Some("fr".to_string());
        let app = router(state);
        let resp = app.oneshot(Request::get("/plugins/radio/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["greeting"], "Bonjour", "neither the chosen language nor a hardcoded en carries this key");
    }

    #[tokio::test]
    async fn two_languages_serve_two_different_disk_packs() {
        // What `immutable` must never lie about: serving French under the
        // English URL. The registry (task 4), not a cache keyed by
        // `(plugin, lang)`, is what keeps the two apart now — each language
        // is its own on-disk pack under `<locales_root>/radio/<lang>.toml`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/en.toml"), "marker = \"en-value\"\n").unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "marker = \"fr-value\"\n").unwrap();
        let app = router(state_with_locales_root(Fake::default(), dir.path().to_path_buf()));
        for (lang, expected) in [("fr", "fr-value"), ("en", "en-value")] {
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
            assert_eq!(v["marker"], expected, "served the wrong language's entry for ?lang={lang}");
        }
    }

    #[tokio::test]
    async fn the_plugin_catalog_route_is_never_marked_immutable_unlike_the_asset_route() {
        // The premise `immutable` used to rest on (see `AppState::session`'s
        // doc, before this task): nothing under a stamped plugin URL could
        // change within one session, because nothing read the registry's
        // disk tier for a plugin. Task 5 is precisely what broke that —
        // `admin_i18n` reads `Registry::chain_for` directly, and the
        // registry's disk tier can change mid-session without the core's
        // `session` stamp ever moving. This test writes out the case an
        // operator triggers directly: an on-disk pack edited, then a real
        // locale change (`Registry::resweep_async`, what `Core::set_locale`
        // calls). `AppState::session`'s doc names a second case closed the
        // same way, for the same reason, without a second test: a plugin
        // that self-updates and re-announces mid-run
        // (`hotplug`/`insert_announced`) used to leave a browser's
        // `immutable`-cached catalogue stale until the *core* restarted,
        // even though the registry itself had already moved on — both cases
        // are exactly the same fact (the URL no longer determines the
        // content) reached by two different writers of the same registry.
        // The fix (this fix round) is not a longer key: the catalog route
        // never claims `immutable` any more, at all — this test writes the
        // on-disk-edit sequence end to end (same URL, an edit, a resweep,
        // two different answers) and asserts the *header*, which is the
        // discriminating check: it must fail the moment `immutable` is
        // restored on this route, even with `lang` and a stamp both present
        // — the exact shape that used to trigger it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/en.toml"), "greeting = \"before\"\n").unwrap();
        let state = state_with_locales_root(Fake::default(), dir.path().to_path_buf());
        let app = router(state.clone());
        let url = "/plugins/radio/api/i18n?lang=en&v=abc";

        let first = app.clone().oneshot(Request::get(url).body(Body::empty()).unwrap()).await.unwrap();
        let cc1 = first.headers().get(axum::http::header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(!cc1.contains("immutable"), "{cc1}");
        let v1: serde_json::Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v1["greeting"], "before");

        // The operator edits the pack on disk, then the interface language
        // is switched — the only gesture, today, that calls
        // `Registry::resweep_async` (`Core::set_locale`). Called directly
        // here: this test is at the HTTP layer, with no `Core` to drive. Now
        // that the route never promises `immutable`, this is no longer a
        // broken promise — a revalidating caller is expected to see it.
        std::fs::write(dir.path().join("radio/en.toml"), "greeting = \"after\"\n").unwrap();
        crate::i18n::Registry::resweep_async(&state.registry).await;

        let second = app.oneshot(Request::get(url).body(Body::empty()).unwrap()).await.unwrap();
        let cc2 = second.headers().get(axum::http::header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(!cc2.contains("immutable"), "{cc2}");
        let v2: serde_json::Value =
            serde_json::from_slice(&second.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v2["greeting"], "after");

        // Contrast, in the same test: `admin_asset` keeps `immutable`, since
        // its own justification (one IPC fetch per plugin process, held for
        // that process's whole lifetime) is untouched by this task.
        let asset_resp = router(state_with(Fake::default()))
            .oneshot(Request::get("/plugins/radio/ui.js?v=cafe").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let asset_cc = asset_resp.headers()[axum::http::header::CACHE_CONTROL].to_str().unwrap();
        assert!(asset_cc.contains("immutable"), "{asset_cc}");
    }

    #[tokio::test]
    async fn a_malformed_language_is_refused_and_never_marked_immutable() {
        // Charset/length violation (`valid_locale`'s own rule, reused rather
        // than a bespoke grammar) — refused outright, before the registry is
        // ever consulted.
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?lang=..&v=abc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let cc = resp.headers().get(axum::http::header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(!cc.contains("immutable"), "{cc}");
    }

    #[tokio::test]
    async fn a_locale_outside_the_installed_set_is_still_served_from_the_registry() {
        // Deliberate behavior change (see `admin_i18n`'s doc): membership in
        // the installed set existed only to bound `CatalogCache`, which is gone.
        // A well-shaped but uninstalled language is no longer refused — it
        // is resolved like any other, and `chain_for` falls through to `en`
        // (then the key itself) for a module or language it knows nothing
        // about, exactly as it already does for the core's own catalog.
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n?lang=fr&v=abc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn forget_page_purges_the_announced_layer_the_route_reads_too() {
        // Same invariant `main.rs`'s own registry tests pin at the `Registry`
        // level, checked here through the actual HTTP route: a plugin that
        // disconnects must not go on serving its old announced translations.
        let state = state_with(Fake::default());
        let mut layers = ritornello_i18n::ModuleLayers::new("radio");
        layers.insert("en", ritornello_i18n::Layer::from_map([("greeting".to_string(), "Hi".to_string())].into()));
        state.registry.write().await.insert_announced("radio", layers);
        let app = router(state.clone());

        let get = |app: axum::Router| async move {
            app.oneshot(Request::get("/plugins/radio/api/i18n?lang=en").body(Body::empty()).unwrap()).await.unwrap()
        };

        let before = get(app.clone()).await;
        let v: serde_json::Value = serde_json::from_slice(&before.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["greeting"], "Hi");

        forget_page(&state.admin_backends, &state.admin_assets, &state.registry, "radio").await;
        state.admin_backends.write().await.insert("radio".into(), Arc::new(Fake::default()));
        let after = get(app.clone()).await;
        let v: serde_json::Value = serde_json::from_slice(&after.into_body().collect().await.unwrap().to_bytes()).unwrap();
        // `entries()` (unlike `Chain::get`) never invents a key: a module
        // with nothing left in the registry serves an empty map.
        assert!(v.get("greeting").is_none(), "the forgotten announcement must no longer be served: {v}");
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

    /// Companion to `invalid_put_data_returns_422_with_a_message`, proving
    /// the other half of `AdminResult::Set.error_text`: a **keyed** refusal
    /// (what every migrated plugin now sends, `error_text` being all it has
    /// left to hand back — see `AdminPlugin::set_data`'s own doc) resolves
    /// through the registry, exactly as `admin_i18n` resolves a whole
    /// catalog, and the browser still reads a finished sentence out of
    /// `{"error": ...}` without knowing any of this changed.
    ///
    /// **[MUTATION]**: change `resolve_admin_text` to return the bare `key`
    /// instead of resolving it through `registry.chain_for(...)` — this
    /// test fails, asserting `"bad_request"` instead of the resolved
    /// sentence. Also fires if the `{detail}` substitution is dropped.
    #[tokio::test]
    async fn a_keyed_refusal_resolves_through_the_registry() {
        let state = state_with(Fake { reject: true, ..Default::default() });
        let mut layers = ritornello_i18n::ModuleLayers::new("radio");
        layers.insert(
            "en",
            ritornello_i18n::Layer::from_map(
                [("bad_request".to_string(), "Bad request: {detail}".to_string())].into(),
            ),
        );
        state.registry.write().await.insert_announced("radio", layers);
        // Overrides the plain-`Fake` reject path above with a keyed one:
        // `state_with` already wired a `Fake { reject: true, .. }` under
        // "radio", but that one answers `Text::Verbatim("duplicate
        // preset")` — this test needs the **keyed** shape instead, so it
        // re-registers its own backend under the same name.
        struct Keyed;
        #[async_trait::async_trait]
        impl AdminBackend for Keyed {
            async fn asset(&self, _path: &str) -> Result<Option<(String, String)>> {
                Ok(None)
            }
            async fn get_data(&self) -> Result<serde_json::Value> {
                Ok(serde_json::json!({}))
            }
            async fn set_data(&self, _data: serde_json::Value) -> Result<Result<(), Text>> {
                let mut params = std::collections::HashMap::new();
                params.insert("detail".to_string(), "missing field `stations`".to_string());
                Ok(Err(Text::Keyed { key: "bad_request".into(), params }))
            }
            async fn ping(&self) -> Result<()> {
                Ok(())
            }
        }
        state.admin_backends.write().await.insert("radio".into(), Arc::new(Keyed));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "Bad request: missing field `stations`");
    }

    /// Task 13 fix round, F-1: `resolve_admin_text` used to pass a hardcoded
    /// `"en"` as `chain_for`'s fallback tier, so a plugin's save error never
    /// honoured the device's own fallback setting even though the core's own
    /// status line and every Source status text already did. The key here is
    /// defined **only** in the fallback language ("fr"), never in the chosen
    /// one ("de") nor in English — neither a disk pack (there is none) nor a
    /// hardcoded one — so a resolution through anything but the real
    /// fallback tier falls through to the raw key instead.
    ///
    /// **[MUTATION]**: revert `resolve_admin_text` to pass a hardcoded `"en"`
    /// — this test fails, asserting the raw key `"bad_request"` instead of
    /// the resolved French sentence.
    #[tokio::test]
    async fn a_keyed_refusal_resolves_through_the_devices_fallback_not_a_hardcoded_en() {
        let state = state_with(Fake { reject: true, ..Default::default() });
        *state.locale_current.write().await = Some("de".to_string());
        *state.fallback_current.write().await = Some("fr".to_string());
        let mut layers = ritornello_i18n::ModuleLayers::new("radio");
        layers.insert(
            "fr",
            ritornello_i18n::Layer::from_map(
                [("bad_request".to_string(), "Mauvaise requête : {detail}".to_string())].into(),
            ),
        );
        state.registry.write().await.insert_announced("radio", layers);
        struct Keyed;
        #[async_trait::async_trait]
        impl AdminBackend for Keyed {
            async fn asset(&self, _path: &str) -> Result<Option<(String, String)>> {
                Ok(None)
            }
            async fn get_data(&self) -> Result<serde_json::Value> {
                Ok(serde_json::json!({}))
            }
            async fn set_data(&self, _data: serde_json::Value) -> Result<Result<(), Text>> {
                let mut params = std::collections::HashMap::new();
                params.insert("detail".to_string(), "missing field `stations`".to_string());
                Ok(Err(Text::Keyed { key: "bad_request".into(), params }))
            }
            async fn ping(&self) -> Result<()> {
                Ok(())
            }
        }
        state.admin_backends.write().await.insert("radio".into(), Arc::new(Keyed));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "Mauvaise requête : missing field `stations`");
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
        // `Chain::get` is silent, and a bare key would be displayed as is.
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
