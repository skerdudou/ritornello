//! Admin backend of the musicbrainz plugin: serves the Vue page shipped in
//! `ui/dist`, and applies the three actions it can emit to the store of ICY
//! patterns (see `patterns.rs`) — the same store the `metadata` loop reads and
//! writes to probe and split radio streams.
//!
//! A `metadata` plugin **never** receives a `SetLocale` frame: that frame only
//! exists for `SourcePlugin` (see `ritornello_proto`). The catalog loaded here
//! is therefore frozen to the language passed at the plugin's launch — a
//! change of the device's language only shows on this page after a restart of
//! the plugin. Same limit as the MPD plugin's page
//! (`ritornello-plugin-mpd::admin`).

use crate::patterns::{Store, Pattern};
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as StoreLock;

/// What the page sends. Dedicated structure and **mandatory** fields, like the
/// MPD plugin's `ConfigWrite`: `Entry` (the persisted type) has
/// `#[serde(default)]` to reread a file from an earlier version, and reusing
/// them here would make a field forgotten by the page pass for a deliberate
/// choice rather than for a malformed request to reject.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Write {
    /// Set a pattern by hand on a station. Always `Origin::Manual`: it is
    /// `set_manual` that carries this rule, the page does not have to emit it.
    Set { url: String, pattern: WrittenPattern },
    Remove { url: String },
    Clear,
}

/// Same external shape as `patterns::Pattern` (externally tagged: the object
/// `{"split": {...}}` or the bare string `"do_not_split"`), but a separate
/// type: this one is the page's write contract, the other is the store's
/// persisted format. The two evolve for different reasons.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WrittenPattern {
    Split {
        separator: String,
        artist_first: bool,
        /// `serde(default)` while the other fields are mandatory, and it is
        /// deliberate: a page that does not know this form keeps writing, and
        /// absence means "no" — the common form.
        #[serde(default)]
        title_in_middle: bool,
    },
    DoNotSplit,
}

impl From<WrittenPattern> for Pattern {
    fn from(m: WrittenPattern) -> Self {
        match m {
            WrittenPattern::Split { separator, artist_first, title_in_middle } => {
                // `title_in_middle` is **carried over** and not reset to false.
                //
                // The page does not *offer* it in its closed set — this form is
                // only obtained through a probe — but it **replays** it when the
                // form was opened on an entry that carries it. Resetting it to
                // false here meant that "Save" without changing anything
                // degraded the pattern: the album got glued back onto the title
                // from the next track, and since the entry became `Manual`,
                // nothing could repair it anymore. The destructive gesture was
                // not "set this form", it was "save without modification".
                Pattern::Split { separator, artist_first, title_in_middle }
            }
            WrittenPattern::DoNotSplit => Pattern::DoNotSplit,
        }
    }
}

pub struct MusicBrainzAdmin {
    store: Arc<StoreLock<Store>>,
    state_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    /// Root of the on-disk language packs, kept so a catalog can be rebuilt in
    /// any requested language — `Catalog::load` only parses a TOML file, so
    /// this costs nothing per request.
    locales_root: PathBuf,
}

impl MusicBrainzAdmin {
    pub fn new(
        store: Arc<StoreLock<Store>>,
        state_path: PathBuf,
        catalog: Arc<RwLock<Catalog>>,
        locales_root: PathBuf,
    ) -> Self {
        Self { store, state_path, catalog, locales_root }
    }

    /// Resolves a catalog key into the sentence of the current language.
    fn translate(&self, key: &str) -> String {
        self.catalog.read().unwrap().get(key).to_string()
    }
}

#[async_trait::async_trait]
impl AdminPlugin for MusicBrainzAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => {
                Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string()))
            }
            "ui.css" => Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string())),
            _ => None,
        }
    }

    fn catalog(&self, lang: Option<&str>) -> serde_json::Value {
        match lang {
            // The language the plugin was started in: the catalog already
            // built, no work at all.
            None => serde_json::json!(self.catalog.read().unwrap().entries()),
            // A language explicitly asked for. Rebuilt rather than translated
            // from the current one: the on-disk pack is the authority, and
            // only `Catalog::load` knows how to layer it over the embedded
            // English.
            Some(l) => {
                let c = Catalog::load("musicbrainz", l, &self.locales_root, crate::MUSICBRAINZ_EN);
                serde_json::json!(c.entries())
            }
        }
    }

    async fn get_data(&self) -> serde_json::Value {
        let store = self.store.read().await;
        // Sorted copy: the store keeps insertion order, only the page needs an
        // order. `Option<String>` orders `None` before any `Some`; comparing
        // `b` to `a` (rather than `a` to `b`) therefore gives the most recent
        // first and the never-served stations last, without going through a
        // two-stage sort.
        let mut stations = store.entries().to_vec();
        stations.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        // **The threshold travels with the data.** The page has to say whether
        // a "do not split" is still provisional, which takes comparing
        // `failed_probes` to the anchoring threshold — and that threshold is a
        // decision of the probing logic, not of the page. Sending it spares a
        // second copy of the number, which would be free to drift.
        serde_json::json!({
            "stations": stations,
            "probes_before_anchoring": crate::PROBES_BEFORE_ANCHORING,
        })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        // `Write`, not a type modeled on `Entry`: see the comment on the type.
        // A missing field or an unknown action must reject the request, not
        // get completed by a *loading* default.
        let write: Write =
            serde_json::from_value(data).map_err(|e| {
                // Through the catalog, like every other refusal of this method:
                // a hard-coded English string is not "a translated sentence",
                // it is just a key in disguise — a user in French would see
                // English. The plugin's catalog did not have this key (an
                // omission of my brief), it was added on the exact model of the
                // mpd plugin's.
                self.catalog.read().unwrap().get("bad_request").replace("{detail}", &e.to_string())
            })?;

        let mut store = self.store.write().await;
        match write {
            Write::Set { url, pattern } => {
                // Validation **before** any write: an empty separator, or one
                // without a space on each side, would cut a hyphenated name in
                // two ("Jean-Michel Jarre"). The page already validates for
                // immediate feedback, but the backend remains the authority.
                if let WrittenPattern::Split { separator, .. } = &pattern {
                    // `trim()` and not `is_empty()`: a separator made only of
                    // spaces passed both checks — `" "` starts *and* ends with a
                    // space, the same one — and would have split on **every**
                    // space of the announced string. "Empty" is the right word
                    // for it: it carries nothing.
                    if separator.trim().is_empty() {
                        return Err(self.translate("separator_empty"));
                    }
                    if !(separator.starts_with(' ') && separator.ends_with(' ')) {
                        return Err(self.translate("separator_no_space"));
                    }
                }
                store.set_manual(&url, pattern.into());
            }
            Write::Remove { url } => {
                // A refusal, not a silent success: the page would show "done"
                // on a gesture with no effect.
                if store.entry(&url).is_none() {
                    return Err(self.translate("unknown_station"));
                }
                store.remove(&url);
            }
            Write::Clear => {
                // Nothing to erase: a "clear all" on an already empty store
                // must not trigger a disk write for nothing — and hence not
                // risk a `save_failed` refusal on a gesture that would change
                // nothing anyway.
                if store.is_empty() {
                    return Ok(());
                }
                store.clear_all();
            }
        }
        // No method of the store writes to disk on its own: this is deliberate
        // (see `patterns.rs`), so that a write does not hide behind a name that
        // does not mention it. So it is here, and only here, that the mutation
        // becomes persistent.
        store.save(&self.state_path).map_err(|e| {
            tracing::warn!("could not save ICY patterns: {e}");
            self.translate("save_failed")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::Origin;

    struct Fixture {
        admin: MusicBrainzAdmin,
        state_path: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("patterns.json");
        let catalog = Arc::new(RwLock::new(Catalog::load(
            "musicbrainz",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::MUSICBRAINZ_EN,
        )));
        Fixture {
            admin: MusicBrainzAdmin::new(
                Arc::new(StoreLock::new(Store::default())),
                state_path.clone(),
                catalog,
                std::path::PathBuf::from("/nonexistent"),
            ),
            state_path,
            _dir: dir,
        }
    }

    #[test]
    fn unknown_assets_are_not_served() {
        let f = fixture();
        let (mime, body) = f.admin.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!body.is_empty());
        assert_eq!(f.admin.asset("ui.css").unwrap().0, "text/css");
        // An unknown path is not an error: it is a 404 on the core's side.
        // Serving anything else would open an arbitrary read route.
        assert!(f.admin.asset("../../../etc/passwd").is_none());
        assert!(f.admin.asset("index.html").is_none());
    }

    #[test]
    fn catalog_exposes_the_components_keys() {
        let f = fixture();
        let v = f.admin.catalog(None);
        assert!(v["title"].is_string(), "the catalog must carry the plugin's keys");
    }

    #[test]
    fn a_requested_language_is_honoured_whatever_the_current_one() {
        // The plugin is started in English; asking for another language must
        // rebuild, not return the current catalog. This is the whole basis of
        // the `immutable` answer served over HTTP.
        let mut f = fixture();
        let locales = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(locales.path().join("musicbrainz")).unwrap();
        std::fs::write(locales.path().join("musicbrainz/fr.toml"), "title = \"Empreintes\"\n").unwrap();
        f.admin.locales_root = locales.path().to_path_buf();
        let en = f.admin.catalog(None);
        let fr = f.admin.catalog(Some("fr"));
        assert_ne!(en, fr);
        assert_eq!(fr["title"], "Empreintes");
    }

    /// French pack shipped in the repository.
    fn fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/musicbrainz/fr.toml");
        std::fs::read_to_string(p).expect("shipped fr pack")
    }

    #[test]
    fn key_parity_between_the_embedded_en_and_the_fr_pack() {
        let en = ritornello_i18n::try_parse(crate::MUSICBRAINZ_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let mut en_keys: Vec<&String> = en.keys().collect();
        let mut fr_keys: Vec<&String> = fr.keys().collect();
        en_keys.sort();
        fr_keys.sort();
        assert_eq!(en_keys, fr_keys, "en/fr key sets diverge");
    }

    #[tokio::test]
    async fn setting_a_pattern_makes_it_manual_and_persists_it() {
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "set",
            "url": "http://example/stream.mp3",
            "pattern": { "split": { "separator": " - ", "artist_first": true } }
        });
        assert!(f.admin.set_data(op).await.is_ok());

        let data = f.admin.get_data().await;
        assert_eq!(data["stations"][0]["url"], "http://example/stream.mp3");
        assert_eq!(data["stations"][0]["origin"], "manual");
        // `title_in_middle` appears in the serialized form: the field is
        // additive (`serde(default)`), so the page that ignores it keeps
        // reading, and the one that only sends the other two keeps writing.
        // Pinned here because it is the contract the page consumes.
        assert_eq!(
            data["stations"][0]["pattern"],
            serde_json::json!({
                "split": {
                    "separator": " - ",
                    "artist_first": true,
                    "title_in_middle": false
                }
            })
        );

        // Persisted to disk, not only in memory: `save` was called after the
        // mutation, as the contract requires.
        let reread = Store::load(&f.state_path);
        assert_eq!(reread.entry("http://example/stream.mp3").unwrap().origin, Origin::Manual);
    }

    /// A separator made **only** of spaces is refused as empty.
    ///
    /// `" "` passed the two original checks — it starts and ends with a space,
    /// the same one — and would have split on every space of the announced
    /// string: "Miles Davis - So What" became artist "Miles". Finding of the
    /// cross review.
    #[tokio::test]
    async fn a_separator_made_only_of_spaces_is_refused() {
        let mut f = fixture();
        for sep in [" ", "  ", "\t"] {
            let op = serde_json::json!({
                "action": "set",
                "url": "http://example/stream.mp3",
                "pattern": { "split": { "separator": sep, "artist_first": true } }
            });
            let err = f.admin.set_data(op).await.expect_err("an empty separator must be refused");
            assert!(!err.contains("separator_"), "never the raw key: {err}");
        }
    }

    #[tokio::test]
    async fn a_separator_without_spaces_is_refused_by_a_sentence_not_a_key() {
        // The SDK's contract, and the real rule: without surrounding spaces,
        // `Jean-Michel Jarre` would get cut in two.
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "set",
            "url": "http://f",
            "pattern": { "split": { "separator": "-", "artist_first": true } }
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("space"), "must be the catalog's sentence: {err}");
        assert!(!err.contains("separator_no_space"), "never the raw key: {err}");
        // Nothing should have been set.
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_empty_separator_is_refused_by_a_distinct_sentence() {
        let mut f = fixture();
        let op = serde_json::json!({
            "action": "set",
            "url": "http://f",
            "pattern": { "split": { "separator": "", "artist_first": true } }
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "the separator cannot be empty");
        assert_ne!(err, "separator_empty");
    }

    #[tokio::test]
    async fn removing_an_unknown_station_is_a_refusal_and_not_a_silent_success() {
        let mut f = fixture();
        let op = serde_json::json!({ "action": "remove", "url": "http://unknown" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "no entry for that stream");
        assert_ne!(err, "unknown_station");
    }

    #[tokio::test]
    async fn removing_a_known_station_succeeds_and_persists() {
        let mut f = fixture();
        f.admin
            .set_data(serde_json::json!({
                "action": "set", "url": "http://f", "pattern": "do_not_split"
            }))
            .await
            .unwrap();
        assert!(f.admin.set_data(serde_json::json!({ "action": "remove", "url": "http://f" })).await.is_ok());
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
        assert!(Store::load(&f.state_path).entry("http://f").is_none());
    }

    #[tokio::test]
    async fn clear_erases_all_the_stations_and_persists() {
        let mut f = fixture();
        f.admin
            .set_data(serde_json::json!({ "action": "set", "url": "http://f", "pattern": "do_not_split" }))
            .await
            .unwrap();
        assert!(f.admin.set_data(serde_json::json!({ "action": "clear" })).await.is_ok());
        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty());
        assert!(Store::load(&f.state_path).is_empty());
    }

    #[tokio::test]
    async fn clearing_an_already_empty_store_does_not_write_to_disk() {
        // The shortcut of `Write::Clear`: nothing to erase, hence nothing to
        // write. Proven by the absence of the state file, which `save` would
        // have created.
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "action": "clear" })).await.is_ok());
        assert!(!f.state_path.exists(), "no write should have happened on an already empty store");
    }

    #[tokio::test]
    async fn get_data_carries_the_anchoring_threshold() {
        // Without it the page would hold its own copy of the number, free to
        // drift from the one that actually decides.
        let f = fixture();
        let data = f.admin.get_data().await;
        assert_eq!(data["probes_before_anchoring"], crate::PROBES_BEFORE_ANCHORING);
    }

    #[tokio::test]
    async fn get_data_sorts_by_last_used_descending() {
        // Writes the state file directly rather than spacing calls to
        // `record_success()` in real time (which would produce timestamps in
        // the same second, hence an unobservable order): this is exactly the
        // form `Store::save` produces itself (see the round-trip test of
        // `patterns.rs`), not an invented JSON.
        let f = fixture();
        let raw = serde_json::json!({
            "stations": [
                { "url": "http://b", "pattern": "do_not_split", "origin": "learned_deviation",
                  "last_used": "2024-01-01T00:00:00Z", "split_titles": 5 },
                { "url": "http://a", "pattern": { "split": { "separator": " - ", "artist_first": true } },
                  "origin": "standard_confirmed", "last_used": "2026-01-01T00:00:00Z", "split_titles": 10 },
                { "url": "http://c", "pattern": "do_not_split", "origin": "manual",
                  "last_used": null, "split_titles": 0 }
            ]
        });
        std::fs::write(&f.state_path, serde_json::to_string(&raw).unwrap()).unwrap();
        let store = Store::load(&f.state_path);
        let admin = MusicBrainzAdmin::new(
            Arc::new(StoreLock::new(store)),
            f.state_path.clone(),
            Arc::new(RwLock::new(Catalog::load(
                "musicbrainz",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::MUSICBRAINZ_EN,
            ))),
            std::path::PathBuf::from("/nonexistent"),
        );

        let data = admin.get_data().await;
        let urls: Vec<&str> = data["stations"].as_array().unwrap().iter().map(|s| s["url"].as_str().unwrap()).collect();
        assert_eq!(urls, vec!["http://a", "http://b", "http://c"], "most recent first, never probed last");
    }

    #[tokio::test]
    async fn a_malformed_write_is_rejected() {
        // Missing field, unknown action: refusal, not a default applied.
        let mut f = fixture();
        let err = f.admin.set_data(serde_json::json!({ "action": "set", "pattern": "do_not_split" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "unexpected message: {err}");

        let err = f.admin.set_data(serde_json::json!({ "action": "wipe_everything" })).await.unwrap_err();
        assert!(err.starts_with("Unexpected request:"), "unexpected message: {err}");

        assert!(f.admin.get_data().await["stations"].as_array().unwrap().is_empty(), "nothing should have been applied");
    }

    #[tokio::test]
    async fn a_write_failure_returns_a_catalog_sentence_not_the_io_detail() {
        // Same regression as in mpd, generic-input and radio:
        // `save(...).map_err(|e| e.to_string())` would put the raw I/O detail
        // in the response body. `state_path` here points at an ordinary file
        // as if it were a parent directory, to make the write of the temporary
        // file fail without touching the real disk.
        let mut f = fixture();
        let obstacle = f.state_path.parent().unwrap().join("obstacle");
        std::fs::write(&obstacle, b"not a directory").unwrap();
        f.admin.state_path = obstacle.join("patterns.json");
        let op = serde_json::json!({ "action": "set", "url": "http://f", "pattern": "do_not_split" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert_eq!(err, "could not write the pattern file");
    }
}
