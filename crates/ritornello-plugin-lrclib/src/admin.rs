//! Admin backend of the lrclib plugin: serves the Vue page shipped in
//! `ui/dist`, reports what the lyrics cache holds, and empties it on request.
//!
//! The cache is the very one the `metadata` half reads and writes, shared
//! behind a lock: a page that cleared a copy would leave the plugin serving
//! from the old one until it restarted.

use crate::cache::Cache;
use ritornello_plugin_sdk::AdminPlugin;
use ritornello_proto::Text;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// What the page sends. A closed set: an unknown action is refused as a
/// malformed request, never read as some default gesture.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Write {
    Clear,
}

pub struct LrclibAdmin {
    cache: Arc<Mutex<Cache>>,
}

impl LrclibAdmin {
    pub fn new(cache: Arc<Mutex<Cache>>) -> Self {
        Self { cache }
    }
}

#[async_trait::async_trait]
impl AdminPlugin for LrclibAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string())),
            "ui.css" => Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string())),
            _ => None,
        }
    }

    async fn get_data(&self) -> serde_json::Value {
        // The index lives in memory: no disk read under the lock, so the
        // admin route answers at once (no HTTP route may block).
        let stats = crate::lock(&self.cache).stats();
        serde_json::json!({ "bytes": stats.bytes, "found": stats.found, "absent": stats.absent })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), Text> {
        let write: Write = serde_json::from_value(data).map_err(|e| Text::Keyed {
            key: "bad_request".into(),
            params: HashMap::from([("detail".to_string(), e.to_string())]),
        })?;
        match write {
            Write::Clear => crate::lock(&self.cache).clear().map_err(|e| {
                // The I/O detail goes to the log, not to the page: a raw OS
                // message is neither translated nor actionable there.
                tracing::warn!("could not empty the lyrics cache: {e}");
                Text::Keyed { key: "clear_failed".into(), params: HashMap::new() }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{Cached, Key};
    use crate::client::Query;
    use ritornello_proto::Lyrics;
    use std::time::SystemTime;

    fn query(title: &str) -> Query {
        Query { artist: "Daft Punk".into(), title: title.into(), album: None, duration_s: Some(248) }
    }

    fn filled(dir: &std::path::Path) -> Arc<Mutex<Cache>> {
        let mut cache = Cache::open(dir);
        let lyrics = Lyrics { plain: Some("Like the legend of the phoenix".into()), synced: None };
        cache.put(&Key::of(&query("Get Lucky")), Cached::Found(lyrics), SystemTime::now());
        cache.put(&Key::of(&query("Unknown")), Cached::Absent { since: SystemTime::now() }, SystemTime::now());
        Arc::new(Mutex::new(cache))
    }

    #[tokio::test]
    async fn get_data_reports_the_size_and_the_counts() {
        let dir = tempfile::tempdir().unwrap();
        let admin = LrclibAdmin::new(filled(dir.path()));
        let data = admin.get_data().await;
        assert_eq!(data["found"], 1);
        assert_eq!(data["absent"], 1);
        assert!(data["bytes"].as_u64().unwrap() > 0, "two entries weigh something: {data}");
    }

    #[tokio::test]
    async fn clear_empties_the_cache_and_reports_zero() {
        let dir = tempfile::tempdir().unwrap();
        let cache = filled(dir.path());
        let mut admin = LrclibAdmin::new(cache.clone());
        assert!(admin.set_data(serde_json::json!({ "action": "clear" })).await.is_ok());
        assert_eq!(admin.get_data().await, serde_json::json!({ "bytes": 0, "found": 0, "absent": 0 }));
        // The plugin's own handle sees it too: one cache, not two copies.
        let mut shared = crate::lock(&cache);
        assert_eq!(shared.get(&Key::of(&query("Get Lucky")), SystemTime::now()), None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "the files are gone from the disk");
    }

    #[tokio::test]
    async fn an_unknown_action_is_a_bad_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut admin = LrclibAdmin::new(filled(dir.path()));
        for body in [serde_json::json!({ "action": "wipe_everything" }), serde_json::json!({})] {
            let err = admin.set_data(body).await.unwrap_err();
            assert!(matches!(&err, Text::Keyed { key, .. } if key == "bad_request"), "unexpected: {err:?}");
        }
        assert_eq!(admin.get_data().await["found"], 1, "nothing was applied");
    }

    #[tokio::test]
    async fn a_failed_clear_is_the_clear_failed_key_not_the_io_detail() {
        // The cache's directory removed from under it: `read_dir` fails, as
        // it would on a card gone read-only or a permission changed by hand.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("cache");
        let cache = filled(&sub);
        std::fs::remove_dir_all(&sub).unwrap();
        let mut admin = LrclibAdmin::new(cache);
        let err = admin.set_data(serde_json::json!({ "action": "clear" })).await.unwrap_err();
        assert_eq!(err, Text::Keyed { key: "clear_failed".into(), params: HashMap::new() });
    }

    #[test]
    fn unknown_assets_are_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let admin = LrclibAdmin::new(filled(dir.path()));
        assert_eq!(admin.asset("ui.js").unwrap().0, "text/javascript");
        assert_eq!(admin.asset("ui.css").unwrap().0, "text/css");
        assert!(admin.asset("../../../etc/passwd").is_none());
        assert!(admin.asset("index.html").is_none());
    }

    /// Same guard as every plugin with a page: the language list is derived
    /// from the tree, and an empty derivation is a failure, not a pass.
    #[test]
    fn key_and_param_parity_between_the_embedded_en_and_every_shipped_language() {
        let en = ritornello_i18n::try_parse(crate::LRCLIB_EN).unwrap();
        let deploy_locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = ritornello_i18n::shipped_language_packs(&deploy_locales, "lrclib");
        assert!(!shipped.is_empty(), "no shipped language found for lrclib under deploy/locales");
        for (lang, content) in shipped {
            let pack = ritornello_i18n::try_parse(&content)
                .unwrap_or_else(|e| panic!("{lang} pack for lrclib is invalid TOML: {e}"));
            let mut en_keys: Vec<&String> = en.keys().collect();
            let mut pack_keys: Vec<&String> = pack.keys().collect();
            en_keys.sort();
            pack_keys.sort();
            assert_eq!(en_keys, pack_keys, "en/{lang} key sets diverge for lrclib");
            for (key, en_value) in &en {
                if let Some(translated) = pack.get(key) {
                    assert_eq!(
                        ritornello_i18n::params_in(en_value),
                        ritornello_i18n::params_in(translated),
                        "key {key}: {lang} translation's named parameters diverge from English"
                    );
                }
            }
        }
    }
}
