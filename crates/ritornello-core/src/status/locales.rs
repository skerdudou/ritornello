//! Languages: the packs available on disk, the current selection (GET/PUT /api/locale) and the flattened catalog served to the SPA.

use super::*;

/// Names of the available languages, from the file names of a `core/`
/// directory: `en` (always) + each `<lang>.toml`. Pure function, testable,
/// separated from disk access (like `audio_output::parse_device_list`).
pub fn parse_available_locales(filenames: &[String]) -> Vec<String> {
    let mut out = vec!["en".to_string()];
    for f in filenames {
        if let Some(stem) = f.strip_suffix(".toml")
            && stem != "en" && !out.iter().any(|x| x == stem)
        {
            out.push(stem.to_string());
        }
    }
    out
}

/// Core languages = `en` + the `<root>/core/*.toml` packs present.
pub fn list_locales(root: &std::path::Path) -> Vec<String> {
    let names: Vec<String> = std::fs::read_dir(root.join("core"))
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).collect()
        })
        .unwrap_or_default();
    parse_available_locales(&names)
}

#[derive(Serialize)]
pub(super) struct LocaleResponse {
    locales: Vec<String>,
    current: Option<String>,
}

pub(super) async fn locale_json(State(state): State<AppState>) -> Json<LocaleResponse> {
    let locales = list_locales(&state.locales_root);
    let current = state.locale_current.read().await.clone();
    Json(LocaleResponse { locales, current })
}

#[derive(Deserialize)]
pub(super) struct LocaleRequest {
    locale: String,
}

/// Shape of an acceptable language code: what the `<lang>.toml` file names of
/// the packs produce (`fr`, `en`, `pt-BR`…).
///
/// The value ends up in file paths (`<root>/<component>/<lang>.toml` via
/// `Catalog::load`), in `state.json` and in an environment variable of the
/// plugins: same rigor as for the theme and the audio output, which are
/// validated — an arbitrary string opened a path traversal
/// (`{"locale":"../../whatever"}`) on an unauthenticated API.
pub(super) fn valid_locale(locale: &str) -> bool {
    !locale.is_empty()
        && locale.len() <= 16
        && locale.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub(super) async fn locale_put(State(state): State<AppState>, Json(req): Json<LocaleRequest>) -> StatusCode {
    if !valid_locale(&req.locale) {
        return StatusCode::BAD_REQUEST;
    }
    *state.locale_current.write().await = Some(req.locale.clone());
    if state.locale_tx.send(req.locale).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::NO_CONTENT
}

/// The core catalog in the current language, flattened, for the SPA's `t()`.
pub(super) async fn i18n_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cat = state.catalog.read().await;
    Json(serde_json::json!(cat.entries()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::tests_support::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    #[test]
    fn parse_available_locales_prefixes_en_and_deduplicates() {
        let names = vec!["fr.toml".to_string(), "en.toml".to_string(), "README.md".to_string()];
        assert_eq!(parse_available_locales(&names), vec!["en".to_string(), "fr".to_string()]);
    }

    #[tokio::test]
    async fn get_locale_lists_en_and_the_core_packs() {
        let (state, _rx, _dir) = app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], "fr");
        let locales: Vec<String> = serde_json::from_value(v["locales"].clone()).unwrap();
        assert!(locales.contains(&"en".to_string()));
        assert!(locales.contains(&"fr".to_string()));
    }

    #[tokio::test]
    async fn put_locale_notifies_and_updates_the_selection() {
        let (state, mut locale_rx, _dir) = app_state_fr();
        let locale_current = state.locale_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"fr"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(locale_rx.recv().await.unwrap(), "fr");
        assert_eq!(locale_current.read().await.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn put_locale_refuses_a_value_that_is_not_a_language_code() {
        // Regression (review 2026-07-27): the value ends up in file paths, in
        // state.json and in an environment variable of the plugins; `../../x`
        // must be refused **before** any update, as the theme and the audio
        // output already do for their fields.
        let (state, mut locale_rx, _dir) = app_state_fr();
        let locale_current = state.locale_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"../../var/lib/whatever"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // Neither notified nor kept as the current selection.
        assert!(locale_rx.try_recv().is_err());
        assert_eq!(locale_current.read().await.as_deref(), Some("fr"));
    }

    #[test]
    fn valid_locale_accepts_codes_and_refuses_the_rest() {
        for ok in ["en", "fr", "pt-BR", "zh_Hant", "fr-CA"] {
            assert!(valid_locale(ok), "{ok} should pass");
        }
        for ko in ["", "..", "../fr", "fr/..", "fr toml", "a".repeat(17).as_str()] {
            assert!(!valid_locale(ko), "{ko:?} should be refused");
        }
    }

    #[tokio::test]
    async fn api_i18n_returns_the_flattened_catalog() {
        let app = router(tests_support::app_state());
        let resp = app.oneshot(Request::get("/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // The core's embedded English carries these keys (src/locales/en.toml).
        assert!(v["remote_title"].is_string());
        assert!(v["audio_output"].is_string());
    }

    #[tokio::test]
    async fn api_i18n_follows_the_current_language() {
        let (state, _rx, _dir) = tests_support::app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["audio_output"], "Sortie audio");
    }
}
