use crate::status::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;

/// Le `dist/` produit par `npm run build --workspaces`, embarqué à la
/// compilation. `build.rs` garantit qu'il existe (bouchon à défaut).
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

/// Les chunks de l'app portent un hash dans leur nom : ils sont immuables.
/// `vue.js` et `ui-kit.js` gardent au contraire un **nom stable** — c'est le
/// contrat que les modules de plugin importent — donc ils doivent être
/// revalidés (l'`ETag` s'en charge).
pub fn cache_control(path: &str) -> &'static str {
    let nom = path.rsplit('/').next().unwrap_or(path);
    if nom.starts_with("app-") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Le repli ne sert le shell **que** hors des espaces de données. Sans cette
/// restriction, une faute de frappe sur une route d'API répondrait 200 avec du
/// HTML — un piège à débogage coûteux.
pub fn serves_shell(path: &str) -> bool {
    if path.starts_with("/api/") || path.starts_with("/assets/") {
        return false;
    }
    if let Some(reste) = path.strip_prefix("/plugins/") {
        if let Some((_, apres)) = reste.split_once('/') {
            if apres.starts_with("api/") || apres.starts_with("ui.") {
                return false;
            }
            // Chemin d'actif profond (`/plugins/radio/assets/chunk.js`) : les
            // actifs d'un plugin ne sont servis que sur **un seul** segment
            // (`/plugins/<nom>/<fichier>`), donc un chemin à plusieurs segments
            // ne correspond à aucune route et tombait ici — le repli renvoyait
            // alors **le shell HTML en 200**, si bien qu'un `import()`
            // dynamique recevait du HTML : mode d'échec très déroutant, rien ne
            // signalant l'erreur. Il obtient maintenant un 404 propre.
            //
            // Le test `serves_shell("/plugins/<nom>/")` reste vert : `apres`
            // vaut alors la chaîne vide, qui ne contient pas de `/`. C'est
            // aussi pourquoi cette condition est préférée à un wildcard
            // `/plugins/:name/*fichier` dans le routeur, dont le reste vide ne
            // matche pas de façon fiable — et cette URL est un invariant.
            if apres.contains('/') {
                return false;
            }
        }
    }
    true
}

pub fn inject_theme(html: &str, theme: &str, mode: &str) -> String {
    // Le `Display` de `serde_json::Value` n'échappe pas `/` : une valeur
    // (par ex. `theme`) contenant `</script>` fermerait prématurément la
    // balise et permettrait d'injecter du HTML arbitraire dans le shell.
    // `theme::validate` interdit déjà ces caractères sur le chemin HTTP, mais
    // `main.rs` relit `theme`/`mode` depuis `state.json` sans revalider — un
    // fichier d'état corrompu ou édité à la main reste donc un vecteur.
    // On échappe donc chaque signe « inférieur » de la valeur sérialisée par
    // son équivalent JSON en notation `\u`+code-point (voir `replace`
    // ci-dessous) : ça reste du JSON strictement équivalent — le navigateur
    // le redécode à l'identique — mais la sous-chaîne `</script>` ne peut
    // plus apparaître littéralement dans le document produit.
    let payload =
        serde_json::json!({ "theme": theme, "mode": mode }).to_string().replace('<', "\\u003c");
    let script = format!("<script>window.__RITORNELLO_THEME__={payload};</script>");
    match html.find("</head>") {
        Some(i) => format!("{}{}{}", &html[..i], script, &html[i..]),
        None => format!("{script}{html}"),
    }
}

/// Le shell embarqué, ou le bouchon si `build.rs` n'a trouvé aucun livrable.
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
    let chemin = uri.path().trim_start_matches("/assets/");
    let Some(f) = Dist::get(&format!("assets/{chemin}")) else {
        return (StatusCode::NOT_FOUND, "actif inconnu").into_response();
    };
    let etag = etag_of(&f.metadata.sha256_hash());
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime_for(chemin)),
            (header::CACHE_CONTROL, cache_control(chemin)),
            (header::ETAG, etag.as_str()),
        ],
        f.data.to_vec(),
    )
        .into_response()
}

/// Repli du routeur : sert le shell pour les chemins de la SPA, 404 sinon.
pub async fn shell(State(state): State<AppState>, uri: Uri) -> Response {
    if !serves_shell(uri.path()) {
        return (StatusCode::NOT_FOUND, "inconnu").into_response();
    }
    let t = state.theme_current.read().await.clone();
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        inject_theme(&shell_html(), &t.theme, &t.mode),
    )
        .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/assets/*chemin", get(asset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    #[test]
    fn inject_theme_pose_le_choix_avant_la_fermeture_du_head() {
        let html = "<!doctype html><html><head><title>x</title></head><body></body></html>";
        let out = inject_theme(html, "cyberpunk", "dark");
        assert!(out.contains(r#"window.__RITORNELLO_THEME__={"mode":"dark","theme":"cyberpunk"}"#));
        assert!(out.find("__RITORNELLO_THEME__").unwrap() < out.find("</head>").unwrap());
    }

    #[test]
    fn inject_theme_survit_a_un_html_sans_head() {
        let out = inject_theme("<div>x</div>", "vercel", "light");
        assert!(out.contains("__RITORNELLO_THEME__"));
    }

    #[test]
    fn inject_theme_echappe_les_fermetures_de_script_dans_les_valeurs() {
        // `theme::validate` empêche ce cas sur le chemin HTTP normal, mais
        // `main.rs` relit `theme`/`mode` depuis `state.json` sans revalider :
        // un fichier d'état corrompu ou édité à la main reste un vecteur.
        // `inject_theme` doit rester sûre même en recevant une valeur hostile.
        let nom_hostile = "</script><script>alert(1)</script>";
        let out = inject_theme("<div>x</div>", nom_hostile, "dark");
        // La seule fermeture de balise `<script>` du document doit être
        // celle qui ferme réellement le script injecté : aucune fermeture
        // prématurée venue de la valeur ne doit apparaître littéralement.
        assert_eq!(out.matches("</script>").count(), 1);
        // Le JSON reste analysable malgré l'échappement, et le navigateur y
        // retrouverait la valeur d'origine, caractère pour caractère.
        let debut = out.find('{').unwrap();
        let fin = out.find(";</script>").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out[debut..fin]).unwrap();
        assert_eq!(v["theme"], nom_hostile);
    }

    #[test]
    fn le_repli_ne_sert_le_shell_que_hors_des_espaces_de_donnees() {
        // Les URL historiques et les routes du routeur Vue : shell.
        assert!(serves_shell("/"));
        assert!(serves_shell("/status"));
        assert!(serves_shell("/config"));
        assert!(serves_shell("/plugins/radio/"));
        assert!(serves_shell("/plugins/"));
        // Les espaces de donnees : jamais de shell, sinon une faute de frappe
        // sur une route d'API repondrait 200 avec du HTML — piege a debogage.
        assert!(!serves_shell("/api/statuss"));
        assert!(!serves_shell("/api/theme"));
        assert!(!serves_shell("/assets/inconnu.js"));
        assert!(!serves_shell("/plugins/radio/api/data"));
        assert!(!serves_shell("/plugins/radio/ui.js"));
        assert!(!serves_shell("/plugins/radio/ui.css"));
        // Chemin d'actif profond : les actifs d'un plugin ne sont servis que
        // sur un seul segment, donc `/plugins/radio/assets/chunk.js` ne matche
        // aucune route. Il tombait sur le repli, qui repondait 200 avec le
        // shell HTML — un `import()` dynamique recevait du HTML, mode d'echec
        // tres deroutant. Desormais un 404 propre.
        assert!(!serves_shell("/plugins/radio/assets/chunk.js"));
        assert!(!serves_shell("/plugins/radio/a/b/c.js"));
        // ... sans toucher a l'URL historique, qui doit continuer de servir le
        // shell : le reste apres le nom du plugin est vide, donc sans `/`.
        assert!(serves_shell("/plugins/radio/"));
        assert!(serves_shell("/plugins/generic-input/"));
        // Un actif plat reste possible : c'est le contrat des IHM de plugin.
        assert!(serves_shell("/plugins/radio/quelconque"));
    }

    #[tokio::test]
    async fn un_chemin_dactif_profond_de_plugin_repond_404_et_non_le_shell() {
        // Bout en bout a travers le routeur : c'est la reponse HTTP reelle qui
        // compte, un `import()` dynamique ne devant jamais recevoir du HTML.
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app
            .oneshot(Request::get("/plugins/radio/assets/chunk.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn mime_et_cache_selon_le_nom_du_fichier() {
        assert_eq!(mime_for("app-abc.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("app-abc.mjs"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("app-abc.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("logo.svg"), "image/svg+xml");
        assert_eq!(mime_for("police.woff2"), "font/woff2");
        assert_eq!(mime_for("favicon.png"), "image/png");
        assert_eq!(mime_for("inconnu.bin"), "application/octet-stream");
        // Nom hashe : immuable. Noms stables du contrat : a revalider.
        assert!(cache_control("app-abc123.js").contains("immutable"));
        assert!(!cache_control("vue.js").contains("immutable"));
        assert!(!cache_control("ui-kit.js").contains("immutable"));
    }

    #[tokio::test]
    async fn la_racine_sert_le_shell_avec_le_theme_injecte() {
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
    async fn un_chemin_inconnu_hors_api_sert_le_shell() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp =
            app.oneshot(Request::get("/plugins/quelconque/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn une_route_dapi_inconnue_repond_404_et_non_le_shell() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app.oneshot(Request::get("/api/statuss").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn le_contenu_embarque_est_servable() {
        // Vrai `dist/` ou bouchon : l'un des deux est necessairement present,
        // `build.rs` le garantit.
        let html = shell_html();
        assert!(!html.is_empty());
        assert!(html.contains("<!doctype html>") || html.contains("<!DOCTYPE html>"));
    }

    #[tokio::test]
    async fn un_actif_absent_repond_404() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app
            .oneshot(Request::get("/assets/nexiste-pas.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
