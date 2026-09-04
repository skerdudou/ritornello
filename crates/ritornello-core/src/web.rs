use crate::status::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;

/// The `dist/` produced by `npm run build --workspaces`, embedded at compile
/// time. `build.rs` guarantees it exists (placeholder otherwise).
#[derive(RustEmbed)]
#[folder = "../../web/app/dist/"]
struct Dist;

/// Header of an asset a caller has stamped with a fingerprint of its own
/// (`?v=…`): a year, and never to be revalidated, since the URL itself
/// determines the content and a fresh fingerprint names a fresh URL.
///
/// One `const` used from both routers — the SPA's own assets here, and a
/// plugin's assets/catalog in `admin.rs` — rather than four copies of the
/// same string: a divergence between them would mean a wrong header served
/// for a year to whichever route lagged behind. Not a shared *function*: the
/// two routers hold no state in common, and the two "is this URL stamped?"
/// checks are legitimately different (a bare `v=`, versus `lang` **and**
/// `v` both present — see `admin::admin_i18n`).
pub const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

pub fn mime_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// The app's chunks carry a hash in their name: they are immutable. `vue.js`
/// and `ui-kit.js` on the contrary keep a **stable name** — that is the
/// contract the plugin modules import — so they must be revalidated (the
/// `ETag` takes care of it).
pub fn cache_control(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.starts_with("app-") {
        IMMUTABLE_CACHE_CONTROL
    } else {
        "no-cache"
    }
}

/// The fallback serves the shell **only** outside the data namespaces. Without
/// this restriction, a typo on an API route would answer 200 with HTML — a
/// costly debugging trap.
pub fn serves_shell(path: &str) -> bool {
    if path.starts_with("/api/") || path.starts_with("/assets/") {
        return false;
    }
    if let Some(rest) = path.strip_prefix("/plugins/")
        && let Some((_, after)) = rest.split_once('/')
    {
        if after.starts_with("api/") || after.starts_with("ui.") {
            return false;
        }
        // Deep asset path (`/plugins/radio/assets/chunk.js`): a plugin's
        // assets are only served on **a single** segment
        // (`/plugins/<name>/<file>`), so a multi-segment path matches no
        // route and fell through here — the fallback then returned **the
        // HTML shell with a 200**, so that a dynamic `import()` received
        // HTML: a very confusing failure mode, nothing flagging the error.
        // It now gets a clean 404.
        //
        // The `serves_shell("/plugins/<name>/")` test stays green: `after`
        // is then the empty string, which contains no `/`. That is also why
        // this condition is preferred to a `/plugins/:name/*file` wildcard
        // in the router, whose empty remainder does not match reliably —
        // and this URL is an invariant.
        if after.contains('/') {
            return false;
        }
    }
    // A last segment carrying an extension names a **file**, never an app
    // route: every route of the Vue router is a bare word (`/config`,
    // `/system`, `/plugins/<name>/`).
    //
    // The case that made this necessary is `/favicon.ico`. No icon was
    // declared, so every browser asked for that path on its own initiative --
    // and got the entire HTML shell with a 200. It cannot be decoded as an
    // image, there is nothing worth caching in a reply like that, so the
    // browser asked again on the next load, for ever. Reported in use as an
    // icon request that never settles, on a project that has no icon.
    //
    // The same reasoning already applied to a plugin's deep asset paths just
    // above; this extends it to the shell's own namespace. `/robots.txt` and
    // `/apple-touch-icon.png` now get an honest 404 too, instead of a page.
    let last = path.rsplit('/').next().unwrap_or(path);
    if last.contains('.') {
        return false;
    }
    true
}

pub fn inject_theme(html: &str, theme: &str, mode: &str) -> String {
    // The `Display` of `serde_json::Value` does not escape `/`: a value (e.g.
    // `theme`) containing `</script>` would close the tag prematurely and allow
    // injecting arbitrary HTML into the shell.
    // `theme::validate` already forbids these characters on the HTTP path, but
    // `main.rs` re-reads `theme`/`mode` from `state.json` without revalidating
    // — a corrupted or hand-edited state file therefore remains a vector.
    // So we escape every "less-than" sign of the serialized value with its JSON
    // equivalent in `\u`+code-point notation (see `replace` below): it remains
    // strictly equivalent JSON — the browser decodes it back identically — but
    // the `</script>` substring can no longer appear literally in the produced
    // document.
    let payload =
        serde_json::json!({ "theme": theme, "mode": mode }).to_string().replace('<', "\\u003c");
    let script = format!("<script>window.__RITORNELLO_THEME__={payload};</script>");
    match html.find("</head>") {
        Some(i) => format!("{}{}{}", &html[..i], script, &html[i..]),
        None => format!("{script}{html}"),
    }
}

/// The embedded shell, or the placeholder if `build.rs` found no deliverable.
pub fn shell_html() -> String {
    match Dist::get("index.html") {
        Some(f) => String::from_utf8_lossy(&f.data).into_owned(),
        None => crate::placeholder::placeholder_html("npm ci && npm run build --workspaces"),
    }
}

fn etag_of(hash: &[u8]) -> String {
    let hex: String = hash.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

async fn asset(headers: HeaderMap, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches("/assets/");
    let Some(f) = Dist::get(&format!("assets/{path}")) else {
        return (StatusCode::NOT_FOUND, "unknown asset").into_response();
    };
    let etag = etag_of(&f.metadata.sha256_hash());
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime_for(path)),
            (header::CACHE_CONTROL, cache_control_for(path, uri.query())),
            (header::ETAG, etag.as_str()),
        ],
        f.data.to_vec(),
    )
        .into_response()
}

/// Cache policy for an asset request, query included.
///
/// A version in the query means the URL identifies this exact content, so it
/// never needs revalidating. The value is not compared against anything: a
/// client asking under an old stamp gets the current bytes, and its next
/// `index.html` — served `no-cache` — sends it to the new URL anyway.
pub fn cache_control_for(path: &str, query: Option<&str>) -> &'static str {
    if query.is_some_and(|q| q.split('&').any(|p| p.starts_with("v="))) {
        return IMMUTABLE_CACHE_CONTROL;
    }
    cache_control(path)
}

/// Fingerprint of an embedded asset, short enough to keep the URL readable.
///
/// Derived from the bytes actually served, never declared by hand: an
/// `immutable` response under a fingerprint that does not follow its content
/// would freeze a client on a stale bundle until they clear their cache.
fn asset_version(name: &str) -> Option<String> {
    let f = Dist::get(&format!("assets/{name}"))?;
    Some(f.metadata.sha256_hash().iter().take(8).map(|b| format!("{b:02x}")).collect())
}

/// The two stable names of the plugin UI contract, resolved through the import
/// map. They cannot carry a hash in their filename — that name **is** the
/// contract — so the fingerprint travels in the query instead.
const STABLE_ASSETS: [&str; 2] = ["vue.js", "ui-kit.js"];

/// Rewrites the import map's URLs with the fingerprint of what will be served.
///
/// Done at serve time and not at build time, for a measured reason: `vue.js`
/// is produced by a **second** vite pass that runs *after* the import map has
/// been injected into `index.html`, so its bytes do not exist yet when the
/// build would need to hash them. The core, on the other hand, holds exactly
/// the bytes it is about to serve.
///
/// **The stamp must appear here and nowhere else.** A module is identified by
/// its resolved URL: were `/assets/vue.js` and `/assets/vue.js?v=…` both
/// reachable, the browser would evaluate Vue twice and the shell and the
/// plugins would stop sharing a reactivity graph.
pub fn stamp_import_map(html: &str, version: impl Fn(&str) -> Option<String>) -> String {
    let mut out = html.to_string();
    for name in STABLE_ASSETS {
        // No fingerprint (placeholder build): leave the plain URL. It still
        // resolves; a URL stamped with nothing would not.
        let Some(v) = version(name) else { continue };
        out = out.replace(&format!("\"/assets/{name}\""), &format!("\"/assets/{name}?v={v}\""));
    }
    out
}

/// Router fallback: serves the shell for SPA paths, 404 otherwise.
pub async fn shell(State(state): State<AppState>, uri: Uri) -> Response {
    if !serves_shell(uri.path()) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let t = state.theme_current.read().await.clone();
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        inject_theme(&stamp_import_map(&shell_html(), asset_version), &t.theme, &t.mode),
    )
        .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/assets/{*path}", get(asset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    #[test]
    fn inject_theme_puts_the_choice_before_the_head_closes() {
        let html = "<!doctype html><html><head><title>x</title></head><body></body></html>";
        let out = inject_theme(html, "cyberpunk", "dark");
        assert!(out.contains(r#"window.__RITORNELLO_THEME__={"mode":"dark","theme":"cyberpunk"}"#));
        assert!(out.find("__RITORNELLO_THEME__").unwrap() < out.find("</head>").unwrap());
    }

    #[test]
    fn inject_theme_survives_an_html_without_head() {
        let out = inject_theme("<div>x</div>", "vercel", "light");
        assert!(out.contains("__RITORNELLO_THEME__"));
    }

    #[test]
    fn inject_theme_escapes_script_closings_in_values() {
        // `theme::validate` prevents this case on the normal HTTP path, but
        // `main.rs` re-reads `theme`/`mode` from `state.json` without
        // revalidating: a corrupted or hand-edited state file remains a vector.
        // `inject_theme` must stay safe even when receiving a hostile value.
        let hostile_name = "</script><script>alert(1)</script>";
        let out = inject_theme("<div>x</div>", hostile_name, "dark");
        // The only `<script>` closing tag in the document must be the one that
        // actually closes the injected script: no premature closing coming from
        // the value may appear literally.
        assert_eq!(out.matches("</script>").count(), 1);
        // The JSON remains parseable despite the escaping, and the browser
        // would recover the original value in it, character for character.
        let start = out.find('{').unwrap();
        let end = out.find(";</script>").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out[start..end]).unwrap();
        assert_eq!(v["theme"], hostile_name);
    }

    #[test]
    fn the_fallback_serves_the_shell_only_outside_the_data_namespaces() {
        // The historical URLs and the Vue router's routes: shell.
        assert!(serves_shell("/"));
        assert!(serves_shell("/status"));
        assert!(serves_shell("/config"));
        assert!(serves_shell("/plugins/radio/"));
        assert!(serves_shell("/plugins/"));
        // The data namespaces: never the shell, otherwise a typo on an API
        // route would answer 200 with HTML — a debugging trap.
        assert!(!serves_shell("/api/statuss"));
        assert!(!serves_shell("/api/theme"));
        assert!(!serves_shell("/assets/inconnu.js"));
        assert!(!serves_shell("/plugins/radio/api/data"));
        assert!(!serves_shell("/plugins/radio/ui.js"));
        assert!(!serves_shell("/plugins/radio/ui.css"));
        // A path that names a file is never an app route. `/favicon.ico` is
        // the one every browser asks for on its own, and it used to be
        // answered with the whole HTML shell and a 200: the browser cannot
        // decode that as an image, has nothing worth keeping, and asks again
        // on the next load — for ever. Same failure mode as the deep asset
        // path below, reported in use as an icon request that never settles.
        assert!(!serves_shell("/favicon.ico"));
        assert!(!serves_shell("/apple-touch-icon.png"));
        assert!(!serves_shell("/robots.txt"));
        // The rule keys on the **last** segment: a dot earlier in the path
        // says nothing about what the last one names.
        assert!(serves_shell("/plugins/a.b/"));
        // Deep asset path: a plugin's assets are only served on a single
        // segment, so `/plugins/radio/assets/chunk.js` matches no route. It
        // fell onto the fallback, which answered 200 with the HTML shell — a
        // dynamic `import()` received HTML, a very confusing failure mode. Now
        // a clean 404.
        assert!(!serves_shell("/plugins/radio/assets/chunk.js"));
        assert!(!serves_shell("/plugins/radio/a/b/c.js"));
        // ... without touching the historical URL, which must keep serving the
        // shell: the remainder after the plugin name is empty, hence without `/`.
        assert!(serves_shell("/plugins/radio/"));
        assert!(serves_shell("/plugins/generic-input/"));
        // A flat asset remains possible: it is the contract of plugin UIs.
        assert!(serves_shell("/plugins/radio/quelconque"));
    }

    #[tokio::test]
    async fn a_deep_plugin_asset_path_answers_404_and_not_the_shell() {
        // End to end through the router: it is the real HTTP response that
        // counts, a dynamic `import()` must never receive HTML.
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app
            .oneshot(Request::get("/plugins/radio/assets/chunk.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn mime_and_cache_according_to_the_file_name() {
        assert_eq!(mime_for("app-abc.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("app-abc.mjs"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("app-abc.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("logo.svg"), "image/svg+xml");
        assert_eq!(mime_for("police.woff2"), "font/woff2");
        assert_eq!(mime_for("favicon.png"), "image/png");
        assert_eq!(mime_for("inconnu.bin"), "application/octet-stream");
        // Hashed name: immutable. Stable names of the contract: to revalidate.
        assert!(cache_control("app-abc123.js").contains("immutable"));
        assert!(!cache_control("vue.js").contains("immutable"));
        assert!(!cache_control("ui-kit.js").contains("immutable"));
    }

    #[tokio::test]
    async fn the_root_serves_the_shell_with_the_theme_injected() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(
            resp.into_body().collect().await.unwrap().to_bytes().to_vec(),
        )
        .unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
        assert!(html.contains("northern-lights"));
    }

    #[tokio::test]
    async fn an_unknown_path_outside_the_api_serves_the_shell() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp =
            app.oneshot(Request::get("/plugins/quelconque/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unknown_api_route_answers_404_and_not_the_shell() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app.oneshot(Request::get("/api/statuss").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_embedded_content_is_servable() {
        // Real `dist/` or placeholder: one of the two is necessarily present,
        // `build.rs` guarantees it.
        let html = shell_html();
        assert!(!html.is_empty());
        assert!(html.contains("<!doctype html>") || html.contains("<!DOCTYPE html>"));
    }

    #[tokio::test]
    async fn a_missing_asset_answers_404() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app
            .oneshot(Request::get("/assets/nexiste-pas.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_import_map_gets_the_fingerprint_of_each_stable_url() {
        // The two stable names are the plugin UI contract: they cannot carry a
        // hash the way `app-*.js` does, so the fingerprint goes in the query.
        let html = r#"<script type="importmap">
      {"imports":{"vue":"/assets/vue.js","@ritornello/ui":"/assets/ui-kit.js"}}
    </script>"#;
        let out = stamp_import_map(html, |name| match name {
            "vue.js" => Some("aaaa".into()),
            "ui-kit.js" => Some("bbbb".into()),
            _ => None,
        });
        assert!(out.contains(r#""vue":"/assets/vue.js?v=aaaa""#), "{out}");
        assert!(out.contains(r#""@ritornello/ui":"/assets/ui-kit.js?v=bbbb""#), "{out}");
    }

    #[test]
    fn an_absent_asset_leaves_its_url_untouched() {
        // A placeholder build (no `dist/`) must still serve a usable import map:
        // a URL stamped with nothing would 404 where the plain one merely misses.
        let html = r#"{"imports":{"vue":"/assets/vue.js"}}"#;
        assert_eq!(stamp_import_map(html, |_| None), html);
    }

    #[test]
    fn a_version_in_the_query_makes_any_asset_immutable() {
        // The stable names are the plugin UI contract and cannot carry a hash in
        // the filename; the fingerprint travels in the query instead, and that is
        // what lets them be cached without revalidation.
        assert!(cache_control_for("vue.js", Some("v=abcd")).contains("immutable"));
        assert!(cache_control_for("ui-kit.js", Some("v=abcd")).contains("immutable"));
    }

    #[test]
    fn the_same_asset_without_a_version_is_still_revalidated() {
        // The switch must not silently freeze a client on a stale bundle: an
        // unstamped URL keeps the old behaviour.
        assert_eq!(cache_control_for("vue.js", None), "no-cache");
        assert_eq!(cache_control_for("vue.js", Some("theme=dark")), "no-cache");
    }

    #[test]
    fn a_hashed_chunk_stays_immutable_without_a_version() {
        // `app-*.js` already carries its hash in its name — the query changes
        // nothing for it, and that existing policy must survive this change.
        assert!(cache_control_for("app-abc123.js", None).contains("immutable"));
    }
}
