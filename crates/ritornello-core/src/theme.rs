use crate::status::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ritornello_i18n::Catalog;
use serde::{Deserialize, Serialize};

/// Default preset of the installation. The core only knows its name.
pub const DEFAULT_THEME: &str = "northern-lights";
/// Default mode. There is **no** `system` mode: the default is explicit and
/// persisted, like the locale.
pub const DEFAULT_MODE: &str = "light";

const MAX_NAME: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeState {
    pub theme: String,
    pub mode: String,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self { theme: DEFAULT_THEME.to_string(), mode: DEFAULT_MODE.to_string() }
    }
}

/// Theme validation error. Follows the model of `ValidationError`
/// (`ritornello-plugin-radio/src/config.rs`): the user-facing text is produced
/// at the boundary via `message(&Catalog)`, `Display` provides an English
/// version for the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeError {
    UnknownMode { mode: String },
    InvalidNameLength,
    InvalidNameChars,
}

impl ThemeError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ThemeError::UnknownMode { mode } => {
                catalog.get("theme_unknown_mode").replace("{mode}", mode)
            }
            ThemeError::InvalidNameLength => catalog.get("theme_name_invalid_length").to_string(),
            ThemeError::InvalidNameChars => catalog.get("theme_name_invalid_chars").to_string(),
        }
    }
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeError::UnknownMode { mode } => write!(f, "unknown mode: {mode}"),
            ThemeError::InvalidNameLength => write!(f, "invalid theme name length"),
            ThemeError::InvalidNameChars => write!(f, "theme name outside [a-z0-9-]"),
        }
    }
}

impl std::error::Error for ThemeError {}

/// Validates the **shape** only: the core does not know the list of the 42
/// presets (it lives in the SPA) and therefore cannot check that the requested
/// preset exists. It does check that the name is a plausible identifier — which
/// incidentally rules out values that would have no business in a state file
/// or in an HTML page.
///
/// Pure function, without a catalog: `theme_put` resolves the returned error
/// against the core's.
pub fn validate(theme: &str, mode: &str) -> Result<(), ThemeError> {
    if mode != "light" && mode != "dark" {
        return Err(ThemeError::UnknownMode { mode: mode.to_string() });
    }
    if theme.is_empty() || theme.len() > MAX_NAME {
        return Err(ThemeError::InvalidNameLength);
    }
    if !theme.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err(ThemeError::InvalidNameChars);
    }
    Ok(())
}

/// Theme state at startup, from what `state.json` carries, falling back to the
/// defaults for any invalid value.
///
/// `theme_put` already validated the HTTP path, but `main.rs` re-read
/// `theme`/`mode` from `state.json` **without revalidating**. A corrupted state
/// file, hand-edited, or written by an earlier version, could therefore carry
/// an unknown theme name. The escaping in `inject_theme` makes the injection
/// harmless, but an unknown name makes `applyTheme`
/// (`web/kit/src/themes/engine.ts`) exit on its `console.warn` **without
/// setting a single CSS variable** — and `theme.css` declares
/// `--color-background: var(--background)` with no fallback value, so every
/// color resolves to nothing: the UI renders **entirely unthemed**.
///
/// The two fields are judged separately: an invalid mode is harmless for
/// display (the spread in `applyTheme` falls back to `light`) and must not lose
/// an otherwise valid theme name, nor the reverse.
pub fn from_persisted(theme: Option<&str>, mode: Option<&str>) -> ThemeState {
    let mut state = ThemeState {
        theme: theme.unwrap_or(DEFAULT_THEME).to_string(),
        mode: mode.unwrap_or(DEFAULT_MODE).to_string(),
    };
    // `validate` judges both fields as a block: we call it twice, neutralizing
    // the other field with its default value, to attribute the error to the
    // right field.
    // The log names the **rejected value**, not the `ThemeError` message: that
    // message is meant for the reader of a 422 and resolves against the
    // catalog, hence in the device's language, whereas logs are in English.
    // Interpolating it here would require a catalog this pure function does not
    // have, and would mix two languages in one log line. The offending value is
    // in any case more useful in a log than its description.
    if validate(&state.theme, DEFAULT_MODE).is_err() {
        tracing::warn!("invalid persisted theme {:?}, falling back to {DEFAULT_THEME}", state.theme);
        state.theme = DEFAULT_THEME.to_string();
    }
    if validate(DEFAULT_THEME, &state.mode).is_err() {
        tracing::warn!("invalid persisted mode {:?}, falling back to {DEFAULT_MODE}", state.mode);
        state.mode = DEFAULT_MODE.to_string();
    }
    state
}

pub async fn theme_json(State(state): State<AppState>) -> Json<ThemeState> {
    Json(state.theme_current.read().await.clone())
}

pub async fn theme_put(State(state): State<AppState>, Json(req): Json<ThemeState>) -> Response {
    if let Err(e) = validate(&req.theme, &req.mode) {
        let msg = e.message(&*state.catalog.read().await);
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    *state.theme_current.write().await = req.clone();
    if state.theme_tx.send(req).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_northern_lights_in_light_mode() {
        assert_eq!(DEFAULT_THEME, "northern-lights");
        assert_eq!(DEFAULT_MODE, "light");
    }

    #[test]
    fn mode_accepts_only_light_and_dark() {
        assert!(validate("vercel", "light").is_ok());
        assert!(validate("vercel", "dark").is_ok());
        // No `system` mode: the default is explicit.
        assert!(validate("vercel", "system").is_err());
        assert!(validate("vercel", "").is_err());
    }

    #[test]
    fn from_persisted_with_nothing_persisted_gives_the_defaults() {
        assert_eq!(from_persisted(None, None), ThemeState::default());
    }

    #[test]
    fn from_persisted_keeps_valid_values() {
        let e = from_persisted(Some("cyberpunk"), Some("dark"));
        assert_eq!(e.theme, "cyberpunk");
        assert_eq!(e.mode, "dark");
    }

    #[test]
    fn from_persisted_judges_both_fields_separately() {
        // An invalid mode must not lose a valid theme name...
        let e = from_persisted(Some("cyberpunk"), Some("system"));
        assert_eq!(e.theme, "cyberpunk");
        assert_eq!(e.mode, DEFAULT_MODE);
        // ... nor the reverse.
        let e = from_persisted(Some("Vercel!"), Some("dark"));
        assert_eq!(e.theme, DEFAULT_THEME);
        assert_eq!(e.mode, "dark");
    }

    #[test]
    fn a_state_json_carrying_an_invalid_theme_loads_onto_the_defaults() {
        // `theme_put` validates the HTTP path, but `main.rs` re-read
        // `theme`/`mode` from `state.json` without revalidating. An unknown
        // theme name makes `applyTheme` exit on its `console.warn` WITHOUT
        // setting a single CSS variable, and `theme.css` has no fallback value:
        // the UI renders entirely unthemed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"theme":"../../etc/passwd","mode":"nawak"}"#,
        )
        .unwrap();
        let persisted = crate::state::load(&path);
        // The file is indeed re-read as is: validation is what corrects it.
        assert_eq!(persisted.theme.as_deref(), Some("../../etc/passwd"));
        let state = from_persisted(persisted.theme.as_deref(), persisted.mode.as_deref());
        assert_eq!(state, ThemeState::default());
        assert_eq!(state.theme, DEFAULT_THEME);
        assert_eq!(state.mode, DEFAULT_MODE);
    }

    #[test]
    fn the_core_validates_the_name_shape_without_knowing_the_preset_list() {
        // A preset unknown to the core but well-formed is accepted: the list of
        // the 42 presets lives in the SPA, never here.
        assert!(validate("a-preset-added-later", "light").is_ok());
        // Rejected shapes: empty, too long, characters outside [a-z0-9-].
        assert!(validate("", "light").is_err());
        assert!(validate(&"a".repeat(65), "light").is_err());
        assert!(validate("Vercel", "light").is_err());
        assert!(validate("v e r c e l", "light").is_err());
        assert!(validate("../../etc/passwd", "light").is_err());
    }

    #[test]
    fn validate_returns_the_right_variant() {
        assert_eq!(validate("vercel", "system"), Err(ThemeError::UnknownMode { mode: "system".to_string() }));
        assert_eq!(validate("", "light"), Err(ThemeError::InvalidNameLength));
        assert_eq!(validate(&"a".repeat(65), "light"), Err(ThemeError::InvalidNameLength));
        assert_eq!(validate("Vercel", "light"), Err(ThemeError::InvalidNameChars));
    }

    #[test]
    fn theme_message_uses_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "theme_unknown_mode = \"mode {mode} inconnu\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::i18n::EN);
        let err = ThemeError::UnknownMode { mode: "system".to_string() };
        assert_eq!(err.message(&cat), "mode system inconnu");
    }
}
