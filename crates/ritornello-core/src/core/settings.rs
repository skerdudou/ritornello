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
    /// (read by the status page) and persists the state.
    ///
    /// Called from the `select!` loop of `main` on reception from the
    /// `locale_rx` channel, itself fed by the `PUT /api/locale` route.
    ///
    /// Also resolves `standby_status` in the brand-new catalog, and publishes
    /// the state: without the latter, changing language during standby left
    /// the word displayed in the old language until the next
    /// `Command::Power` cycle (see the doc of `standby_status`).
    ///
    /// **No message reaches a plugin here.** A language never crosses the
    /// Source wire at all (task 11 of the language-packs chantier retired
    /// `SourceReq::SetLocale`): a Source's `status_text` is resolved against
    /// the registry at publication (see `Core::decide_status_text`), and a
    /// plugin's own admin catalog is served on demand, by locale, straight
    /// from the registry (`admin::admin_i18n`) — nothing a plugin process
    /// holds ever goes stale, so there is nothing left here to notify it of.
    ///
    /// The catalog is rebuilt through `crate::i18n::core_catalog`, which
    /// stacks the full chain a `Registry` produces (task 4) — same
    /// construction as the core's own startup, so a locale change and a
    /// fresh boot never resolve a key two different ways. The device's own
    /// fallback (`self.fallback`, task 13's setting; `"en"` if none is set
    /// yet) is passed as the second language; until a fallback is chosen it
    /// coincides with the structural `en` block, which is harmless — see
    /// `Registry::chain_for`.
    ///
    /// A real locale change is also the registry's refresh gesture: the
    /// registry's disk tier is swept once (at startup) and never re-read on
    /// its own, so resweeping here is what lets an operator who edited a
    /// pack on disk see it without restarting the service. **This method
    /// must actually run for that to happen** — `ConfigView.vue`'s
    /// `saveDisplay` short-circuits before `PUT /api/locale` when the
    /// picked language (and, while it is incomplete, the fallback) is
    /// unchanged from what was last loaded, so re-picking the language
    /// already in force calls neither this method nor `set_fallback` and
    /// resweeps nothing (task 15 review, blocking finding 3: an earlier
    /// version of this comment, and of `docs/interface.md`, claimed
    /// otherwise). The two gestures that do reach here: a service restart
    /// (which sweeps once at startup regardless), or an **actual** change
    /// of the language or the fallback. Through `Registry::resweep_async`,
    /// not the bare, synchronous
    /// `resweep`: the directory walk and TOML parse run off the async
    /// runtime and before any lock is taken, so this call never blocks a
    /// concurrent reader of the registry (`admin::admin_i18n`, since task 5)
    /// behind disk I/O — see `resweep_async`'s own doc.
    pub async fn set_locale(&mut self, locale: String) -> Result<()> {
        self.locale = Some(locale.clone());
        crate::i18n::Registry::resweep_async(&self.registry).await;
        let fallback = self.fallback.clone().unwrap_or_else(|| "en".to_string());
        let new_catalog = crate::i18n::core_catalog(&*self.registry.read().await, &locale, &fallback);
        self.standby_status = Some(resolve_standby_status(&new_catalog));
        *self.catalog.write().await = new_catalog;
        self.persist();
        self.publish_state();
        Ok(())
    }

    /// Changes the device's fallback language — the setting behind the
    /// chosen → fallback → English → key resolution order (`Registry::
    /// chain_for`) — and retranslates and republishes at once, exactly like
    /// `set_locale` does for the chosen language.
    ///
    /// Called from the `select!` loop of `main` on reception from the
    /// `fallback_rx` channel, itself fed by `PUT /api/locale`'s optional
    /// `fallback` field.
    ///
    /// **Republishing here, without waiting for a new frame from any
    /// plugin, is the point.** A standing status resolved through the old
    /// fallback must retranslate the moment the setting changes — the exact
    /// defect (a status stuck in the old language until the next unrelated
    /// event) this whole chantier exists to fix, now for the fallback
    /// setting too, not only for the chosen language `set_locale` already
    /// covers.
    ///
    /// **Accepted whatever its relationship to the chosen language.** A
    /// fallback equal to `self.locale` is stored and simply has no visible
    /// effect — `Registry::chain_for` tries the chosen block first and only
    /// falls through to the fallback block on a miss, so a coinciding
    /// fallback changes nothing observable — by the owner's own rule:
    /// switching language must never invalidate a fallback already stored,
    /// and refusing "fallback == locale" would be indistinguishable, from
    /// the caller's side, from that same invalidation.
    pub async fn set_fallback(&mut self, fallback: String) -> Result<()> {
        self.fallback = Some(fallback.clone());
        crate::i18n::Registry::resweep_async(&self.registry).await;
        let locale = self.locale.clone().unwrap_or_else(|| "en".to_string());
        let new_catalog = crate::i18n::core_catalog(&*self.registry.read().await, &locale, &fallback);
        self.standby_status = Some(resolve_standby_status(&new_catalog));
        *self.catalog.write().await = new_catalog;
        self.persist();
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

    /// The local day the last automatic update run happened on, as
    /// `schedule::day_key` identifies it. `None` on a device that has never
    /// had one.
    pub fn update_last_run_day(&self) -> Option<i64> {
        self.update_last_run_day
    }

    /// Records — or un-records — the local day of the last automatic run, and
    /// writes it down at once.
    ///
    /// **Noted before the run rather than after it**, and that is the point:
    /// the ticker asks every minute, so a day noted only on success would fire
    /// again sixty seconds after a failed check — and, under
    /// `CheckAndInstall`, keep re-downloading all night. Once a day means once
    /// a day, whatever the outcome of the run.
    ///
    /// Which is why it takes an `Option` rather than a day: a run that never
    /// *left* — the worker's queue was full — has not had its turn, and the
    /// caller puts the previous value back so the next minute tries again.
    /// The distinction is "did it happen", not "did it succeed".
    ///
    /// It goes through `persist` like every other piece of state, which is
    /// what keeps it from being lost by the next unrelated settings write.
    pub fn set_update_last_run_day(&mut self, day: Option<i64>) {
        self.update_last_run_day = day;
        self.persist();
    }

    pub(super) fn persist(&self) {
        let st = PersistedState {
            active_source: self.active_source.clone(),
            volume: self.volume,
            standby: self.standby,
            audio_device: self.audio_device.clone(),
            locale: self.locale.clone(),
            fallback: self.fallback.clone(),
            theme: self.theme.clone(),
            mode: self.mode.clone(),
            settings: self.settings.clone(),
            random: self.random,
            repeat_all: self.repeat_all,
            update_last_run_day: self.update_last_run_day,
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
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())), ..Default::default() }));
        let persisted = PersistedState {
            active_source: "radio".into(),
            volume: 60,
            standby: false,
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            fallback: None,
            theme: None,
            mode: None,
            settings: crate::state::Settings::default(),
            random: false,
            repeat_all: false,
            update_last_run_day: None,
        };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Chain::load_for_tests("core", "en", &root, crate::i18n::EN)));
        let (covers, cover_tx) = test_covers();
        let manifest_order = declared_order(&sources);
        let mut core = Core::new(player, Wiring { sources, persisted, state_path: dir.path().join("state.json"), catalog, registry: test_registry(&root), manifest_order, metadata: silent_wiring(vec![]), sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
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
    async fn set_locale_persists_and_reaches_no_source() {
        // No message reaches a plugin any more (task 11 of the
        // language-packs chantier retired `SourceReq::SetLocale`):
        // resolution is entirely the core's own, through the registry.
        let (mut core, _pc, source_calls, _rx, dir) = setup();
        core.set_locale("fr".into()).await.unwrap();
        assert!(
            source_calls.lock().unwrap().is_empty(),
            "a locale change must not send anything to a Source plugin"
        );
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.locale.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn set_fallback_persists_and_reaches_no_source() {
        // Same guarantee as `set_locale`: the fallback never crosses the
        // Source wire, since resolution is entirely the core's own.
        let (mut core, _pc, source_calls, _rx, dir) = setup();
        core.set_fallback("fr".into()).await.unwrap();
        assert!(
            source_calls.lock().unwrap().is_empty(),
            "a fallback change must not send anything to a Source plugin"
        );
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.fallback.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn a_fallback_survives_a_later_locale_change_to_a_different_language() {
        // The owner's rule: the settings page's fallback control is shown
        // only while the chosen language is incomplete, so it disappears
        // and reappears as the chosen language changes — and must not lose
        // its value in between. Someone who picks a fallback for an
        // incomplete language, moves to a different (here, complete)
        // language and back must find the same fallback still stored.
        let (mut core, _pc, _sc, _rx, dir) = setup();
        core.set_locale("de".into()).await.unwrap();
        core.set_fallback("fr".into()).await.unwrap();
        core.set_locale("en".into()).await.unwrap();
        core.set_locale("de".into()).await.unwrap();
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(
            st.fallback.as_deref(),
            Some("fr"),
            "an unrelated locale change must not clear the stored fallback"
        );
    }

    #[tokio::test]
    async fn a_fallback_equal_to_the_chosen_locale_is_accepted_and_stored() {
        // The owner's other rule: a fallback that happens to equal the
        // chosen language is accepted and stored, not refused — refusing it
        // would be indistinguishable, from the caller, from clearing a
        // stored fallback the moment the chosen language catches up to it.
        let (mut core, _pc, _sc, _rx, dir) = setup();
        core.set_locale("fr".into()).await.unwrap();
        core.set_fallback("fr".into()).await.unwrap();
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.locale.as_deref(), Some("fr"));
        assert_eq!(st.fallback.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn set_fallback_retranslates_a_standing_status_with_no_new_frame_from_any_plugin() {
        // Partner regression to `changing_language_in_standby_republishes_
        // the_standby_word_at_once`, for the fallback setting: a standing
        // status resolved through the old fallback must retranslate the
        // moment the setting changes, not wait for the next `Command::Power`
        // cycle or any other event from a Source. The word here comes from
        // neither the chosen language ("fr", which never defines it) nor a
        // hardcoded "en" — only from the fallback tier `set_fallback` wires
        // in, proving the chain actually reaches it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/nl.toml"), "standby = \"SLAAP\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())), ..Default::default() }));
        let (state_tx, mut state_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Chain::load_for_tests("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataWiring {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            state: state_tx,
        };
        let (covers, cover_tx) = test_covers();
        let manifest_order = declared_order(&sources);
        let mut core = Core::new(player, Wiring { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, registry: test_registry(&root), manifest_order, metadata, sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        // No `core/fr.toml` on disk, and this rig's registry is a bare
        // sweep (no embedded English seed) — so before any fallback is set,
        // nothing in the chain defines "standby" and `Chain::get` falls
        // back to the raw key, its own documented safety net.
        core.set_locale("fr".into()).await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(
            state_rx.borrow_and_update().status.as_deref(),
            Some("standby"),
            "neither the chosen language nor a hardcoded en resolves the key in this rig"
        );
        core.set_fallback("nl".into()).await.unwrap();
        assert_eq!(
            state_rx.borrow_and_update().status.as_deref(),
            Some("SLAAP"),
            "set_fallback must republish the retranslated standby word at once, with no new frame from any plugin"
        );
    }

    #[tokio::test]
    async fn the_standby_word_is_translated_by_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())), ..Default::default() }));
        let (state_tx, mut state_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Chain::load_for_tests("core", "fr", &root, crate::i18n::EN)));
        let metadata = MetadataWiring {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            state: state_tx,
        };
        let (covers, cover_tx) = test_covers();
        let manifest_order = declared_order(&sources);
        let mut core = Core::new(player, Wiring { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, registry: test_registry(&root), manifest_order, metadata, sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
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
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())), ..Default::default() }));
        let (state_tx, mut state_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        // Built in English: "STANDBY", the embedded value of the key.
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Chain::load_for_tests("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataWiring {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            state: state_tx,
        };
        let (covers, cover_tx) = test_covers();
        let manifest_order = declared_order(&sources);
        let mut core = Core::new(player, Wiring { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, registry: test_registry(&root), manifest_order, metadata, sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
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
        // Default setting: "on", and no install marker to override it.
        core.startup(crate::update::StartupOverride::AsConfigured).await.unwrap();
        assert!(!on_disk());
    }

    #[test]
    fn the_core_embedded_en_is_non_empty() {
        assert!(!ritornello_i18n::try_parse(crate::i18n::EN).unwrap().is_empty());
    }

    /// **Generalized (task 15).** Was "en vs fr, key sets only" — a
    /// hardcoded language that stops covering a second one the moment it
    /// ships, and a comparison blind to a translation that renamed or
    /// dropped a `{named}` parameter — task 14 added several phrase keys
    /// with more than one, so this is not theoretical. `shipped_language_
    /// packs` derives the language list from the tree; the `assert!(!
    /// shipped.is_empty(), ...)` below is what keeps that derivation honest
    /// instead of vacuously green on a broken discovery. `fr_pack()` above
    /// stays: two other tests in this file still want the shipped French
    /// text specifically, not every shipped language.
    #[test]
    fn key_and_param_parity_between_the_embedded_en_and_every_shipped_language() {
        let en = ritornello_i18n::try_parse(crate::i18n::EN).unwrap();
        let deploy_locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        let shipped = ritornello_i18n::shipped_language_packs(&deploy_locales, "core");
        assert!(!shipped.is_empty(), "no shipped language found for core under deploy/locales");
        for (lang, content) in shipped {
            let pack = ritornello_i18n::try_parse(&content)
                .unwrap_or_else(|e| panic!("{lang} pack for core is invalid TOML: {e}"));
            let mut en_keys: Vec<&String> = en.keys().collect();
            let mut pack_keys: Vec<&String> = pack.keys().collect();
            en_keys.sort();
            pack_keys.sort();
            assert_eq!(en_keys, pack_keys, "en/{lang} key sets diverge for core");

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

    /// The sentence that tells an owner which switch to tick must name that
    /// switch as it is actually labelled. A rename on one side alone makes
    /// the interface contradict itself, and no other test would notice.
    #[test]
    fn the_prerelease_sentence_quotes_the_switch_label() {
        for pack in [
            ritornello_i18n::try_parse(crate::i18n::EN).unwrap(),
            ritornello_i18n::try_parse(&fr_pack()).unwrap(),
        ] {
            let label = pack.get("update_prereleases_label").expect("label key present");
            let sentence = pack.get("update_only_prereleases").expect("sentence key present");
            assert!(
                sentence.contains(label.as_str()),
                "{sentence:?} does not name {label:?}"
            );
        }
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
