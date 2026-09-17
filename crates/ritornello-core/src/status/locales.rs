//! Languages: the packs available on disk, the current selection (GET/PUT /api/locale) and the flattened catalog served to the SPA.

use super::*;

/// One candidate language's completeness, flattened from
/// `ritornello_i18n::Coverage` for the wire: `complete` is the boolean the
/// owner's display rule pivots on ("nothing shown for a complete language,
/// just its name" — the SPA never has to compute `done == total` itself),
/// `done`/`total` are the raw numbers task 14's phrase key needs
/// (`{done}`/`{total}`, never a concatenated string — see the chantier's
/// own rule against that).
///
/// `complete_modules` (fix round 1, task 14 review): the **names** of the
/// modules `Complete` for this language — `Coverage::modules()` filtered to
/// `ModuleCoverage::Complete`. Added because the SPA's fallback line used to
/// approximate the combined chosen+fallback coverage with `Math.max(chosen
/// .done, fallback.done)`, a bound that is exact only when one language's
/// covered set contains the other's, and reachable as far off as "3 still
/// in English" when the truth is 0 — the review's own worked example, with
/// `chosen` covering `{radio, mpd, musicbrainz}` and `fallback` covering
/// the *disjoint* `{core, files, generic-input, nrj-metas}`. The bound's
/// own justification ("a per-module breakdown would mean a second HTTP
/// round trip per keystroke") was false: `locale_json` already builds the
/// full `Coverage` — module list included — for every language, under one
/// registry read, in this very handler; publishing the names is a purely
/// additive serde field on the same response, no new route, no new IPC.
/// With this field the SPA computes the **true** `|A ∪ B|` (a set union of
/// names, not an inequality) and also gets what the brief's own annotation
/// names by module ("core + 3 plugins /7") — see `LanguageCard.vue`.
#[derive(Serialize)]
pub(super) struct LanguageCompleteness {
    language: String,
    complete: bool,
    done: usize,
    total: usize,
    complete_modules: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct LocaleResponse {
    /// The **union** of every language at least one module — the core or a
    /// connected plugin — translates (task 12), not only the core's own
    /// packs. A language a single third-party plugin ships is in here even
    /// if the core has never heard of it: the origin defect this chantier
    /// was opened to fix.
    locales: Vec<String>,
    current: Option<String>,
    /// Completeness for every language in `locales`, in the same order.
    completeness: Vec<LanguageCompleteness>,
    /// The device's persisted fallback language (task 13), or `"en"` on a
    /// device that has never set one — matching `Registry::chain_for`'s own
    /// doc: "typically fallback is itself en until a device has a real
    /// fallback setting", now that this is exactly that setting.
    fallback_current: String,
    /// Eligible fallback languages: the **core's own** installed set only
    /// (`Registry::core_languages`), per the owner's arbitration — a
    /// fallback is chosen among what is guaranteed to resolve everywhere,
    /// not among every plugin's own languages. Never empty:
    /// `core_languages` always includes `"en"`.
    fallback_candidates: Vec<String>,
}

/// Builds every field of `LocaleResponse` from **one** registry read guard,
/// deliberately: `locales`/`completeness` and `fallback_candidates` used to
/// be sourced from two different places — the registry's swept snapshot for
/// the first two, a live `std::fs::read_dir` of the pack root for the third
/// (`list_locales`, removed in the same change that added this comment —
/// see task 12's review, "F-1"; `AppState.locales_root`, its only remaining
/// reader, was removed with it). The live read was not actually more
/// current in any way that mattered: `Registry::chain_for` — what a chosen
/// fallback would *actually* resolve through — only ever sees post-sweep
/// state, so a device could be offered a fallback candidate the registry
/// could not yet resolve. `Registry::core_languages` answers from the same
/// snapshot `modules_with_text` already reads here, so the three fields can
/// never disagree about which languages exist.
pub(super) async fn locale_json(State(state): State<AppState>) -> Json<LocaleResponse> {
    let registry = state.registry.read().await;
    let modules = registry.modules_with_text();
    let locales = ritornello_i18n::union_of_languages(&modules);
    let completeness = locales
        .iter()
        .map(|lang| {
            let c = ritornello_i18n::coverage(&modules, lang);
            let complete_modules = c
                .modules()
                .iter()
                .filter(|(_, status)| *status == ritornello_i18n::ModuleCoverage::Complete)
                .map(|(name, _)| name.clone())
                .collect();
            LanguageCompleteness {
                language: lang.clone(),
                complete: c.is_complete(),
                done: c.complete_count(),
                total: c.total(),
                complete_modules,
            }
        })
        .collect();
    let fallback_candidates = registry.core_languages();
    drop(registry);
    // Clamped to `locales` (the **union**, not `core_languages`), falling
    // back to `None` — fix round 1, task 14 review, finding 3/R3. Before
    // this, `current` was the one site of this exact clamp left unclamped:
    // `status_json`'s own `locale` field already clamps `locale_current`
    // (against `core_languages`, `status/mod.rs`) and, since task 12,
    // `fallback_current` right below clamps too (against
    // `fallback_candidates`). A device whose selected language's pack was
    // removed served `current` as-is: the SPA then found no `completeness`
    // entry for it, so `LanguageCard` rendered it as a *complete* language —
    // the trigger showing the removed language's name, no annotation, no
    // fallback control — while every word on the actual page was English.
    //
    // **`locales` on purpose, not `core_languages`.** `current` can
    // legitimately name a language only a *plugin* ships (the origin defect
    // this chantier fixes — see `LocaleResponse::locales`'s own doc);
    // clamping against `core_languages` would silently re-narrow the
    // selector back to core-only packs for exactly the case task 12 added
    // the union to unlock. `locales` is the same list already computed
    // above from this same registry read, so this can never disagree with
    // what the selector itself offers.
    let current = state.locale_current.read().await.clone().filter(|l| locales.iter().any(|x| x == l));
    // Clamped to `fallback_candidates`, falling back to `en` — the same
    // discipline `status_json` already applies to `locale_current` against
    // `core_languages` (status/mod.rs), for the same reason: the stored
    // value can name a pack removed after being selected, or restored as-is
    // from a hand-edited `state.json` (permissive at load, by design — see
    // `PersistedState.fallback`'s doc). Serving it unclamped would let
    // `fallback_current` name a language absent from its own
    // `fallback_candidates` list — the exact shape that renders empty in a
    // reka-ui `Select` bound to it (task 14's SPA control).
    let fallback_current = state
        .fallback_current
        .read()
        .await
        .clone()
        .filter(|f| fallback_candidates.iter().any(|c| c == f))
        .unwrap_or_else(|| "en".to_string());
    Json(LocaleResponse {
        locales,
        current,
        completeness,
        fallback_current,
        fallback_candidates,
    })
}

#[derive(Deserialize)]
pub(super) struct LocaleRequest {
    locale: String,
    /// The fallback language (task 13), optional: present only when the
    /// settings page's own fallback control is visible (the chosen language
    /// is incomplete — the owner's rule, "nothing offered for what needs no
    /// second choice") and submits a value. A plain `{"locale": ...}` body —
    /// the shape every earlier client and every pre-task-13 test already
    /// sends — leaves the persisted fallback exactly as it was: the control
    /// disappearing while the chosen language happens to be complete must
    /// not clear a fallback stored for later.
    #[serde(default)]
    fallback: Option<String>,
}

/// Shape of an acceptable language code: what the `<lang>.toml` file names of
/// the packs produce (`fr`, `en`, `pt-BR`…).
///
/// The value ends up in file paths (`<root>/<component>/<lang>.toml`, swept
/// by `Registry` and resolved by `Registry::chain_for`) and in `state.json`:
/// same rigor as for the theme and the audio output, which are validated —
/// an arbitrary string opened a path traversal (`{"locale":"../../whatever"}`)
/// on an unauthenticated API.
///
/// `pub(crate)`, not `pub(super)`: `admin.rs` reuses this exact rule to
/// validate the `lang` query parameter of `/plugins/<name>/api/i18n`, rather
/// than inventing a second grammar that could drift from this one — see
/// `admin::admin_i18n`. `locale_put` (task 13) reuses it a second time, for
/// the optional `fallback` field of the very same request: one grammar for
/// every language code this route accepts, not two that could drift.
pub(crate) fn valid_locale(locale: &str) -> bool {
    !locale.is_empty()
        && locale.len() <= 16
        && locale.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Sets the chosen language and, optionally, the fallback — in one request,
/// since the settings page's fallback control lives right next to the
/// language selector and is submitted alongside it.
///
/// **Both are validated by shape before either is written.** A `fallback`
/// that fails `valid_locale` refuses the request with `BAD_REQUEST` before
/// `req.locale` is persisted either — the same "refused before any update"
/// rule the path-traversal regression test already pins for `locale` alone,
/// extended here so a malformed fallback can never smuggle in a locale
/// change as a side effect.
///
/// **No rejection for `fallback == locale`.** The owner's rule: a fallback
/// equal to the chosen language is accepted and stored, exactly as
/// submitted — refusing it would be indistinguishable, from the settings
/// page, from silently clearing a fallback the moment the chosen language
/// happens to catch up to it, which is precisely the loss the "memorized
/// value" rule exists to prevent.
pub(super) async fn locale_put(State(state): State<AppState>, Json(req): Json<LocaleRequest>) -> StatusCode {
    if !valid_locale(&req.locale) {
        return StatusCode::BAD_REQUEST;
    }
    if let Some(fallback) = &req.fallback
        && !valid_locale(fallback)
    {
        return StatusCode::BAD_REQUEST;
    }
    // Both sends before either write (F-5, task 13 fix round): with the
    // write first, a failed `fallback_tx.send` used to return 500 with
    // `locale_current` already mutated and `locale_tx` never touched — the
    // HTTP layer and the core would disagree about the chosen language
    // until the next successful PUT. Sending first makes the route atomic
    // for free: on any failure, neither `AppState` field is written, so a
    // partial send never has a partial write sitting next to it.
    if let Some(fallback) = &req.fallback
        && state.fallback_tx.send(fallback.clone()).await.is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    if state.locale_tx.send(req.locale.clone()).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *state.locale_current.write().await = Some(req.locale);
    if let Some(fallback) = req.fallback {
        *state.fallback_current.write().await = Some(fallback);
    }
    StatusCode::NO_CONTENT
}

/// The core catalog in the current language, flattened, for the SPA's `t()`.
///
/// No I/O: `state.catalog` is a snapshot already resolved from the shared
/// `Registry` (task 4) by `crate::i18n::core_catalog`, rebuilt only on a real
/// locale change (`Core::set_locale`) — see `AppState.catalog`'s own doc.
/// This route, like `admin::admin_i18n` (task 5), only ever reads memory
/// that was already built before the request arrived.
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

    #[tokio::test]
    async fn get_locale_lists_en_and_the_core_packs() {
        let (state, _rx, _frx, _dir) = app_state_fr();
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
        let (state, mut locale_rx, _frx, _dir) = app_state_fr();
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
        let (state, mut locale_rx, _frx, _dir) = app_state_fr();
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

    #[tokio::test]
    async fn put_locale_with_a_fallback_notifies_both_and_persists_both() {
        let (state, mut locale_rx, mut fallback_rx, _dir) = app_state_fr();
        let locale_current = state.locale_current.clone();
        let fallback_current = state.fallback_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"de","fallback":"nl"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(locale_rx.recv().await.unwrap(), "de");
        assert_eq!(fallback_rx.recv().await.unwrap(), "nl");
        assert_eq!(locale_current.read().await.as_deref(), Some("de"));
        assert_eq!(fallback_current.read().await.as_deref(), Some("nl"));
    }

    /// F-5 (task 13 fix round): both channel sends must happen before either
    /// `AppState` field is written, so a failed `fallback_tx.send` — here,
    /// simulated by dropping its receiver, the shape the core's `select!`
    /// loop takes once it is gone — leaves `locale_current` untouched and
    /// never reaches `locale_tx` at all. Before this fix, `locale_current`
    /// was written first: this same request would have left it at `"de"`
    /// while `locale_tx` stayed silent, the HTTP layer and the core then
    /// disagreeing about the chosen language.
    ///
    /// **[MUTATION]**: move the two `write().await` lines back above the two
    /// sends — this test fails, `locale_current` reading `Some("de")`
    /// instead of the original `Some("fr")`.
    #[tokio::test]
    async fn put_locale_writes_neither_field_when_the_fallback_send_fails() {
        let (state, mut locale_rx, fallback_rx, _dir) = app_state_fr();
        drop(fallback_rx);
        let locale_current = state.locale_current.clone();
        let fallback_current = state.fallback_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"de","fallback":"nl"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(locale_rx.try_recv().is_err(), "locale_tx must never be reached once the fallback send has already failed");
        assert_eq!(locale_current.read().await.as_deref(), Some("fr"), "app_state_fr's initial value, unchanged");
        assert_eq!(fallback_current.read().await.as_deref(), None);
    }

    /// The rule the memorized-value scenario depends on at the HTTP layer:
    /// a plain `{"locale": ...}` body — no `fallback` field at all, exactly
    /// what the settings page sends once the fallback control is hidden
    /// because the newly chosen language is complete — must leave whatever
    /// fallback was persisted before untouched, neither cleared nor
    /// notified again.
    #[tokio::test]
    async fn put_locale_without_a_fallback_field_leaves_the_persisted_fallback_untouched() {
        let (state, _rx, mut fallback_rx, _dir) = app_state_fr();
        *state.fallback_current.write().await = Some("nl".to_string());
        let fallback_current = state.fallback_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"en"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(fallback_rx.try_recv().is_err(), "no fallback field means no fallback event");
        assert_eq!(fallback_current.read().await.as_deref(), Some("nl"), "the stored fallback must survive");
    }

    /// The owner's rule that a fallback equal to the chosen language must be
    /// accepted, not refused — refusing it would look, from the settings
    /// page, exactly like the loss the test above guards against.
    #[tokio::test]
    async fn put_locale_accepts_a_fallback_equal_to_the_chosen_locale() {
        let (state, _rx, mut fallback_rx, _dir) = app_state_fr();
        let fallback_current = state.fallback_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"fr","fallback":"fr"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(fallback_rx.recv().await.unwrap(), "fr");
        assert_eq!(fallback_current.read().await.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn put_locale_refuses_a_fallback_that_is_not_a_language_code() {
        // Same regression model as `put_locale_refuses_a_value_that_is_not_
        // a_language_code`, for the `fallback` field: a shape-invalid value
        // must be refused **before** any update — including the otherwise
        // valid `locale` field of the very same request, which must not be
        // written as a side effect of a request that is refused overall.
        let (state, mut locale_rx, mut fallback_rx, _dir) = app_state_fr();
        let locale_current = state.locale_current.clone();
        let fallback_current = state.fallback_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/locale")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locale":"de","fallback":"../../var/lib/whatever"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(locale_rx.try_recv().is_err(), "an invalid fallback must block the locale event too");
        assert!(fallback_rx.try_recv().is_err());
        assert_eq!(locale_current.read().await.as_deref(), Some("fr"), "the locale field must not have been written either");
        assert_eq!(fallback_current.read().await.as_deref(), None);
    }

    #[tokio::test]
    async fn get_locale_reports_the_persisted_fallback() {
        // "fr", not "en": it must be the stored value read back, not the
        // clamp's own default reappearing by coincidence. "fr" is installed
        // in this rig's own `core/` (`app_state_fr` writes it), so this test
        // stays clear of the clamp added below — that one has a test of its
        // own, on an uninstalled language.
        let (state, _rx, _frx, _dir) = app_state_fr();
        *state.fallback_current.write().await = Some("fr".to_string());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["fallback_current"], "fr");
    }

    /// F-3 (task 13 fix round): a fallback stored while its pack was
    /// installed must not be echoed once that pack is gone — the same
    /// discipline `status_json` already applies to `locale_current` against
    /// `core_languages` (`status/mod.rs`). Otherwise `/api/locale` reports a
    /// `fallback_current` absent from its own `fallback_candidates`, which a
    /// reka-ui `Select` bound to it renders empty.
    ///
    /// **[MUTATION]**: remove the `.filter(...)` clamp in `locale_json` —
    /// this test fails, asserting `"nl"` (echoed verbatim) instead of `"en"`.
    #[tokio::test]
    async fn get_locale_clamps_an_uninstalled_fallback_to_en() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        // "nl" is not among this rig's core languages (only "en" and "fr"
        // — see `app_state_fr`'s own `core/fr.toml`): exactly the "pack
        // removed after being selected" case the clamp exists for.
        *state.fallback_current.write().await = Some("nl".to_string());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["fallback_current"], "en");
        let fallback_candidates: Vec<String> = serde_json::from_value(v["fallback_candidates"].clone()).unwrap();
        assert!(
            !fallback_candidates.contains(&"nl".to_string()),
            "the clamp only matters because nl is genuinely absent from the candidate list"
        );
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

    fn layers(pairs: &[(&str, &[(&str, &str)])]) -> ritornello_i18n::ModuleLayers {
        let mut m = ritornello_i18n::ModuleLayers::new("test");
        for (lang, kv) in pairs {
            let source: String = kv.iter().map(|(k, v)| format!("{k} = {v:?}\n")).collect();
            m.insert(*lang, ritornello_i18n::Layer::parse(&source).unwrap());
        }
        m
    }

    /// The regression this task was opened to fix: a language only a
    /// connected plugin translates must be in `locales` (the union), even
    /// though the core's own `core/` directory has never heard of it —
    /// the core-only list this route reported before task 12 would have
    /// missed it entirely.
    #[tokio::test]
    async fn get_locale_lists_a_language_only_a_plugin_translates() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        state.registry.write().await.insert_announced(
            "radio",
            layers(&[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]),
        );
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let locales: Vec<String> = serde_json::from_value(v["locales"].clone()).unwrap();
        assert!(locales.contains(&"de".to_string()), "de must be in the union: {locales:?}");
    }

    /// A language a plugin translates only partially must be reported as
    /// such — `done`/`total`, not `complete: true` — rather than the route
    /// declaring victory because the language exists at all.
    #[tokio::test]
    async fn get_locale_completeness_reports_a_partial_language_honestly() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        // The core (seeded by `app_state_fr`) is the only module with
        // text, and its French disk pack (`core/fr.toml`, written by the
        // fixture) defines only 2 of the core's many embedded English
        // keys — a real partial, not a contrived one.
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let completeness: Vec<serde_json::Value> = serde_json::from_value(v["completeness"].clone()).unwrap();
        let fr = completeness.iter().find(|c| c["language"] == "fr").expect("fr must be reported");
        assert_eq!(fr["complete"], false, "fr covers only 2 of the core's keys, not every one");
        assert_eq!(fr["total"], 1, "only the core has text in this rig");
        assert_eq!(fr["done"], 0);
    }

    /// The mirror case: a language every counted module covers completely
    /// must read `complete: true`, so the SPA (task 14) never has to
    /// compare `done` to `total` itself.
    #[tokio::test]
    async fn get_locale_completeness_reports_a_complete_language_as_complete() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        // English is always complete against itself.
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let completeness: Vec<serde_json::Value> = serde_json::from_value(v["completeness"].clone()).unwrap();
        let en = completeness.iter().find(|c| c["language"] == "en").expect("en must be reported");
        assert_eq!(en["complete"], true);
        assert_eq!(en["done"], en["total"]);
    }

    /// `complete_modules` names the modules a language actually covers —
    /// added (fix round 1, task 14 review, finding 1/R1) so the SPA can
    /// compute a true set union between the chosen language and a
    /// candidate fallback instead of approximating it with
    /// `Math.max(chosen.done, fallback.done)`, a bound the review measured
    /// as reachable up to "3 still in English" when the truth is 0.
    #[tokio::test]
    async fn get_locale_completeness_names_the_complete_modules() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let completeness: Vec<serde_json::Value> = serde_json::from_value(v["completeness"].clone()).unwrap();
        // English is complete against itself: its only counted module
        // ("core", the only module with text in this rig) must be named.
        let en = completeness.iter().find(|c| c["language"] == "en").expect("en must be reported");
        let en_modules: Vec<String> = serde_json::from_value(en["complete_modules"].clone()).unwrap();
        assert_eq!(en_modules, vec!["core".to_string()]);
        // The mirror case: fr covers only 2 of the core's keys (a real
        // `Partial`, never `Complete`), so its list must be empty — a
        // caller must not have to subtract `done` from `total` itself to
        // learn that nothing is covered.
        let fr = completeness.iter().find(|c| c["language"] == "fr").expect("fr must be reported");
        let fr_modules: Vec<String> = serde_json::from_value(fr["complete_modules"].clone()).unwrap();
        assert_eq!(fr_modules, Vec::<String>::new());
    }

    /// Fix round 1 (task 14 review, finding 3/R3). Before this clamp,
    /// `current` was the one site of this exact discipline left unclamped:
    /// `status_json`'s own `locale` field already clamps `locale_current`
    /// against `core_languages`, and `fallback_current` — see
    /// `get_locale_clamps_an_uninstalled_fallback_to_en`, above — already
    /// clamps against `fallback_candidates`. A selected language whose pack
    /// was removed used to be echoed verbatim: the SPA then found no
    /// `completeness` entry for it and rendered it as a *complete*
    /// language — the trigger showing the removed name, no annotation, no
    /// fallback control — while the rest of the page was in English.
    ///
    /// **[MUTATION]**: remove the `.filter(...)` clamp on `current` — this
    /// test fails, asserting `"de"` instead of `null`.
    #[tokio::test]
    async fn get_locale_clamps_a_removed_current_language_to_none() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        // "de" is not in this rig's union (only "en" and "fr" — see
        // `app_state_fr`'s own `core/fr.toml`): exactly the "pack removed
        // after being selected" case the clamp exists for.
        *state.locale_current.write().await = Some("de".to_string());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], serde_json::Value::Null);
        let locales: Vec<String> = serde_json::from_value(v["locales"].clone()).unwrap();
        assert!(
            !locales.contains(&"de".to_string()),
            "the clamp only matters because de is genuinely absent from the union"
        );
    }

    /// The clamp on `current` must use the **union** (`locales`), not the
    /// core-only `fallback_candidates` list — a chosen language only a
    /// plugin ships must survive it, or the clamp would silently
    /// reintroduce the defect this chantier's union fixes (see
    /// `LocaleResponse::locales`'s own doc): a plugin-only language
    /// becoming unreachable again.
    ///
    /// **[MUTATION]**: clamp `current` against `fallback_candidates`
    /// (core-only) instead of `locales` — this test fails, asserting
    /// `null` instead of `"de"`.
    #[tokio::test]
    async fn get_locale_does_not_clamp_a_plugin_only_current_language() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        state.registry.write().await.insert_announced(
            "radio",
            layers(&[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]),
        );
        *state.locale_current.write().await = Some("de".to_string());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], "de");
    }

    /// The eligible fallback list stays the **core's own** languages even
    /// when a plugin translates a language the core does not — the owner's
    /// arbitration: a fallback must be guaranteed to resolve everywhere.
    #[tokio::test]
    async fn get_locale_fallback_candidates_stay_core_only() {
        let (state, _rx, _frx, _dir) = app_state_fr();
        state.registry.write().await.insert_announced(
            "radio",
            layers(&[("en", &[("play", "Play")]), ("de", &[("play", "Spielen")])]),
        );
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/locale").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let fallback_candidates: Vec<String> = serde_json::from_value(v["fallback_candidates"].clone()).unwrap();
        assert_eq!(fallback_candidates, vec!["en".to_string(), "fr".to_string()]);
        assert!(!fallback_candidates.contains(&"de".to_string()), "de is a plugin language, not a core one");
        assert_eq!(v["fallback_current"], "en", "no fallback persisted yet on this rig, so the default applies");
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
        let (state, _rx, _frx, _dir) = tests_support::app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["audio_output"], "Sortie audio");
    }
}
