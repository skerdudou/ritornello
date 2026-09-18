use crate::config::{Station, Stations};
use crate::directory::{Directory, DirectoryCountry, DirectoryStation};
use ritornello_plugin_sdk::AdminPlugin;
use ritornello_proto::Text;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

/// Operations carried by `SetData`, discriminated by the `op` field (model of
/// the generic-input plugin): the admin protocol is **not** extended, everything
/// goes through `GetAsset` / `GetData` / `SetData`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// Saves the whole table. The only operation that writes to disk. Presets
    /// are assigned by position on the browser side, but `Stations::validate`
    /// remains the authority.
    Save {
        #[serde(default)]
        stations: Vec<Station>,
    },
    /// Queries the online directory and memorizes the results. No station is
    /// persisted: the user then adds the ones of interest and clicks "Save".
    /// The **country**, however, is retained (see `PluginState::country`): it
    /// is a device preference, and finding it again on reload avoids typing it
    /// in every time.
    Search {
        query: String,
        /// ISO country code; empty string = "all countries".
        #[serde(default)]
        country: String,
    },
    /// Fetches the directory's country list and memorizes it.
    ///
    /// A distinct operation, and **on demand**: it costs a network call that
    /// nothing justifies as long as the user does not open the country
    /// selector. Memorizing it avoids asking again at every opening.
    Countries,
}

pub struct RadioAdmin {
    pub stations_path: PathBuf,
    /// Persisted plugin state, shared with the Source half: this is where the
    /// chosen country is retained, next to the preset.
    pub state_path: PathBuf,
    pub stations: Arc<AsyncRwLock<Stations>>,
    /// Access to the directory behind a trait: tests inject results without
    /// ever touching the network.
    pub directory: Arc<dyn Directory>,
    /// Last search results, exposed by `get_data` (field `search`); empty list
    /// as long as no search has been made. A failed search leaves them intact.
    pub search: RwLock<Vec<DirectoryStation>>,
    /// Country list, once fetched. Empty as long as the user has not opened the
    /// selector: no network call is made without that.
    pub countries: RwLock<Vec<DirectoryCountry>>,
    /// Announces the new `Stations::preset_count()` to the Source half after a
    /// successful save: this is what lets the web remote's grid update without
    /// waiting for a preset to be played. See `RadioSource::poll_notification`
    /// on the Source side.
    pub preset_count_tx: tokio::sync::watch::Sender<u8>,
}

impl RadioAdmin {
    /// A bare key, no parameters — unresolved, the core resolving it
    /// against this plugin's announced catalog (language-packs chantier,
    /// task 10).
    fn text(&self, key: &str) -> Text {
        Text::Keyed { key: key.to_string(), params: HashMap::new() }
    }

    /// A key with a single named parameter.
    fn text_with(&self, key: &str, param: &str, value: &str) -> Text {
        Text::Keyed { key: key.to_string(), params: HashMap::from([(param.to_string(), value.to_string())]) }
    }
}

#[async_trait::async_trait]
impl AdminPlugin for RadioAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => Some((
                "text/javascript".to_string(),
                include_str!("../ui/dist/ui.js").to_string(),
            )),
            "ui.css" => Some((
                "text/css".to_string(),
                include_str!("../ui/dist/ui.css").to_string(),
            )),
            _ => None,
        }
    }

    async fn get_data(&self) -> serde_json::Value {
        let stations = self.stations.read().await.stations.clone();
        // `std::sync` guards taken after the function's only `.await`: no guard
        // crosses an await point.
        let search = self.search.read().unwrap().clone();
        let countries = self.countries.read().unwrap().clone();
        // The country is re-read from disk on every call rather than kept in
        // memory: the Source half writes to the same file, and an in-memory
        // copy would diverge without anyone noticing.
        let country = crate::state::load(&self.state_path).country;
        serde_json::json!({
            "stations": stations,
            "search": search,
            "countries": countries,
            "country": country,
        })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), Text> {
        let op: Op = serde_json::from_value(data)
            .map_err(|e| self.text_with("bad_request", "detail", &e.to_string()))?;
        match op {
            Op::Save { stations } => {
                let stations = Stations { stations };
                stations.validate().map_err(|e| e.text())?;
                stations.save(&self.stations_path).map_err(|e| {
                    // The technical detail (path, I/O cause) stays in the log:
                    // a read-only `/var/lib` must remain diagnosable, but not
                    // at the price of serving that diagnosis as UI text.
                    tracing::warn!("failed to save stations: {e}");
                    self.text("save_failed")
                })?;
                let count = stations.preset_count();
                *self.stations.write().await = stations;
                // Spontaneous announcement to the Source half, on **every**
                // successful save — even if the count does not change:
                // comparing before sending would cost a guard for no gain, the
                // core-side merge (`Core::handle_source_update`) and its
                // broadcast (`publish_state`) already deduplicating any frame
                // identical to the previous one. No receiver in degraded mode
                // (no admin): `send` then returns an inconsequential error,
                // ignored.
                let _ = self.preset_count_tx.send(count);
                Ok(())
            }
            Op::Search { query, country } => {
                let country = country.trim().to_string();
                let country = if country.is_empty() { None } else { Some(country) };
                // The network call is awaited here (no polling, unlike the
                // input plugin's learning); it only concerns the Admin half,
                // audio playback is never blocked. It is also the point that
                // must yield before the 5 s `AdminClient::request` grants the
                // core: the `search_with_fallback` budget (4 s) is there for
                // that.
                let results = self
                    .directory
                    .search(query.trim(), country.as_deref())
                    .await
                    .map_err(|detail| self.text_with("search_error", "detail", &detail))?;
                *self.search.write().unwrap() = results;
                // The country is only retained after a **successful** search: a
                // failed search says nothing about the user's intent, and
                // memorizing a country that just failed would make it retry on
                // reload.
                let chosen = country.unwrap_or_default();
                if let Err(e) = crate::state::update(&self.state_path, |s| s.country = chosen) {
                    // No consequence for the search that just succeeded: only
                    // the memory of the choice is lost.
                    tracing::warn!("country not saved: {e}");
                }
                Ok(())
            }
            Op::Countries => {
                let countries = self
                    .directory
                    .countries()
                    .await
                    .map_err(|detail| self.text_with("search_error", "detail", &detail))?;
                *self.countries.write().unwrap() = countries;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Station;
    use crate::directory::parse_search_results;

    const FIXTURE: &str = include_str!("../tests/fixtures/radio-browser-search.json");

    /// Test directory: returns a fixed result (or an error) and records the
    /// arguments received. No socket, no network.
    struct StubDirectory {
        result: Result<Vec<DirectoryStation>, String>,
        countries: Result<Vec<DirectoryCountry>, String>,
        seen: std::sync::Mutex<Vec<(String, Option<String>)>>,
        country_calls: std::sync::atomic::AtomicUsize,
    }

    impl StubDirectory {
        fn ok(stations: Vec<DirectoryStation>) -> Arc<Self> {
            Arc::new(StubDirectory {
                result: Ok(stations),
                countries: Ok(vec![
                    DirectoryCountry { code: "FR".into(), stations: 2746 },
                    DirectoryCountry { code: "BE".into(), stations: 300 },
                ]),
                seen: std::sync::Mutex::new(Vec::new()),
                country_calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }
        fn err(msg: &str) -> Arc<Self> {
            Arc::new(StubDirectory {
                result: Err(msg.to_string()),
                countries: Err(msg.to_string()),
                seen: std::sync::Mutex::new(Vec::new()),
                country_calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }
    }

    #[async_trait::async_trait]
    impl Directory for StubDirectory {
        async fn search(
            &self,
            query: &str,
            country: Option<&str>,
        ) -> Result<Vec<DirectoryStation>, String> {
            self.seen
                .lock()
                .unwrap()
                .push((query.to_string(), country.map(str::to_string)));
            self.result.clone()
        }

        async fn countries(&self) -> Result<Vec<DirectoryCountry>, String> {
            self.country_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.countries.clone()
        }
    }

    fn admin_with(dir: &std::path::Path, directory: Arc<dyn Directory>) -> RadioAdmin {
        let path = dir.join("stations.toml");
        let stations = Stations {
            stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }],
        };
        stations.save(&path).unwrap();
        RadioAdmin {
            stations_path: path,
            state_path: dir.join("plugin-radio.json"),
            stations: Arc::new(AsyncRwLock::new(stations)),
            directory,
            search: RwLock::new(Vec::new()),
            countries: RwLock::new(Vec::new()),
            // No observer: tests that do not care about the preset count have
            // nothing to wire. See `admin_with_channel` for those that observe
            // it.
            preset_count_tx: tokio::sync::watch::channel(0).0,
        }
    }

    fn admin(dir: &std::path::Path) -> RadioAdmin {
        admin_with(dir, StubDirectory::ok(Vec::new()))
    }

    /// Like `admin_with`, but also exposes the receiver of the preset count
    /// channel, for tests that check what is published on it.
    fn admin_with_channel(
        dir: &std::path::Path,
        directory: Arc<dyn Directory>,
    ) -> (RadioAdmin, tokio::sync::watch::Receiver<u8>) {
        let mut a = admin_with(dir, directory);
        let (tx, rx) = tokio::sync::watch::channel(0);
        a.preset_count_tx = tx;
        (a, rx)
    }

    #[test]
    fn asset_exposes_ui_js_and_ui_css_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let (mime, body) = a.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!body.is_empty());
        assert_eq!(a.asset("ui.css").unwrap().0, "text/css");
        // An unknown path is not an error: it is a 404 on the core side.
        assert!(a.asset("../../../etc/passwd").is_none());
        assert!(a.asset("index.html").is_none());
    }

    #[tokio::test]
    async fn get_data_returns_the_stations_and_an_empty_search() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let v = a.get_data().await;
        assert_eq!(v["stations"][0]["name"], "FIP");
        assert_eq!(v["search"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn valid_save_persists_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let new = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "Inter", "url": "http://inter", "preset": 1 }]
        });
        assert!(a.set_data(new).await.is_ok());
        assert_eq!(a.stations.read().await.stations[0].name, "Inter");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "Inter");
    }

    #[tokio::test]
    async fn a_write_failure_returns_the_save_failed_key_not_the_io_detail() {
        // `stations_path` targets an ordinary file as if it were a parent
        // directory: `create_dir_all` fails with an I/O error, without ever
        // touching the real stations on disk.
        // Targeted regression: `Stations::save(...).map_err(|e| e.to_string())`
        // put that raw error (paths included) in the response body — the text
        // meant for the player must remain a resolvable key, the technical
        // detail going to the log. The plugin no longer resolves it itself
        // (no `Catalog` left — language-packs chantier, task 10).
        let dir = tempfile::tempdir().unwrap();
        let obstacle = dir.path().join("obstacle");
        std::fs::write(&obstacle, b"not a directory").unwrap();
        let mut a = admin(dir.path());
        a.stations_path = obstacle.join("stations.toml");
        let new = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "Inter", "url": "http://inter", "preset": 1 }]
        });
        let err = a.set_data(new).await.unwrap_err();
        assert_eq!(err, Text::Keyed { key: "save_failed".into(), params: HashMap::new() });
    }

    #[tokio::test]
    async fn a_successful_save_publishes_the_new_count() {
        // This is what lets the web remote's grid update as soon as the save
        // happens, without waiting for a preset to be played — see the defect
        // observed in use.
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut rx) = admin_with_channel(dir.path(), StubDirectory::ok(Vec::new()));
        let new = serde_json::json!({
            "op": "save",
            "stations": [
                { "name": "A", "url": "http://a", "preset": 1 },
                { "name": "B", "url": "http://b", "preset": 2 }
            ]
        });
        assert!(a.set_data(new).await.is_ok());
        assert!(rx.has_changed().unwrap(), "the new count must be published");
        assert_eq!(*rx.borrow_and_update(), 2);
    }

    #[tokio::test]
    async fn a_rejected_save_publishes_nothing() {
        // A payload that does not pass `Stations::validate` must announce
        // nothing: nothing changed on the table side.
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut rx) = admin_with_channel(dir.path(), StubDirectory::ok(Vec::new()));
        rx.borrow_and_update();
        let bad = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "X", "url": "http://x", "preset": 200 }]
        });
        assert!(a.set_data(bad).await.is_err());
        assert!(!rx.has_changed().unwrap(), "a rejected save must publish nothing");
    }

    #[tokio::test]
    async fn save_numbers_from_1_to_n_by_position() {
        // Payload as produced by the UI: `preset` = position.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let new = serde_json::json!({
            "op": "save",
            "stations": [
                { "name": "A", "url": "http://a", "preset": 1 },
                { "name": "B", "url": "http://b", "preset": 2 },
                { "name": "C", "url": "http://c", "preset": 3 }
            ]
        });
        assert!(a.set_data(new).await.is_ok());
        let s = Stations::load(&a.stations_path).unwrap();
        assert_eq!(s.by_preset(2).unwrap().name, "B");
        assert_eq!(s.by_preset(3).unwrap().name, "C");
    }

    #[tokio::test]
    async fn invalid_save_returns_an_error_and_does_not_persist() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let bad = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "X", "url": "http://x", "preset": 200 }]
        });
        assert!(a.set_data(bad).await.is_err());
        // the shared state and the disk remain unchanged
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn saving_an_out_of_range_preset_is_rejected_server_side() {
        // Server-side net: `Stations::validate` remains the authority even for
        // a payload that does not go through the admin page — the bound is now
        // 1..=99 (before: 1..=9, hence the former name of this test).
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let stations = vec![serde_json::json!({ "name": "S100", "url": "http://x", "preset": 100 })];
        let err = a
            .set_data(serde_json::json!({ "op": "save", "stations": stations }))
            .await
            .unwrap_err();
        match err {
            Text::Keyed { key, params } => {
                assert_eq!(key, "preset_out_of_range");
                assert_eq!(params.get("p").map(String::as_str), Some("100"), "{params:?}");
            }
            Text::Verbatim(s) => panic!("expected a keyed text, got verbatim: {s}"),
        }
        assert!(!Stations::load(&a.stations_path).unwrap().stations.is_empty());
    }

    #[tokio::test]
    async fn search_memorizes_the_results_and_get_data_exposes_them() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(parse_search_results(FIXTURE).unwrap());
        let mut a = admin_with(dir.path(), stub.clone());
        let op = serde_json::json!({ "op": "search", "query": "france", "country": "FR" });
        assert!(a.set_data(op).await.is_ok());

        let v = a.get_data().await;
        assert_eq!(v["search"].as_array().unwrap().len(), 4);
        assert_eq!(v["search"][0]["name"], "France Info");
        assert_eq!(v["search"][0]["url"], "http://direct.franceinfo.fr/live/franceinfo-midfi.mp3");
        assert_eq!(v["search"][0]["codec"], "MP3");
        assert_eq!(v["search"][0]["bitrate"], 128);
        assert_eq!(v["search"][0]["country"], "FR");
        // the configured stations do not move
        assert_eq!(v["stations"][0]["name"], "FIP");
        // nothing is persisted by a search
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
        assert_eq!(stub.seen.lock().unwrap()[0], ("france".to_string(), Some("FR".to_string())));
    }

    #[tokio::test]
    async fn search_without_country_passes_no_countrycode() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(Vec::new());
        let mut a = admin_with(dir.path(), stub.clone());
        let op = serde_json::json!({ "op": "search", "query": "  jazz  ", "country": "" });
        assert!(a.set_data(op).await.is_ok());
        assert_eq!(stub.seen.lock().unwrap()[0], ("jazz".to_string(), None));
        assert_eq!(a.get_data().await["search"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn failed_search_returns_a_translated_message_and_leaves_the_state_intact() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(parse_search_results(FIXTURE).unwrap());
        let mut a = admin_with(dir.path(), stub);
        assert!(a
            .set_data(serde_json::json!({ "op": "search", "query": "france", "country": "FR" }))
            .await
            .is_ok());

        // the directory goes down: the previous results remain displayable
        a.directory = StubDirectory::err("timeout");
        let err = a
            .set_data(serde_json::json!({ "op": "search", "query": "france", "country": "FR" }))
            .await
            .unwrap_err();
        assert_eq!(err, Text::Keyed { key: "search_error".into(), params: HashMap::from([("detail".to_string(), "timeout".to_string())]) });
        assert_eq!(a.get_data().await["search"].as_array().unwrap().len(), 4);
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn countries_are_only_fetched_on_demand_and_memorized() {
        // The network call must not leave when the page loads: it is only
        // justified when the user opens the country selector.
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(Vec::new());
        let mut a = admin_with(dir.path(), stub.clone());
        assert_eq!(a.get_data().await["countries"], serde_json::json!([]));
        assert_eq!(stub.country_calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        assert!(a.set_data(serde_json::json!({ "op": "countries" })).await.is_ok());
        let v = a.get_data().await;
        assert_eq!(v["countries"][0]["code"], "FR");
        assert_eq!(v["countries"][0]["stations"], 2746);
        assert_eq!(stub.country_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_countries_return_a_translated_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_with(dir.path(), StubDirectory::err("timeout"));
        let err = a.set_data(serde_json::json!({ "op": "countries" })).await.unwrap_err();
        assert_eq!(err, Text::Keyed { key: "search_error".into(), params: HashMap::from([("detail".to_string(), "timeout".to_string())]) });
        assert_eq!(a.get_data().await["countries"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn a_successful_search_memorizes_the_country_and_get_data_returns_it() {
        // This is what avoids typing the country in again every time the page
        // opens.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_with(dir.path(), StubDirectory::ok(Vec::new()));
        assert_eq!(a.get_data().await["country"], "");
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "BE" });
        assert!(a.set_data(op).await.is_ok());
        assert_eq!(a.get_data().await["country"], "BE");
        // "all countries" is a choice like any other, and must be retained too.
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "" });
        assert!(a.set_data(op).await.is_ok());
        assert_eq!(a.get_data().await["country"], "");
    }

    #[tokio::test]
    async fn memorizing_the_country_does_not_lose_the_preset() {
        // Both halves of the plugin write to the same state file.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_with(dir.path(), StubDirectory::ok(Vec::new()));
        crate::state::update(&a.state_path, |s| s.preset = 6).unwrap();
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "DE" });
        assert!(a.set_data(op).await.is_ok());
        let state = crate::state::load(&a.state_path);
        assert_eq!(state.country, "DE");
        assert_eq!(state.preset, 6, "the preset must not be overwritten");
    }

    #[tokio::test]
    async fn a_failed_search_does_not_memorize_the_country() {
        // Retaining a country that just failed would make the reload retry
        // what is already known not to work.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin_with(dir.path(), StubDirectory::err("timeout"));
        let op = serde_json::json!({ "op": "search", "query": "rock", "country": "IT" });
        assert!(a.set_data(op).await.is_err());
        assert_eq!(crate::state::load(&a.state_path).country, "");
    }

    #[tokio::test]
    async fn unknown_or_missing_op_returns_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let err = a.set_data(serde_json::json!({ "op": "destroy" })).await.unwrap_err();
        assert!(matches!(&err, Text::Keyed { key, .. } if key == "bad_request"), "unexpected: {err:?}");
        let err2 = a
            .set_data(serde_json::json!({ "stations": [] }))
            .await
            .unwrap_err();
        assert!(matches!(&err2, Text::Keyed { key, .. } if key == "bad_request"), "unexpected: {err2:?}");
    }

    /// **Generalized (task 15).** Was "en vs fr, key sets only" — a
    /// hardcoded language that stops covering a second one the moment it
    /// ships, and a comparison blind to a translation that renamed or
    /// dropped a `{named}` parameter. `shipped_language_packs` derives the
    /// language list from the tree; the `assert!(!shipped.is_empty(), ...)`
    /// below is what keeps that derivation honest instead of vacuously
    /// green on a broken discovery.
    #[test]
    fn key_and_param_parity_between_the_embedded_en_and_every_shipped_language() {
        let en = ritornello_i18n::try_parse(crate::RADIO_EN).unwrap();
        let deploy_locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = ritornello_i18n::shipped_language_packs(&deploy_locales, "radio");
        assert!(!shipped.is_empty(), "no shipped language found for radio under deploy/locales");
        for (lang, content) in shipped {
            let pack = ritornello_i18n::try_parse(&content)
                .unwrap_or_else(|e| panic!("{lang} pack for radio is invalid TOML: {e}"));
            let mut en_keys: Vec<&String> = en.keys().collect();
            let mut pack_keys: Vec<&String> = pack.keys().collect();
            en_keys.sort();
            pack_keys.sort();
            assert_eq!(en_keys, pack_keys, "en/{lang} key sets diverge for radio");

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
