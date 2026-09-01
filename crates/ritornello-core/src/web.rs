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
        "public, max-age=31536000, immutable"
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
            (header::CACHE_CONTROL, cache_control(path)),
            (header::ETAG, etag.as_str()),
        ],
        f.data.to_vec(),
    )
        .into_response()
}

/// Router fallback: serves the shell for SPA paths, 404 otherwise.
pub async fn shell(State(state): State<AppState>, uri: Uri) -> Response {
    if !serves_shell(uri.path()) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let t = state.theme_current.read().await.clone();
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        inject_theme(&shell_html(), &t.theme, &t.mode),
    )
        .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/assets/*path", get(asset))
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
}
