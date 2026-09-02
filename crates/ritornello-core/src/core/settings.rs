//! Persisted settings: audio output, language, theme, and the writing of state.json.

use super::*;

impl<P: Player> Core<P> {
    /// Applies an output choice from the config page. `None` means "follow
    /// the system default": mpv gets its native `auto` back (settable at
    /// runtime), and nothing is recorded on disk — the same state as a fresh
    /// install, where `resume()` sends no device at all.
    pub async fn set_audio_device(&mut self, device: Option<String>) -> Result<()> {
        match &device {
            Some(d) => self.player.set_audio_device(d).await?,
            None => self.player.set_audio_device("auto").await?,
        }
        self.audio_device = device;
        self.persist();
        Ok(())
    }

    /// Changes the current language: rebuilds the core's shared catalog
    /// (read by the status page), persists the state, and pushes `SetLocale`
    /// to every connected Source plugin (best-effort).
    ///
    /// Called from the `select!` loop of `main` on reception from the
    /// `locale_rx` channel, itself fed by the `PUT /api/locale` route.
    ///
    /// Also resolves `standby_status` in the brand-new catalog, and publishes
    /// the state: without the latter, changing language during standby left
    /// the word displayed in the old language until the next
    /// `Command::Power` cycle (see the doc of `standby_status`).
    pub async fn set_locale(&mut self, locale: String) -> Result<()> {
        self.locale = Some(locale.clone());
        let new_catalog = Catalog::load("core", &locale, &self.locales_root, crate::i18n::EN);
        self.standby_status = Some(resolve_standby_status(&new_catalog));
        *self.catalog.write().await = new_catalog;
        self.persist();
        for name in self.source_order.clone() {
            if let Some(src) = self.sources.get(&name) {
                if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                    tracing::warn!("SetLocale to {name}: {e}");
                }
            }
        }
        self.publish_state();
        Ok(())
    }

    /// Changes the current theme and persists it. Unlike `set_locale`,
    /// nothing is pushed to the plugins: the theme is an appearance setting
    /// of the web UI, of which no plugin is aware.
    ///
    /// Called from the `select!` loop of `main` on reception from the
    /// `theme_rx` channel, itself fed by the `PUT /api/theme` route.
    pub fn set_theme(&mut self, t: crate::theme::ThemeState) {
        self.theme = Some(t.theme);
        self.mode = Some(t.mode);
        self.persist();
    }

    pub(super) fn persist(&self) {
        let st = PersistedState {
            active_source: self.active_source.clone(),
            volume: self.volume,
            standby: self.standby,
            audio_device: self.audio_device.clone(),
            locale: self.locale.clone(),
            theme: self.theme.clone(),
            mode: self.mode.clone(),
            settings: self.settings.clone(),
        };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistence failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn resume_applies_the_persisted_audio_output() {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let persisted = PersistedState {
            active_source: "radio".into(),
            volume: 60,
            standby: false,
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            theme: None,
            mode: None,
            settings: crate::state::Settings::default(),
        };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, cover_tx) = test_covers();
        let mut core = Core::new(player, Wiring { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: silent_wiring(vec![]), sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device bluealsa:DEV=XX".to_string()));
    }

    #[tokio::test]
    async fn set_audio_device_applies_and_persists() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device(Some("hw:CARD=Headphones".into())).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device hw:CARD=Headphones".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device.as_deref(), Some("hw:CARD=Headphones"));
    }

    #[tokio::test]
    async fn set_audio_device_none_returns_to_the_system_default() {
        // "System default" from the config page: nothing imposed on mpv
        // anymore (its native `auto`), and no device recorded on disk.
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device(Some("hw:CARD=Headphones".into())).await.unwrap();
        core.set_audio_device(None).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device auto".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device, None);
    }

    #[tokio::test]
    async fn set_locale_persists_and_notifies_the_sources() {
        let (mut core, _pc, source_calls, _rx, dir) = setup();
        core.set_locale("fr".into()).await.unwrap();
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:SetLocale(\"fr\")"));
        assert!(calls.iter().any(|c| c == "cd:SetLocale(\"fr\")"));
        drop(calls);
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.locale.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn the_standby_word_is_translated_by_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (state_tx, mut state_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "fr", &root, crate::i18n::EN)));
        let metadata = MetadataWiring {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            state: state_tx,
        };
        let (covers, cover_tx) = test_covers();
        let mut core = Core::new(player, Wiring { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("VEILLE"));
    }

    #[tokio::test]
    async fn changing_language_in_standby_republishes_the_standby_word_at_once() {
        // Regression (M1+M9, branch review): the standby word was only
        // resolved when entering standby (`Command::Power`), and
        // `set_locale` published no state anyway. Changing language
        // *during* standby therefore left the word displayed in the old
        // language until the next Power cycle.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (state_tx, mut state_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        // Built in English: "STANDBY", the embedded value of the key.
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataWiring {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            state: state_tx,
        };
        let (covers, cover_tx) = test_covers();
        let mut core = Core::new(player, Wiring { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));
        core.set_locale("fr".into()).await.unwrap();
        assert_eq!(
            state_rx.borrow_and_update().status.as_deref(),
            Some("VEILLE"),
            "set_locale must republish the new standby word at once, without waiting for a new Power cycle"
        );
    }

    /// Standby on disk must describe the device, not an intention: it is
    /// all `StartupPower::Previous` has to decide at the next startup.
    /// Both directions of the toggle and both branches of the startup
    /// write it.
    #[tokio::test]
    async fn standby_is_persisted_at_every_toggle() {
        let (mut core, _pc, _sc, _rx, dir) = setup();
        let on_disk = || crate::state::load(&dir.path().join("state.json")).standby;

        core.handle_command(Command::Power).await.unwrap(); // standby
        assert!(on_disk(), "entering standby is written");
        core.handle_command(Command::Power).await.unwrap(); // wake
        assert!(!on_disk(), "waking too");

        // And a startup puts the file back in agreement with what it did,
        // in both directions: without this, "previous state" chosen later
        // would resurrect a standby the device left long ago.
        core.start_in_standby().await.unwrap();
        assert!(on_disk());
        core.startup().await.unwrap(); // default setting: "on"
        assert!(!on_disk());
    }

    #[test]
    fn the_core_embedded_en_is_non_empty() {
        assert!(!ritornello_i18n::try_parse(crate::i18n::EN).unwrap().is_empty());
    }

    #[test]
    fn key_parity_between_the_embedded_en_and_the_fr_pack() {
        let en = ritornello_i18n::try_parse(crate::i18n::EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let mut en_keys: Vec<&String> = en.keys().collect();
        let mut fr_keys: Vec<&String> = fr.keys().collect();
        en_keys.sort();
        fr_keys.sort();
        assert_eq!(en_keys, fr_keys, "en/fr key sets diverge");
    }

    #[test]
    fn the_cache_estimate_never_promises_that_every_cover_fits() {
        // **A promise the cache cannot keep, in both catalogues.** With
        // re-encoding off, a local cover costs no bytes at all
        // (`cover::payload_cost`), so this sentence read "every cover fits" —
        // and it was false: `cover::evict_to_budget` trims `entries` down to
        // `cover::MAX_ENTRIES` whatever they cost, so cover 257 evicts cover
        // 1. What the user then meets is not a missing niceness but the one
        // failure the subsystem itself logs as a broken promise —
        // `cover_get` answering 404 on a key the core published in
        // `cover_href`, and the square falling back to its ♫. A NAS library
        // past a few hundred albums is unremarkable.
        //
        // **This test pins the claim, not the sentence.** It does not compare
        // the string to a literal — that would break on any rewording and
        // teach nothing — but refuses the two shapes of unbounded promise the
        // wording must never take again, and requires the ceiling to be
        // stated. Named production change it guards: putting "every cover
        // fits" / "toutes les pochettes tiennent" back into either catalogue.
        let en = ritornello_i18n::try_parse(crate::i18n::EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let key = "cover_cache_estimate_unlimited";
        let en_text = en.get(key).expect("the embedded English carries this key").to_lowercase();
        let fr_text = fr.get(key).expect("the shipped French carries this key").to_lowercase();

        for forbidden in ["every cover", "all covers", "any number"] {
            assert!(
                !en_text.contains(forbidden),
                "the English estimate must not promise an unbounded cache: {en_text}"
            );
        }
        for forbidden in ["toutes les pochettes", "toute pochette", "sans limite"] {
            assert!(
                !fr_text.contains(forbidden),
                "the French estimate must not promise an unbounded cache: {fr_text}"
            );
        }
        assert!(
            en_text.contains("a few hundred"),
            "the English estimate must state the ceiling it does have: {en_text}"
        );
        assert!(
            fr_text.contains("quelques centaines"),
            "the French estimate must state the ceiling it does have: {fr_text}"
        );
    }
}
