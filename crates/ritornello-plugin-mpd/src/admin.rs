use crate::config::Config;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Body of `SetData`, distinct from `Config`: both fields are **mandatory**
/// here, with no `#[serde(default = ...)]`. Those defaults are right for
/// `Config::load`, which completes a partial *file* — but wrong for a write,
/// where a `PUT {"port": 6601}` without `listen` must be rejected, not
/// silently understood as "put `listen` back to `0.0.0.0`".
/// Same read/write separation as `generic-input::admin::Op::Save` and
/// `radio::admin::Op::Save`, which each carry a type dedicated to the request,
/// distinct from the configuration type loaded from disk.
#[derive(Debug, Deserialize)]
struct ConfigWrite {
    listen: String,
    port: u16,
}

pub struct MpdAdmin {
    pub config_path: PathBuf,
    /// In-memory copy of the last successful save: `get_data` returns it
    /// without re-reading the disk on every request.
    pub config: RwLock<Config>,
    pub catalog: Arc<RwLock<Catalog>>,
    /// How the new configuration reaches the network half, which then rebinds
    /// without a restart (see `session::listen`).
    ///
    /// **Sent after the disk write and never before**: what is served must be
    /// what is saved, and a rebind on a configuration the file does not carry
    /// yet would be lost at the first real restart — the device would then
    /// listen somewhere other than where it announces.
    ///
    /// `Option` for the tests, which exercise validation and the write without
    /// setting up a socket. A `None` does nothing, silently: that is exactly
    /// the former behaviour.
    pub rebind_tx: Option<tokio::sync::watch::Sender<Config>>,
}

#[async_trait::async_trait]
impl AdminPlugin for MpdAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => {
                Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string()))
            }
            "ui.css" => Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string())),
            _ => None,
        }
    }

    fn catalog(&self) -> serde_json::Value {
        let cat = self.catalog.read().unwrap();
        serde_json::json!(cat.entries())
    }

    async fn get_data(&self) -> serde_json::Value {
        let c = self.config.read().unwrap();
        serde_json::json!({ "listen": c.listen, "port": c.port })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        // `ConfigWrite`, not `Config`: see the comment on the type — a missing
        // field must reject the request (`bad_request`), not get completed by
        // a *loading* default.
        let writer: ConfigWrite = serde_json::from_value(data).map_err(|e| {
            self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e.to_string())
        })?;
        let config = Config { listen: writer.listen, port: writer.port };
        // `save` validates then writes atomically; in both failure cases it
        // returns a catalog **key** (`listen_empty`, `port_zero`,
        // `save_failed`), never a raw I/O detail. It is here, and only here,
        // that the key becomes a sentence: the Vue page shows `error` as is,
        // without re-translating it (see the UI half's report) — returning the
        // bare key would make it appear literally on screen.
        config
            .save(&self.config_path)
            .map_err(|key| self.catalog.read().unwrap().get(&key).to_string())?;
        *self.config.write().unwrap() = config.clone();
        // The network half rebinds on its own. `send` fails only if nobody is
        // listening any more — the plugin is stopping — and there is then
        // nothing to rebind nor anything to report to the user: the file is
        // written, what they asked for is done.
        if let Some(tx) = &self.rebind_tx {
            let _ = tx.send(config);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        admin: MpdAdmin,
        _dir: tempfile::TempDir,
    }

    /// An admin wired to a rebind channel, for the tests that want to see what
    /// the network half receives.
    fn fixture_with_rebind() -> (Fixture, tokio::sync::watch::Receiver<Config>) {
        let mut f = fixture();
        let (tx, rx) = tokio::sync::watch::channel(Config::default());
        f.admin.rebind_tx = Some(tx);
        (f, rx)
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("mpd.toml");
        let catalog = Arc::new(RwLock::new(Catalog::load(
            "mpd",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::MPD_EN,
        )));
        Fixture {
            admin: MpdAdmin {
                config_path,
                config: RwLock::new(Config::default()),
                catalog,
                rebind_tx: None,
            },
            _dir: dir,
        }
    }

    #[test]
    fn asset_exposes_ui_js_and_ui_css_and_nothing_else() {
        let f = fixture();
        let (mime, body) = f.admin.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!body.is_empty());
        assert_eq!(f.admin.asset("ui.css").unwrap().0, "text/css");
        // An unknown path is not an error: it is a 404 on the core side.
        assert!(f.admin.asset("../../../etc/passwd").is_none());
        assert!(f.admin.asset("index.html").is_none());
    }

    #[test]
    fn catalog_exposes_the_component_keys() {
        let f = fixture();
        let v = f.admin.catalog();
        assert!(v["btn_save"].is_string(), "the catalog must carry the plugin keys");
    }

    /// French pack shipped in the repository.
    fn fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/mpd/fr.toml");
        std::fs::read_to_string(p).expect("shipped fr pack")
    }

    #[test]
    fn key_parity_between_the_embedded_en_and_the_fr_pack() {
        let en = ritornello_i18n::try_parse(crate::MPD_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let mut en_keys: Vec<&String> = en.keys().collect();
        let mut fr_keys: Vec<&String> = fr.keys().collect();
        en_keys.sort();
        fr_keys.sort();
        assert_eq!(en_keys, fr_keys, "en/fr key sets diverge");
    }

    #[tokio::test]
    async fn get_data_returns_the_current_settings() {
        let f = fixture();
        let v = f.admin.get_data().await;
        assert_eq!(v["listen"], "0.0.0.0");
        assert_eq!(v["port"], 6600);
    }

    #[tokio::test]
    async fn set_data_validates_persists_and_replaces_the_in_memory_copy() {
        let mut f = fixture();
        let op = serde_json::json!({ "listen": "192.168.1.10", "port": 6601 });
        assert!(f.admin.set_data(op).await.is_ok());
        assert_eq!(f.admin.get_data().await, serde_json::json!({ "listen": "192.168.1.10", "port": 6601 }));
        assert_eq!(Config::load(&f.admin.config_path).port, 6601);
    }

    #[tokio::test]
    async fn a_successful_save_makes_the_network_half_rebind() {
        // The owner's request: no longer having to restart the plugin by hand.
        // The page writes the file **then** pushes the configuration; the
        // network half binds the new address/port pair (see
        // `session::listen`).
        let (mut f, mut rx) = fixture_with_rebind();
        assert!(f.admin.set_data(serde_json::json!({ "listen": "127.0.0.1", "port": 6612 })).await.is_ok());
        assert!(rx.has_changed().unwrap(), "the network half must be notified");
        let c = rx.borrow_and_update().clone();
        assert_eq!((c.listen.as_str(), c.port), ("127.0.0.1", 6612));
        // And what is pushed is indeed what is on disk: serving an address the
        // file does not carry would make it vanish at the first real restart.
        assert_eq!(Config::load(&f.admin.config_path), c);
    }

    #[tokio::test]
    async fn a_rejected_save_rebinds_nothing() {
        // The counterpart: an invalid port must above all not make the serving
        // socket let go.
        let (mut f, rx) = fixture_with_rebind();
        assert!(f.admin.set_data(serde_json::json!({ "listen": "0.0.0.0", "port": 0 })).await.is_err());
        assert!(!rx.has_changed().unwrap(), "nothing must be pushed on a rejection");
    }

    #[tokio::test]
    async fn an_invalid_port_returns_a_catalog_sentence_not_the_raw_key() {
        // The regression this test blocks: `admin.rs` propagates
        // `Err("port_zero".into())` directly without resolving it through the
        // catalog. The Vue page shows `error` as is (no client-side
        // re-translation), so the user would literally read "port_zero" on
        // screen instead of the sentence. Verified by making this test fail
        // deliberately (resolution removed) before writing it for good: see
        // the task report for the proof.
        let mut f = fixture();
        let op = serde_json::json!({ "listen": "0.0.0.0", "port": 0 });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "The port must be between 1 and 65535.");
        assert_ne!(err, "port_zero");
        // Nothing was written, and the in-memory copy did not move.
        assert!(!f.admin.config_path.exists());
        assert_eq!(f.admin.get_data().await["port"], 6600);
    }

    #[tokio::test]
    async fn an_empty_address_returns_a_catalog_sentence() {
        let mut f = fixture();
        let op = serde_json::json!({ "listen": "", "port": 6600 });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "The listen address cannot be empty.");
    }

    #[tokio::test]
    async fn a_write_failure_returns_a_catalog_sentence_not_the_io_detail() {
        // Same regression as in generic-input and radio:
        // `save(...).map_err(|e| e.to_string())` would put the raw I/O detail
        // in the response body. `config_path` here targets an ordinary file as
        // if it were a parent directory, to make the temporary file's write
        // fail without touching the real disk.
        let mut f = fixture();
        let obstacle = f.admin.config_path.parent().unwrap().join("obstacle");
        std::fs::write(&obstacle, b"not a directory").unwrap();
        f.admin.config_path = obstacle.join("mpd.toml");
        let op = serde_json::json!({ "listen": "0.0.0.0", "port": 6600 });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "Could not save the settings.");
    }

    #[tokio::test]
    async fn a_malformed_request_returns_a_translated_error() {
        // `ConfigWrite` (the type of the `SetData` body, distinct from
        // `Config`) has no `#[serde(default = ...)]`: an incompatible field
        // type (here `port` as a string, not a number) makes
        // `serde_json::from_value` fail, just like a missing field (see the
        // next test).
        let mut f = fixture();
        let err =
            f.admin.set_data(serde_json::json!({ "listen": "0.0.0.0", "port": "lots" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "unexpected message: {err}");
    }

    #[tokio::test]
    async fn a_missing_field_is_rejected_rather_than_completed_by_a_default() {
        // The regression found in review: deserializing directly into
        // `Config` (which carries `#[serde(default = ...)]` on both fields,
        // correct for *loading a partial file*) would have let a `PUT
        // {"port": 6601}` without `listen` succeed silently, persisting
        // `listen = "0.0.0.0"` as if the operator had asked for it — a reset
        // of the listen address to zero disguised as a successful save. With
        // `ConfigWrite` (mandatory fields), this body must be rejected as
        // malformed, and nothing must change on disk or in memory.
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "listen": "192.168.1.10", "port": 6601 })).await.is_ok());
        let err = f.admin.set_data(serde_json::json!({ "port": 6601 })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "unexpected message: {err}");
        assert_eq!(f.admin.get_data().await["listen"], "192.168.1.10", "listen must not have moved");
    }
}
