use crate::status::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ritornello_i18n::Catalog;
use serde::{Deserialize, Serialize};

/// Preset par défaut de l'installation. Le cœur n'en connaît que le nom.
pub const DEFAULT_THEME: &str = "northern-lights";
/// Mode par défaut. Il n'existe **pas** de mode `system` : le défaut est
/// explicite et persisté, comme la locale.
pub const DEFAULT_MODE: &str = "light";

const MAX_NOM: usize = 64;

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

/// Erreur de validation du thème. Suit le modèle de `ValidationError`
/// (`ritornello-plugin-radio/src/config.rs`) : le texte utilisateur est
/// produit à la frontière via `message(&Catalog)`, `Display` fournit une
/// version anglaise pour les journaux.
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

/// Valide la **forme** seulement : le cœur ne connaît pas la liste des 42
/// presets (elle vit dans la SPA) et ne peut donc pas vérifier l'existence du
/// preset demandé. Il vérifie en revanche que le nom est un identifiant
/// plausible — ce qui écarte au passage les valeurs qui n'auraient rien à
/// faire dans un fichier d'état ou dans une page HTML.
///
/// Fonction pure, sans catalogue : `theme_put` résout l'erreur rendue contre
/// celui du cœur.
pub fn validate(theme: &str, mode: &str) -> Result<(), ThemeError> {
    if mode != "light" && mode != "dark" {
        return Err(ThemeError::UnknownMode { mode: mode.to_string() });
    }
    if theme.is_empty() || theme.len() > MAX_NOM {
        return Err(ThemeError::InvalidNameLength);
    }
    if !theme.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err(ThemeError::InvalidNameChars);
    }
    Ok(())
}

/// État de thème au démarrage, à partir de ce que porte `state.json`, avec repli
/// sur les défauts pour toute valeur invalide.
///
/// `theme_put` validait déjà le chemin HTTP, mais `main.rs` relisait
/// `theme`/`mode` depuis `state.json` **sans revalider**. Un fichier d'état
/// corrompu, édité à la main, ou écrit par une version antérieure, pouvait donc
/// porter un nom de thème inconnu. L'échappement d'`inject_theme` rend
/// l'injection inoffensive, mais un nom inconnu fait sortir `applyTheme`
/// (`web/kit/src/themes/engine.ts`) sur son `console.warn` **sans poser une
/// seule variable CSS** — et `theme.css` déclare `--color-background:
/// var(--background)` sans valeur de repli, donc toutes les couleurs se
/// résolvent à rien : l'IHM s'affiche **entièrement non thémée**.
///
/// Les deux champs sont jugés séparément : un mode invalide est inoffensif à
/// l'affichage (le spread de `applyTheme` retombe sur `light`) et ne doit pas
/// faire perdre un nom de thème par ailleurs valide, ni l'inverse.
pub fn from_persisted(theme: Option<&str>, mode: Option<&str>) -> ThemeState {
    let mut etat = ThemeState {
        theme: theme.unwrap_or(DEFAULT_THEME).to_string(),
        mode: mode.unwrap_or(DEFAULT_MODE).to_string(),
    };
    // `validate` juge les deux champs d'un bloc : on l'appelle deux fois, en
    // neutralisant l'autre champ avec sa valeur par défaut, pour attribuer
    // l'erreur au bon champ.
    // Le log nomme la **valeur rejetée**, pas le message de `ThemeError` : ce
    // message est destiné au lecteur d'un 422 et se résout contre le catalogue,
    // donc dans la langue de l'appareil, alors que les logs sont en anglais.
    // L'interpoler ici demanderait un catalogue que cette fonction pure n'a pas,
    // et mêlerait deux langues dans une ligne de journal. La valeur fautive est
    // de toute façon plus utile dans un journal que sa description.
    if validate(&etat.theme, DEFAULT_MODE).is_err() {
        tracing::warn!("invalid persisted theme {:?}, falling back to {DEFAULT_THEME}", etat.theme);
        etat.theme = DEFAULT_THEME.to_string();
    }
    if validate(DEFAULT_THEME, &etat.mode).is_err() {
        tracing::warn!("invalid persisted mode {:?}, falling back to {DEFAULT_MODE}", etat.mode);
        etat.mode = DEFAULT_MODE.to_string();
    }
    etat
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
    fn les_defauts_sont_northern_lights_en_clair() {
        assert_eq!(DEFAULT_THEME, "northern-lights");
        assert_eq!(DEFAULT_MODE, "light");
    }

    #[test]
    fn mode_accepte_uniquement_light_et_dark() {
        assert!(validate("vercel", "light").is_ok());
        assert!(validate("vercel", "dark").is_ok());
        // Pas de mode `system` : le defaut est explicite.
        assert!(validate("vercel", "system").is_err());
        assert!(validate("vercel", "").is_err());
    }

    #[test]
    fn from_persisted_sans_rien_de_persiste_donne_les_defauts() {
        assert_eq!(from_persisted(None, None), ThemeState::default());
    }

    #[test]
    fn from_persisted_conserve_des_valeurs_valides() {
        let e = from_persisted(Some("cyberpunk"), Some("dark"));
        assert_eq!(e.theme, "cyberpunk");
        assert_eq!(e.mode, "dark");
    }

    #[test]
    fn from_persisted_juge_les_deux_champs_separement() {
        // Un mode invalide ne doit pas faire perdre un nom de theme valide...
        let e = from_persisted(Some("cyberpunk"), Some("system"));
        assert_eq!(e.theme, "cyberpunk");
        assert_eq!(e.mode, DEFAULT_MODE);
        // ... ni l'inverse.
        let e = from_persisted(Some("Vercel!"), Some("dark"));
        assert_eq!(e.theme, DEFAULT_THEME);
        assert_eq!(e.mode, "dark");
    }

    #[test]
    fn un_state_json_portant_un_theme_invalide_se_charge_sur_les_defauts() {
        // `theme_put` valide le chemin HTTP, mais `main.rs` relisait
        // `theme`/`mode` depuis `state.json` sans revalider. Un nom de theme
        // inconnu fait sortir `applyTheme` sur son `console.warn` SANS poser
        // une seule variable CSS, et `theme.css` n'a pas de valeur de repli :
        // l'IHM s'affiche entierement non themee.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"theme":"../../etc/passwd","mode":"nawak"}"#,
        )
        .unwrap();
        let persisted = crate::state::load(&path);
        // Le fichier est bien relu tel quel : c'est la validation qui corrige.
        assert_eq!(persisted.theme.as_deref(), Some("../../etc/passwd"));
        let etat = from_persisted(persisted.theme.as_deref(), persisted.mode.as_deref());
        assert_eq!(etat, ThemeState::default());
        assert_eq!(etat.theme, DEFAULT_THEME);
        assert_eq!(etat.mode, DEFAULT_MODE);
    }

    #[test]
    fn le_coeur_valide_la_forme_du_nom_sans_connaitre_la_liste_des_presets() {
        // Un preset inconnu du coeur mais bien forme est accepte : la liste
        // des 42 presets vit dans la SPA, jamais ici.
        assert!(validate("un-preset-ajoute-plus-tard", "light").is_ok());
        // Formes refusees : vide, trop long, caracteres hors [a-z0-9-].
        assert!(validate("", "light").is_err());
        assert!(validate(&"a".repeat(65), "light").is_err());
        assert!(validate("Vercel", "light").is_err());
        assert!(validate("v e r c e l", "light").is_err());
        assert!(validate("../../etc/passwd", "light").is_err());
    }

    #[test]
    fn validate_rend_la_bonne_variante() {
        assert_eq!(validate("vercel", "system"), Err(ThemeError::UnknownMode { mode: "system".to_string() }));
        assert_eq!(validate("", "light"), Err(ThemeError::InvalidNameLength));
        assert_eq!(validate(&"a".repeat(65), "light"), Err(ThemeError::InvalidNameLength));
        assert_eq!(validate("Vercel", "light"), Err(ThemeError::InvalidNameChars));
    }

    #[test]
    fn message_de_theme_utilise_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "theme_unknown_mode = \"mode {mode} inconnu\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::core::EN);
        let err = ThemeError::UnknownMode { mode: "system".to_string() };
        assert_eq!(err.message(&cat), "mode system inconnu");
    }
}
