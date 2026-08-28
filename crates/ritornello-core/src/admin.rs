use crate::status::AppState;
use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Abstraction des opérations d'admin dont les routes du cœur ont besoin.
/// Implémentée par `AdminClient` (IPC réel) ; un faux l'implémente en test.
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>>;
    async fn catalog(&self) -> Result<serde_json::Value>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>;
    /// Sonde à 500 ms, sans verrou côté greffon : `Err(Timeout)` = occupé,
    /// `Err(Closed)` = mort.
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
    async fn catalog(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_catalog(self).await
    }
    async fn get_data(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_data(self).await
    }
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        ritornello_plugin_sdk::AdminClient::set_data(self, data).await
    }
}

/// Pages d'admin joignables, par name de greffon.
///
/// Sous verrou, et non plus figée au démarrage : un greffon peut s'annoncer
/// **après** le rassemblement (voir `register`), et sa page doit alors
/// apparaître sans redémarrer le cœur. Le `RwLock` est celui de tokio, comme le
/// reste de l'état partagé avec le routeur.
///
/// Les routes ne gardent jamais le verrou pendant un aller-retour IPC : elles
/// clonent l'`Arc` du dorsal et relâchent aussitôt.
pub type AdminBackends =
    std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<dyn AdminBackend>>>>;

/// Actifs d'IHM déjà récupérés, par `(plugin, path)` → `(mime, corps, etag)`.
/// Un bundle est immuable pour la durée de vie du processus du plugin : on ne
/// le relit pas par IPC à chaque rechargement de page.
///
/// **« Pour la durée de vie du processus du plugin » est un invariant, pas une
/// remarque**, et il n'était tenu par personne : rien ne purgeait ce cache quand
/// ce processus s'arrêtait. Un greffon relancé à la main avec son `ui.js`
/// reconstruit servait donc l'ancien jusqu'au redémarrage du cœur — ce qui pique
/// surtout en développement, là où c'est justement le geste courant.
/// `forget_page` est ce qui le tient désormais.
pub type AssetCache = tokio::sync::RwLock<
    std::collections::HashMap<(String, String), (String, String, String)>,
>;

/// Oublie tout ce que le cœur garde de la page d'admin de `name` : son dorsal et
/// ses active mis en cache.
///
/// **Un seul point de purge, appelé partout où le processus du greffon
/// s'arrête** — mort observée par la supervision, mort déduite de la fermeture
/// des sockets, extinction demandée, et ré-announcement (qui est la fin d'un
/// processus suivie du début d'un autre). C'est délibérément une fonction et non
/// deux lines recopiées : les deux registres doivent tomber *ensemble*, et un
/// invariant dont la justesse dépend de quatre sites de purge finit par mentir
/// sur l'un d'eux.
///
/// Ce que le retrait du dorsal achète : `/api/admin/<name>` répond un 404 franc
/// — « plugin inconnu » — au lieu d'un aller-retour IPC sur un socket fermé.
/// L'échec y était rapide (écrire sur un socket dont le pair a fermé rend
/// `EPIPE` tout de suite), donc le gain n'est pas de la latence sauf dans une
/// course étroite : si l'écriture entre dans le buffer avant que la fermeture
/// soit traitée, la réponse n'arrive jamais et le budget de la requête s'écoule
/// en entier. Le vrai gain est de dire la vérité.
pub async fn forget_page(backends: &AdminBackends, active: &AssetCache, name: &str) {
    backends.write().await.remove(name);
    // `retain` et non `remove` : la clé porte le path de l'active, donc un
    // greffon en a autant d'entrées qu'il a servi de fichiers.
    active.write().await.retain(|(greffon, _), _| greffon != name);
}

fn etag_of(body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    format!("\"{:x}\"", h.finish())
}

/// Réponse à une panne du dialogue d'admin avec un plugin.
///
/// En un seul endroit parce que les quatre routes d'admin faisaient la même
/// chose de la même façon fautive : journaliser la cause, puis renvoyer un 502
/// dont le corps était le texte brut « plugin injoignable ». Le client web ne
/// read que `{"error": …}` ; un corps en texte brut le faisait retomber sur
/// « HTTP 502 », un code nu à l'écran pour une panne dont la cause était connue
/// une line plus haut.
async fn plugin_refusal(st: &AppState, name: &str, contexte: &str, e: &anyhow::Error) -> Response {
    // Le journal garde la cause **entière** et en anglais : c'est elle qui sert
    // au diagnostic à distance, et elle est souvent plus précise que la phrase
    // affichée.
    tracing::warn!("plugin {name} admin unreachable ({contexte}): {e}");
    // Le code HTTP suit la cause, comme le message : 504 quand c'est le temps
    // qui a manqué, 502 quand c'est le greffon.
    let (code, key) = match e.downcast_ref::<ritornello_plugin_sdk::AdminIpcError>() {
        // Vivant mais trop lent : dire « injoignable » enverrait redémarrer un
        // processus qui tourne, au lieu de regarder le réseau.
        Some(ritornello_plugin_sdk::AdminIpcError::Timeout) => (StatusCode::GATEWAY_TIMEOUT, "plugin_timeout"),
        _ => (StatusCode::BAD_GATEWAY, "plugin_unreachable"),
    };
    let msg = st.catalog.read().await.get(key).to_string();
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// `ui.js` ou `ui.css` d'un plugin. Le name du fichier vient du path de la
/// route, jamais d'une liste en dur : le cœur ne sait pas ce qu'un plugin
/// expose.
pub async fn admin_asset(
    State(st): State<AppState>,
    Path((name, fichier)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Cloné puis verrou relâché : la suite fait des allers-retours IPC, et
    // les tenir sous un verrou de playback retarderait l'insertion d'un greffon
    // qui s'announcement en retard.
    let Some(backend) = st.admin_backends.read().await.get(&name).cloned() else {
        return (StatusCode::NOT_FOUND, "plugin inconnu").into_response();
    };
    let key = (name.clone(), fichier.clone());
    let en_cache = st.admin_assets.read().await.get(&key).cloned();
    let (mime, body, etag) = match en_cache {
        Some(v) => v,
        None => match backend.asset(&fichier).await {
            Ok(Some((mime, body))) => {
                let etag = etag_of(&body);
                let v = (mime, body, etag);
                st.admin_assets.write().await.insert(key, v.clone());
                v
            }
            Ok(None) => return (StatusCode::NOT_FOUND, "active inconnu").into_response(),
            Err(e) => return plugin_refusal(&st, &name, &format!("asset {fichier}"), &e).await,
        },
    };
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (axum::http::header::CONTENT_TYPE, mime.as_str()),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
            (axum::http::header::ETAG, etag.as_str()),
        ],
        body,
    )
        .into_response()
}

pub async fn admin_i18n(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    // Le verrou est relâché **avant** l'aller-retour IPC : un temporaire dans
    // le scrutin d'un `match` vivrait jusqu'à la fin du match, donc pendant
    // l'appel au greffon.
    let backend = st.admin_backends.read().await.get(&name).cloned();
    match backend {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.catalog().await {
            Ok(v) => Json(v).into_response(),
            Err(e) => plugin_refusal(&st, &name, "catalog", &e).await,
        },
    }
}

pub async fn admin_get_data(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    // Le verrou est relâché **avant** l'aller-retour IPC : un temporaire dans
    // le scrutin d'un `match` vivrait jusqu'à la fin du match, donc pendant
    // l'appel au greffon.
    let backend = st.admin_backends.read().await.get(&name).cloned();
    match backend {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
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
    // Le verrou est relâché **avant** l'aller-retour IPC : un temporaire dans
    // le scrutin d'un `match` vivrait jusqu'à la fin du match, donc pendant
    // l'appel au greffon.
    let backend = st.admin_backends.read().await.get(&name).cloned();
    match backend {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
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
        /// Le plugin répond, mais au-delà du cap de 5 s. Distinct de `down`
        /// justement parce que le message rendition doit l'être aussi.
        lent: bool,
        appels_asset: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Fake {
        async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
            if self.lent { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            self.appels_asset.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(match path {
                "ui.js" => Some(("text/javascript".to_string(), "export const contract = 1".to_string())),
                _ => None,
            })
        }
        async fn catalog(&self) -> Result<serde_json::Value> {
            if self.lent { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "btn_save": "Enregistrer" }))
        }
        async fn get_data(&self) -> Result<serde_json::Value> {
            if self.lent { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "stations": [] }))
        }
        async fn set_data(&self, _data: serde_json::Value) -> Result<Result<(), String>> {
            if self.lent { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(if self.reject { Err("présélection en double".into()) } else { Ok(()) })
        }
        async fn ping(&self) -> Result<()> {
            if self.lent { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(())
        }
    }

    fn state_with(fake: Fake) -> AppState {
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
            locales_root: std::path::PathBuf::from("/nonexistent"),
            admin_backends: Arc::new(tokio::sync::RwLock::new(backends)),
            admin_assets: Arc::new(Default::default()),
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
    async fn ui_js_est_servi_avec_son_type_et_un_etag() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/javascript");
        assert!(resp.headers().contains_key("etag"));
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("contract"));
    }

    #[tokio::test]
    async fn ui_js_est_mis_en_cache_apres_le_premier_acces() {
        // Un bundle est immuable pour la duration de vie du processus du plugin :
        // le relire par IPC a chaque rechargement de page serait du gaspillage.
        let fake = Fake::default();
        let appels = fake.appels_asset.clone();
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
        assert_eq!(appels.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oublie_page_rend_le_404_franc_et_fait_relire_lui_apres_une_re_annonce() {
        // Trois propriétés d'un coup, et toutes par le **comportement observé**
        // plutôt que par le contenu d'une table : ce qui compte n'est pas qu'une
        // clé ait disparu, c'est ce que la route répond ensuite.
        let fake = Fake::default();
        let appels = fake.appels_asset.clone();
        let state = state_with(fake);
        let app = router(state.clone());

        let get = |app: axum::Router| async move {
            app.oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
                .await
                .unwrap()
        };

        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(appels.load(std::sync::atomic::Ordering::SeqCst), 1, "mis en cache");

        // 1. Purger un **autre** greffon n'emporte rien ici. La clé du cache
        //    porte `(greffon, path)`, donc la purge passe par un `retain` : se
        //    tromper de moitié de clé aurait vidé le cache entier.
        forget_page(&state.admin_backends, &state.admin_assets, "autre").await;
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(appels.load(std::sync::atomic::Ordering::SeqCst), 1, "toujours en cache");

        // 2. Une fois le greffon oublié, la route dit franchement qu'il n'y a
        //    rien là — c'est la moitié du correctif qui retire la page morte du
        //    menu au lieu de rendre une erreur d'IPC.
        forget_page(&state.admin_backends, &state.admin_assets, "radio").await;
        assert_eq!(get(app.clone()).await.status(), StatusCode::NOT_FOUND);

        // 3. Et une ré-announcement relit vraiment : c'est la séquence de
        //    `hotplug` — oublier, puis recâbler. Sans la purge des
        //    active, le greffon relancé avec un `ui.js` reconstruit servait
        //    encore l'ancien jusqu'au redémarrage du cœur.
        state
            .admin_backends
            .write()
            .await
            .insert("radio".into(), Arc::new(Fake { appels_asset: appels.clone(), ..Default::default() }));
        assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
        assert_eq!(
            appels.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "le nouveau processus doit etre relu, pas servi depuis le cache de l'ancien"
        );
    }

    #[tokio::test]
    async fn if_none_match_repond_304() {
        let app = router(state_with(Fake::default()));
        let premier = app
            .clone()
            .oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let etag = premier.headers()["etag"].to_str().unwrap().to_string();
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
    async fn un_actif_inconnu_du_plugin_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.css").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn le_catalogue_du_plugin_est_servi_a_plat() {
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
    async fn ui_js_dun_plugin_inconnu_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/inconnu/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn la_page_dadmin_reste_servie_par_la_spa() {
        // Point de vigilance : la nouvelle route `/plugins/:name/:fichier` ne
        // doit pas capter `/plugins/<name>/` (segment final clear), qui doit
        // continuer de tomber sur le repli et serve le shell — c'est l'URL
        // historique, presente dans le README et dans les liens de la page de
        // statut.
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
    }

    #[tokio::test]
    async fn get_data_relaie_le_json() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["stations"].is_array());
    }

    #[tokio::test]
    async fn put_data_valide_renvoie_204() {
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
    async fn put_data_invalide_renvoie_422_avec_message() {
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
        assert_eq!(v["error"], "présélection en double");
    }

    // Depuis la Task 10, un name de plugin inconnu sur la route d'*active*
    // (`/plugins/<name>/ui.js`) 404 (voir `ui_js_dun_plugin_inconnu_repond_404`),
    // et `/plugins/<name>/` (segment final clear, URL historique) tombe sur le
    // repli SPA (voir `la_page_dadmin_reste_servie_par_la_spa`), qui rend
    // toujours le shell quel que soit le name. Les *données*
    // (`api/data`) restent strictes : un name de plugin inconnu y 404 toujours,
    // pour ne jamais masquer une faute de frappe derrière une réponse 200.
    #[tokio::test]
    async fn plugin_inconnu_sert_le_shell() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/inconnu/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn plugin_inconnu_sur_lapi_de_donnees_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/inconnu/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn un_plugin_injoignable_dit_pourquoi_au_lieu_dun_code_nu() {
        // Symptôme signalé : l'écran affichait « HTTP 502 ». Le client web ne
        // sait read que `{"error": …}` ; un corps en texte brut le faisait
        // retomber sur le code, alors que la cause était connue.
        let app = router(state_with(Fake { down: true, ..Default::default() }));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let corps = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&corps).expect("corps JSON");
        let msg = json["error"].as_str().expect("champ error");
        // Une phrase, pas une clé de sources_catalog : le repli clé par clé de
        // `Catalog::get` est silencieux, et une clé nue s'afficherait telle
        // quelle.
        assert!(msg.contains(' '), "key brute renvoyee a l'ecran : {msg}");
    }

    #[tokio::test]
    async fn un_plugin_trop_lent_ne_se_dit_pas_injoignable() {
        // Deux pannes distinctes, deux conduites à tenir : un plugin mort
        // appelle un redémarrage, un plugin trop lent envoie regarder le
        // réseau. Le cœur les aplatissait en un seul message.
        let lent = router(state_with(Fake { lent: true, ..Default::default() }));
        let r1 = lent
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::GATEWAY_TIMEOUT);
        let c1 = r1.into_body().collect().await.unwrap().to_bytes();
        let m1 = serde_json::from_slice::<serde_json::Value>(&c1).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();

        let mort = router(state_with(Fake { down: true, ..Default::default() }));
        let r2 = mort
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let c2 = r2.into_body().collect().await.unwrap().to_bytes();
        let m2 = serde_json::from_slice::<serde_json::Value>(&c2).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();

        assert_ne!(m1, m2, "le timeout depasse et la panne rendent le meme message");
        assert!(m1.contains(' ') && m2.contains(' '), "key brute : {m1} / {m2}");
    }

    #[tokio::test]
    async fn un_plugin_trop_lent_rend_504_et_un_plugin_mort_502() {
        let lent = router(state_with(Fake { lent: true, ..Default::default() }));
        let r1 = lent.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r1.status(), StatusCode::GATEWAY_TIMEOUT);
        let mort = router(state_with(Fake { down: true, ..Default::default() }));
        let r2 = mort.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r2.status(), StatusCode::BAD_GATEWAY);
    }
}
