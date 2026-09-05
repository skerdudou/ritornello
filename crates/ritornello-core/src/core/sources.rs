//! Source supervisor: the cycle order, switching, hot arrival and death of a plugin, and applying a SourceAction.

use super::*;

impl<P: Player> Core<P> {
    /// Name of the currently active source (for the live status page).
    pub fn active_source(&self) -> &str {
        &self.active_source
    }

    /// Current language, to pass along when launching a relit plugin.
    ///
    /// The language is passed to the process via `RITORNELLO_LOCALE`: a plugin
    /// relit on a French-speaking device must find it at startup, without
    /// waiting for a `SetLocale` — the trap already met with `cd`, which
    /// displayed `NO DISC` again for lack of a language as long as no language
    /// change happened afterwards.
    pub fn current_locale(&self) -> Option<String> {
        self.locale.clone()
    }

    /// Adds a source discovered **after** startup: a plugin that missed the
    /// rendezvous, or that was relaunched by hand. Returns `true` if it is a
    /// replacement (re-announcement of a plugin already wired).
    ///
    /// `source_order` is **re-sorted**: the source cycle follows alphabetical
    /// order, and a source that arrived late must take its normal place in it,
    /// not the tail — otherwise `SourceCycle` changes direction depending on
    /// the startup chronology.
    ///
    /// If no source was active — a startup where *none* had answered — the new
    /// one becomes active: this is the only case where a plugin's arrival
    /// changes what plays.
    ///
    /// **Wakes nothing**: this function only affects the table and the name of
    /// the active one. Hot wiring goes through `hotplug_source`, which chains
    /// the wake — without which a first source arriving late would be active
    /// and silent.
    pub fn add_source(&mut self, name: String, client: Arc<dyn Source>) -> bool {
        let first = self.sources.is_empty();
        let replacement = self.sources.insert(name.clone(), client).is_some();
        if !self.source_order.contains(&name) {
            self.source_order.push(name.clone());
            self.source_order.sort();
        }
        if first {
            self.active_source = name;
        }
        // The sources_catalog just changed length: one more source is listed
        // in it, without presets until it has declared some. See
        // `publish_catalog` for the up-to-date list of its call sites —
        // `remove_source` is its counterpart.
        self.publish_catalog();
        replacement
    }

    /// Switches to `next` (or to **no** source if `None`): stop, `Deactivate`
    /// of the outgoing one, forgettings, persistence, `Activate` of the
    /// incoming one.
    ///
    /// Extracted from `Command::SourceCycle` rather than copied: deactivating a
    /// plugin does exactly the same thing, and two versions of this sequence
    /// would diverge at the first forgetting added on one side.
    ///
    /// Three callers, then: `SourceCycle` (which computes the next name in the
    /// order), `SelectSource` (which receives it ready-made, from the MPD
    /// plugin) and `remove_source` (which may have no name to give). Common
    /// sequence: player stop, best-effort `Deactivate`, forgetting of the
    /// identity, the preset count, the status and eject, `persist()` **before**
    /// `Activate`, final publication.
    pub(super) async fn cycle_source(&mut self, next: Option<String>) -> Result<()> {
        // Changing source always means changing what plays — and it is the
        // core that stops, without depending on the plugins' answers. Before,
        // the action returned by `Deactivate` (the radio plugin's `Stop`) was
        // ignored, and stopping relied on the `Play` of the following
        // `Activate` — which the cd without a disc does not return (`Noop`):
        // the old stream kept playing under a display announcing the new
        // source, ICY titles included.
        self.expecting_stream = false;
        self.playback = false;
        self.player.stop().await?;
        // The old source is notified best-effort: its stop is already done,
        // it only has to realign its own state.
        if let Err(e) = self.active_request(SourceReq::Deactivate).await {
            tracing::debug!("deactivate: {e}");
        }
        self.active_source = next.unwrap_or_default();
        // Acknowledged here without waiting for the new Source to declare it:
        // otherwise a Source that omitted to do so would leave the other's
        // identity in place, and the `metadata` plugins would keep enriching
        // the previous track.
        self.set_identity(None);
        // The preset count and the status announced by the old Source mean
        // nothing for the new one: keeping them would display a window of
        // numbers that matches no real preset, or a status ("NO DISC") under
        // the name of a source that has not yet said anything — until the new
        // Source has spoken (which may never happen: an empty preset declares
        // an ephemeral frame, which does not touch the remembered status).
        self.preset_count = None;
        self.source_status = None;
        // Same for eject: the capability describes the Source that is
        // leaving. Without this clearing, leaving the cd for the radio left
        // the Eject key active until the radio's first frame — and for good
        // if it stayed silent.
        self.can_eject = false;
        self.retry_count = 0;
        // Persist **before** `Activate`: if the new source does not answer
        // (the SDK's 5 s timeout), the in-memory state, the on-disk state and
        // the display already all say the same thing — new source, nothing
        // plays. Without this, the failure left the switch half done: "cd" on
        // screen, "radio" in state.json.
        self.persist();
        if let Some(action) = self.active_request(SourceReq::Activate).await? {
            self.apply(action).await?;
        }
        // The sequence is only complete once the new state is published: all
        // the paths above (`set_identity`, `apply`) only publish when they
        // change something, and nothing guarantees that at least one of them
        // does — deactivating the only source, or deactivating it while it
        // plays without a silent Source answering in time, triggers none of
        // them. `handle_command` already publishes after each command, but a
        // caller outside that path (hot unwiring of a plugin) would otherwise
        // leave the displays describing a source that no longer exists. The
        // channel deduplicates (`publish_state`), so this call costs nothing
        // more on the `SourceCycle` path.
        self.publish_state();
        Ok(())
    }

    /// Forgets a source whose plugin died **on its own** — panic, `SIGSEGV`,
    /// killed by hand. Returns `false` if this name was not a source.
    ///
    /// **The difference with `remove_source` is deliberate, and it fits in one
    /// sentence: that one switches, this one does not.** Both evict the same
    /// thing from the sources_catalog, for the same reason (an MPD client must
    /// not see a stored playlist for a source it can no longer reach); only
    /// the consequence on what plays differs, because only the question of
    /// who decided differs.
    ///
    /// * `remove_source`: **the operator asked** for this source to go.
    ///   Switching to the next one is the continuation of their gesture, and
    ///   stopping the player first is what prevents the old stream from
    ///   continuing under the new source's name.
    /// * here: **nobody asked for anything**. A Source plugin is a
    ///   *controller* — it says what to play, it does not play. The stream is
    ///   held by mpv, which is a child of the core and which the plugin's
    ///   death does not touch. Stopping mpv and switching to the cd would turn
    ///   a controller's failure into silence, then show on screen a source the
    ///   user did not choose: two faults, the second of which is a lie. So we
    ///   do neither — the music goes on, `active_source` keeps the name of the
    ///   source that disappeared, and the status page tells the whole truth
    ///   ("radio", active, not reachable).
    ///
    /// What is forgotten anyway: the named presets (the sources_catalog must
    /// not offer to act on a dead plugin) and, if it was the active one, the
    /// two **capabilities** it had declared — `preset_count` and `can_eject`.
    /// Those describe what a plugin can do, and it is no longer there to do
    /// it: leaving the Eject key lit or the preset grid open would give
    /// commands that can no longer succeed. `cycle_source` already clears
    /// them for this exact reason.
    ///
    /// What is kept, and this is also intended: `source_status` and the
    /// identity of what plays. They describe **the current track**, which is
    /// still playing; clearing them would black out the display in the middle
    /// of a title. `persist()` is not called: `active_source` did not change,
    /// so the on-disk state still names the source the user chose — at the
    /// next startup the plugin is relaunched and finds it again.
    ///
    /// Non-`async`: the direct consequence of not switching. No `Deactivate`
    /// to send_frame (the peer is dead), no `Activate` to wait for.
    pub fn forget_dead_source(&mut self, name: &str) -> bool {
        let Some(pos) = self.source_order.iter().position(|n| n == name) else {
            return false;
        };
        self.sources.remove(name);
        self.source_order.remove(pos);
        self.presets_par_source.remove(name);
        if self.active_source == name {
            self.preset_count = None;
            self.can_eject = false;
        }
        self.publish_catalog();
        // Publish the state too: `can_eject` and `preset_count` are part of
        // it, and no other path will do it — this arm is not a command.
        self.publish_state();
        true
    }

    /// Removes an unwired source — a plugin just switched off from the UI.
    /// Returns `false` if this name was not a source.
    ///
    /// **Not to be confused with `forget_dead_source`**, which handles the
    /// *suffered* death of the same plugin: that one does not switch and does
    /// not stop the player. The other's doc carries the comparison of the two
    /// paths.
    ///
    /// If it was the active one, the **next in the cycle** takes its place, or
    /// none if there is none left: `active_request` already tolerates the
    /// absence of a source, and starting without a source has been legitimate
    /// since hot registration.
    ///
    /// The order is delicate: the switch must happen **before** the removal
    /// from the table, because it is what sends `Deactivate` to the outgoing
    /// source — removed first, it would receive nothing and the plugin would
    /// keep its internal state for its next life.
    pub async fn remove_source(&mut self, name: &str) -> Result<bool> {
        let Some(pos) = self.source_order.iter().position(|n| n == name) else {
            return Ok(false);
        };
        if self.active_source == name {
            let next = if self.source_order.len() > 1 {
                Some(self.source_order[(pos + 1) % self.source_order.len()].clone())
            } else {
                None
            };
            // No `?`: the switch may fail (the incoming one does not answer
            // `Activate`, or the stop itself fails), but the removal that
            // follows must happen anyway. A plugin being switched off must
            // end up fully unwired — never halfway, with a `SourceCycle` that
            // could still land on a process that no longer exists — that is
            // the whole principle of an acknowledgement that only describes
            // an already-true state.
            if let Err(e) = self.cycle_source(next.clone()).await {
                tracing::warn!("switching away from {name} while removing it: {e:#}");
                // `cycle_source` sets `active_source` **before** its stage
                // that can fail (`Activate`) but **after** a `stop()` that can
                // fail too: depending on the stage at fault, `active_source`
                // may still name the source being removed from the table.
                // Setting it again here is safe in both cases.
                self.active_source = next.unwrap_or_default();
            }
        }
        self.sources.remove(name);
        self.source_order.remove(pos);
        // The source's named presets leave with it, and the sources_catalog is
        // republished right away.
        //
        // This is not housekeeping: the sources_catalog is the only channel
        // through which an MPD client learns that a stored playlist exists.
        // Left in place, the entry would list in `listplaylists` a source that
        // no longer exists, and a client could **act** on it — a `load "radio"`
        // on a switched-off plugin. The `Command::SelectSource` guard would
        // refuse it (`source_order` no longer carries the name), but the user
        // would see a playlist that lies until restart: MPD clients readily
        // cache `listplaylists`.
        //
        // `source_order` is emptied just above, so `sources_catalog()` already
        // no longer cites this source; removing the table too prevents a
        // plugin relit under the same name from silently inheriting the
        // playlist of its previous life instead of waiting for its own
        // `ListPresets` (see `hotplug_source`).
        self.presets_par_source.remove(name);
        self.publish_catalog();
        Ok(true)
    }

    /// Wires a source that announces itself **after** startup. Returns `true`
    /// if it is a replacement (re-announcement of a plugin already wired).
    ///
    /// Two paths, and keeping them together here is the whole point:
    ///
    /// - **First source of the core** (the table was empty): startup is
    ///   replayed by `resume`, so `SetLocale` then `Wake`, in that order.
    ///   `add_source` only designates the active one; without this wake, a
    ///   source arriving at t+30 s would be active and **silent** until the
    ///   user touched something — the device would look broken while
    ///   everything is wired.
    /// - **Additional source, or core in standby**: only the language is due.
    ///   Waking here would relight a device that was deliberately switched
    ///   off, and would change what plays because a plugin finished starting.
    ///
    /// The state is published in both cases: the source's name just appeared
    /// in the frame, and the SPA as well as the displays were announcing "no
    /// source" until then. (`resume` already publishes for the first.)
    pub async fn hotplug_source(
        &mut self,
        name: String,
        client: Arc<dyn Source>,
    ) -> Result<bool> {
        let first = self.sources.is_empty();
        let replacement = self.add_source(name.clone(), client);
        if first && !self.standby {
            self.resume().await?;
        } else {
            self.send_locale_to(&name).await;
            self.publish_state();
        }
        Ok(replacement)
    }

    /// Pushes the current language to **a single** source: the one that was
    /// just hot-wired.
    ///
    /// `resume` and `set_locale` only serve the sources present in the table
    /// at the time of their call. A source arriving after — a plugin that
    /// missed the rendezvous, or relaunched by hand without its language
    /// argument — would never have received `SetLocale`: on a French-speaking
    /// device, a relaunched `cd` came back displaying `NO DISC` in its status
    /// line, and would have stayed that way until the next language change.
    ///
    /// No effect if the core has no language set: the plugin then keeps its
    /// default, which is the same as the core's. Best-effort like the two
    /// other paths — a source that does not answer `SetLocale` must not
    /// prevent its wiring.
    pub async fn send_locale_to(&self, name: &str) {
        let Some(locale) = self.locale.clone() else {
            return;
        };
        if let Some(src) = self.sources.get(name)
            && let Err(e) = src.request(SourceReq::SetLocale(locale)).await
        {
            tracing::warn!("SetLocale to {name}: {e}");
        }
    }

    /// Replaces the arbitration order of the `metadata` plugins.
    ///
    /// Called after each late announcement with the **complete** list
    /// recomputed from the manifest: the priority is that of `plugins.toml`,
    /// never the arrival order of the announcements.
    pub fn set_metadata_order(&mut self, order: Vec<String>) {
        self.metadata.set_order(order);
    }

    pub(super) async fn apply(&mut self, action: SourceAction) -> Result<()> {
        match action {
            SourceAction::Noop => {}
            SourceAction::Play { uri, start, finite, playlist } => {
                // The restart machinery (`expecting_stream` then
                // `PlaybackIdle` → retry) only exists for network streams:
                // content that ends is a normal end, not a failure. Confusing
                // it with a cut made the disc restart in a loop: end of disc →
                // mpv idle → restart ~2 s → `Activate` → `Play cdda://` →
                // track 1.
                //
                // It is the Source that declares it, not the core that
                // guesses: the latter sniffed `cdda://`, so that a file path —
                // measured on the bench, mpv going `idle` at the end of the
                // playlist exactly as during a cut — fell on the wrong side.
                self.expecting_stream = !finite;
                self.playback = true;
                // The only place where `playback` becomes true: it is here,
                // and nowhere else, that `paused` must fall back, otherwise
                // yesterday's pause would make a fresh playback "paused".
                self.paused = false;
                // `loadlist` for a playlist, `loadfile` for a medium: it is
                // the Source that declares it, and the core does not guess. An
                // `.m3u8` is a playlist for a file player and an HLS stream
                // for a radio; sniffing the URI would break one or the other.
                if playlist {
                    // The index goes **with** the load, in one operation. It
                    // used to be a correction sent right after, and that
                    // window was long enough for mpv to really open the
                    // list's first entry — measured: it publishes that
                    // entry's `path`, off which the core read a cover and
                    // which it relayed as the playing track. See
                    // `Player::load_list`.
                    self.player.load_list(&uri, start).await?;
                } else {
                    // A medium, not a list: `start` has no meaning here and
                    // the Source is not supposed to send one (see
                    // `SourceAction::start`). Silently ignored rather than
                    // refused — this is a display detail, never a reason to
                    // refuse playback.
                    self.player.play(&uri).await?;
                }
            }
            SourceAction::Stop => {
                self.expecting_stream = false;
                self.playback = false;
                self.player.stop().await?;
            }
            SourceAction::PlayerNext => self.player.next().await?,
            SourceAction::PlayerPrev => self.player.prev().await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;
    use std::sync::Mutex;

    /// Source that never has anything to play: a cd player without a disc.
    struct EmptySource;

    #[async_trait::async_trait]
    impl Source for EmptySource {
        async fn request(&self, _req: SourceReq) -> Result<SourceAction> {
            Ok(SourceAction::Noop)
        }
    }

    /// Source whose activation fails — a stuck plugin, which the SDK
    /// sanctions with a timeout.
    struct FailingSource;

    #[async_trait::async_trait]
    impl Source for FailingSource {
        async fn request(&self, req: SourceReq) -> Result<SourceAction> {
            match req {
                SourceReq::Activate => anyhow::bail!("timeout"),
                _ => Ok(SourceAction::Noop),
            }
        }
    }

    #[test]
    fn active_source_returns_the_current_source() {
        let (core, _pc, _sc, _rx, _d) = setup();
        // PersistedState::default().active_source == "radio".
        assert_eq!(core.active_source(), "radio");
    }

    #[test]
    fn add_source_resorts_the_cycle_order_instead_of_appending() {
        // `SourceCycle` follows alphabetical order. A late source left at the
        // tail would make the cycle direction depend on the startup
        // chronology — the user would press the same key and not get the same
        // source from one day to the next.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let new_source = Arc::new(FakeSource { name: "files", calls: source_calls });
        assert!(!core.add_source("files".into(), new_source), "this is not a replacement");
        assert_eq!(core.source_order, vec!["cd".to_string(), "files".into(), "radio".into()]);
        assert_eq!(
            core.active_source(),
            "radio",
            "an already active source must not be supplanted by a late arrival"
        );
    }

    #[test]
    fn add_source_signals_a_replacement_without_duplicating_the_order() {
        // Re-announcement of a plugin already wired: the client is replaced,
        // the cycle does not gain a duplicate entry.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let replacement = Arc::new(FakeSource { name: "radio", calls: source_calls });
        assert!(core.add_source("radio".into(), replacement));
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
        assert_eq!(core.active_source(), "radio");
    }

    #[test]
    fn add_source_activates_the_first_source_and_only_the_first() {
        // The only case where a plugin's arrival changes what plays: no
        // source had answered at startup, so nothing was active.
        let (mut core, _rx, dir) = setup_without_source();
        assert_eq!(core.active_source(), "");
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.add_source("radio".into(), Arc::new(FakeSource { name: "radio", calls: calls.clone() }));
        assert_eq!(core.active_source(), "radio");
        // The second does not touch it, even if its name sorts first in the order.
        core.add_source("cd".into(), Arc::new(FakeSource { name: "cd", calls }));
        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
        drop(dir);
    }

    #[tokio::test]
    async fn remove_source_switches_to_the_next() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        assert_eq!(core.active_source(), "radio");

        assert!(core.remove_source("radio").await.unwrap());

        assert_eq!(core.active_source(), "cd", "the next in the cycle takes the place");
        assert_eq!(core.source_order, vec!["cd".to_string()]);
        let calls = source_calls.lock().unwrap();
        assert!(
            calls.iter().any(|c| c == "radio:Deactivate"),
            "the outgoing one is notified before disappearing: {calls:?}"
        );
        assert!(calls.iter().any(|c| c == "cd:Activate"), "the incoming one is activated: {calls:?}");
    }

    #[tokio::test]
    async fn a_late_presets_answer_does_not_resurrect_a_removed_source() {
        // The race: `ListPresets` is detached, so its answer may arrive after
        // the plugin was switched off. Without protection, it reinserted the
        // entry that `remove_source` had just evicted — and the
        // sources_catalog started again announcing to an MPD client a stored
        // playlist it could act on. This is exactly the defect eviction
        // exists to prevent.
        //
        // **What protects is the early return at the head of
        // `handle_source_update`** (`!self.sources.contains_key(name)`), not a
        // guard placed near the insertion. This test exists because nothing
        // pinned it: the early return came for the *whole* frame, and its doc
        // describes this case well, but no assertion would have prevented it
        // from disappearing. Verified by mutation: removing it makes this test
        // fall.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        assert!(core.remove_source("radio").await.unwrap());
        assert_eq!(names(&core.sources_catalog()), vec!["cd".to_string()]);

        // The late answer arrives now, for a name the core no longer wires.
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP"), preset_of(5, "OUI FM")]));

        assert!(
            !core.presets_par_source.contains_key("radio"),
            "a removed source must not come back through an in-flight answer"
        );
        assert_eq!(
            names(&core.sources_catalog()),
            vec!["cd".to_string()],
            "and the sources_catalog must not re-announce it"
        );
    }

    #[tokio::test]
    async fn removing_a_source_takes_it_out_of_the_catalog_with_its_presets() {
        // Merge of two workstreams: `remove_source` (hot switch-off of a
        // plugin) arrived from one side, `presets_par_source` and the
        // sources_catalog channel from the other — and nothing linked them.
        // Left in place, the entry listed in an MPD client's `listplaylists` a
        // switched-off source it could **act** on: the `load` would be refused
        // by the `SelectSource` guard, but the user would see a playlist that
        // lies until restart, MPD clients readily caching this
        // sources_catalog.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP"), preset_of(5, "OUI FM")]));
        assert_eq!(names(&core.sources_catalog()), vec!["cd".to_string(), "radio".into()]);

        assert!(core.remove_source("radio").await.unwrap());

        assert_eq!(names(&core.sources_catalog()), vec!["cd".to_string()], "the source leaves the sources_catalog");
        assert!(
            !core.presets_par_source.contains_key("radio"),
            "its presets leave with it: a plugin relit under the same name \
             must wait for its own ListPresets, not inherit from its previous life"
        );
    }

    #[tokio::test]
    async fn a_vanished_source_no_longer_receives_a_switch_and_leaves_the_catalog() {
        // **The danger common to both paths of a plugin's disappearance.** A
        // vanished plugin that left its name in `source_order` and its
        // presets in `presets_par_source` made an MPD client keep its stored
        // playlist in cache, and a `load` on it **passed** the `SelectSource`
        // guard. The switch then went to a dead socket and paid up to two
        // 5 s timeouts of the sources protocol — `Deactivate` then
        // `Activate` — in the main loop, silent during that time. This test
        // takes the voluntary path (`remove_source`); its twin just below
        // takes the suffered-death one (`forget_dead_source`), and it is
        // their *difference* that is pinned there.
        //
        // The test pins both halves in a row: leaving the sources_catalog, and
        // the fact that a `SelectSource` on this name no longer talks to
        // anyone.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        assert!(names(&core.sources_catalog()).contains(&"radio".to_string()));

        // What the `plugin_waits` arm does when the death was not wanted.
        assert!(core.remove_source("radio").await.unwrap());
        // The switch to "cd" has already happened and spoken: we only want to
        // observe what follows.
        source_calls.lock().unwrap().clear();

        // What an MPD client still sends, its sources_catalog being cached.
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();

        let calls = source_calls.lock().unwrap().clone();
        assert!(
            calls.is_empty(),
            "no request must leave after the source disappeared, got {calls:?}"
        );
        assert_eq!(core.active_source(), "cd", "and what plays has not moved");
        assert!(
            !names(&core.sources_catalog()).contains(&"radio".to_string()),
            "the vanished source must no longer appear in the sources_catalog"
        );
        assert!(!core.presets_par_source.contains_key("radio"));
    }

    #[tokio::test]
    async fn the_suffered_death_of_the_active_plugin_evicts_without_stopping_the_music_or_changing_source() {
        // **The decision of finding 3, pinned.** The process exit arm called
        // `remove_source`, which switches when it was the active one: a panic
        // of the radio plugin therefore stopped mpv and displayed "cd" on a
        // device whose user had chosen the radio. Yet a Source plugin is a
        // *controller* — the stream is held by mpv, a child of the core,
        // which the plugin's death does not touch.
        //
        // Three properties in a single test, because it is their conjunction
        // that is the decision: nothing stops, nothing switches, and the
        // sources_catalog forgets anyway.
        let (mut core, player_calls, source_calls, state_rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // the radio plays
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        assert_eq!(state_rx.borrow().playback, Playback::Playing);
        player_calls.lock().unwrap().clear();
        source_calls.lock().unwrap().clear();

        assert!(core.forget_dead_source("radio"));

        assert_eq!(
            core.active_source(),
            "radio",
            "nobody asked to change source: the displayed name must stay the one \
             the user chose, dead plugin or not"
        );
        assert_eq!(
            state_rx.borrow().playback,
            Playback::Playing,
            "a controller's failure must not silence mpv, which is not inside the plugin"
        );
        assert!(
            player_calls.lock().unwrap().is_empty(),
            "no order to the player: got {:?}",
            player_calls.lock().unwrap()
        );
        assert!(
            source_calls.lock().unwrap().is_empty(),
            "neither Deactivate nor Activate: the peer is dead and the other source asked for nothing, \
             got {:?}",
            source_calls.lock().unwrap()
        );
        // And the eviction did happen: it is the half common to both paths.
        assert_eq!(names(&core.sources_catalog()), vec!["cd".to_string()]);
        assert!(!core.presets_par_source.contains_key("radio"));
        // The dead source's capabilities are forgotten: a lit Eject key or an
        // open preset grid would offer commands that can no longer succeed.
        assert!(!state_rx.borrow().can_eject);
        assert_eq!(state_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn after_the_active_source_dies_the_source_key_starts_again_from_the_first() {
        // The corollary of the decision above: `active_source` is no longer
        // in `source_order`, and `SourceCycle` must still lead somewhere
        // useful. A `position().unwrap_or(0)` followed by a `+ 1` skipped the
        // first source, which became unreachable from the keyboard.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let files = Arc::new(FakeSource { name: "files", calls: source_calls });
        core.add_source("files".into(), files);
        assert_eq!(core.source_order, vec!["cd".to_string(), "files".into(), "radio".into()]);
        assert!(core.forget_dead_source("radio"));

        core.handle_command(Command::SourceCycle).await.unwrap();

        assert_eq!(core.active_source(), "cd", "the first remaining source, not the second");
    }

    #[tokio::test]
    async fn a_catalog_answer_still_in_flight_does_not_resurrect_an_evicted_source() {
        // The `ListPresets` fan-out is **detached**: the request leaves in its
        // own task, and `remove_source` may run between it and its answer.
        // That answer therefore truly arrives after the eviction, and
        // `presets_par_source.insert` is deliberately done **before** the
        // active-source guard (the sources_catalog describes all sources, not
        // the one that plays): the playlist was thus reinserted afterwards,
        // the republished sources_catalog announced a stored playlist for a
        // source that no longer exists, and an MPD client could `load` it.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(core.remove_source("radio").await.unwrap());
        assert!(!names(&core.sources_catalog()).contains(&"radio".to_string()));

        // The in-flight answer, as the `SourceClient` relays it: a non-empty
        // list, without identity or status — the exact shape a `ListPresets`
        // frame takes on the wire.
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP"), preset_of(5, "OUI FM")]));

        assert!(
            !core.presets_par_source.contains_key("radio"),
            "an answer for a source the core no longer knows must be dropped"
        );
        assert!(
            !names(&core.sources_catalog()).contains(&"radio".to_string()),
            "and the sources_catalog must not make it reappear"
        );
    }

    #[tokio::test]
    async fn a_catalog_answer_for_an_inactive_but_alive_source_is_still_taken() {
        // The counterpart of the test above, and it is necessary: a guard
        // that is too broad would also have dropped the playlists of sources
        // that are **alive but not active**, which is precisely the case
        // `presets_par_source` exists to serve — `listplaylistinfo "radio"`
        // while the cd plays.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(core.active_source(), "cd");

        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));

        assert_eq!(
            core.presets_par_source.get("radio").map(|p| p.len()),
            Some(1),
            "the source is not active, but it exists: its playlist must enter the sources_catalog"
        );
    }

    #[tokio::test]
    async fn the_catalog_is_republished_when_a_source_is_removed() {
        // Removal is not enough: without the publication, the displays already
        // connected would keep the previous version of the sources_catalog —
        // the channel being `watch`, nobody asks for it again.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut cat_rx = core.sources_catalog_tx.subscribe();
        cat_rx.borrow_and_update();

        assert!(core.remove_source("radio").await.unwrap());

        assert!(cat_rx.has_changed().unwrap(), "the sources_catalog channel must have moved");
        assert_eq!(names(&cat_rx.borrow_and_update()), vec!["cd".to_string()]);
    }

    #[tokio::test]
    async fn deactivating_the_active_source_republishes_the_state_without_the_leftovers_of_the_outgoing_one() {
        // Final review fix: `cycle_source` is borrowed by `remove_source`
        // (hence by the hot deactivation of a plugin) outside of
        // `handle_command`, the only place that published until now. Without
        // a `publish_state` of `cycle_source`'s own, the frame received by the
        // SPA and the displays kept naming the outgoing source, with its
        // preset count, its status and its eject capability.
        let (mut core, _pc, _sc, state_rx, _d) = setup();
        core.handle_source_update(
            "radio",
            SourceUpdate {
                identity: Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream"}))),
                preset: Some(3),
                preset_count: Some(23),
                preset_name: Some("France Inter".into()),
                status: Some("EN DIRECT".into()),
                can_eject: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(state_rx.borrow().source, "radio");
        assert_eq!(state_rx.borrow().preset_count, Some(23));
        assert!(state_rx.borrow().can_eject);

        assert!(core.remove_source("radio").await.unwrap());

        let state = state_rx.borrow();
        assert_eq!(state.source, "cd", "the frame must name the incoming one, not the outgoing one");
        assert_eq!(state.preset_count, None, "the outgoing one's preset count must not survive");
        assert_eq!(state.status, None, "the outgoing one's status must not survive");
        assert!(!state.can_eject, "the eject capability describes the outgoing one, not the incoming one");
    }

    #[tokio::test]
    async fn remove_source_of_the_last_one_leaves_the_core_without_a_source() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(core.remove_source("cd").await.unwrap());
        assert!(core.remove_source("radio").await.unwrap());

        // No source is a legitimate state: `active_request` tolerates it, and
        // starting without a source has been accepted since hot registration.
        assert_eq!(core.active_source(), "");
        assert!(core.source_order.is_empty());
        // And a command in this state does not panic.
        core.handle_input(InputMessage::from(Command::Next)).await.unwrap();
    }

    #[tokio::test]
    async fn remove_source_of_an_inactive_source_does_not_touch_what_plays() {
        let (mut core, player_calls, _sc, _rx, _d) = setup();

        assert!(core.remove_source("cd").await.unwrap());

        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["radio".to_string()]);
        assert!(
            !player_calls.lock().unwrap().iter().any(|c| c == "stop"),
            "removing an inactive source does not stop what plays"
        );
    }

    #[tokio::test]
    async fn remove_source_of_an_unknown_name_is_a_non_event() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(!core.remove_source("never-seen").await.unwrap());
        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
    }

    #[tokio::test]
    async fn remove_source_stays_complete_when_the_incoming_one_fails_to_activate() {
        // Removing the active source switches to the next in the cycle; here
        // the next is "casse", whose `Activate` fails systematically (see
        // `FakeSource::request`). The removal must nonetheless be complete: a
        // plugin being switched off must never stay half-wired, with a
        // `SourceCycle` that could land on an already killed process.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.add_source("casse".into(), Arc::new(FakeSource { name: "casse", calls: source_calls }));
        assert_eq!(core.source_order, vec!["casse".to_string(), "cd".into(), "radio".into()]);
        assert_eq!(core.active_source(), "radio");

        assert!(
            core.remove_source("radio").await.unwrap(),
            "the removal does happen despite the failed switch to the incoming one"
        );

        assert!(
            !core.sources.contains_key("radio"),
            "the killed source must no longer appear in the table, even if the switch failed"
        );
        assert!(!core.source_order.contains(&"radio".to_string()));
        assert_ne!(
            core.active_source(),
            "radio",
            "the core must no longer name a source it just removed from its table"
        );
    }

    #[tokio::test]
    async fn a_hot_wired_source_receives_the_current_language() {
        // `resume` and `set_locale` only serve the sources present in the
        // table at the time of their call. Without this path, a source
        // arriving after would never have received `SetLocale`: on a
        // French-speaking device, a `cd` relaunched by hand came back
        // displaying `NO DISC`.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.set_locale("fr".into()).await.unwrap();

        let late_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.hotplug_source(
            "files".into(),
            Arc::new(FakeSource { name: "files", calls: late_calls.clone() }),
        )
        .await
        .unwrap();

        // The language, and **nothing else**: `files` is not the core's first
        // source, so it is not woken — what plays does not change because a
        // plugin finished starting.
        assert_eq!(
            late_calls.lock().unwrap().as_slice(),
            ["files:SetLocale(\"fr\")".to_string()]
        );
        assert_eq!(core.active_source(), "radio");
        assert_eq!(
            source_calls.lock().unwrap().iter().filter(|c| c.starts_with("radio:SetLocale")).count(),
            1,
            "only the hot-wired source is concerned, the others are not renotified"
        );
    }

    #[tokio::test]
    async fn without_a_set_language_nothing_is_pushed_to_the_hot_wired_source() {
        // No language on the core side: the plugin keeps its default, which
        // is the same. Pushing `SetLocale(None)` does not exist, and pushing
        // "en" by force would overwrite a plugin launched with its own
        // language.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let late_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.hotplug_source(
            "files".into(),
            Arc::new(FakeSource { name: "files", calls: late_calls.clone() }),
        )
        .await
        .unwrap();
        assert!(late_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_first_hot_wired_source_is_woken() {
        // `add_source` only designates the active one: no `SetLocale`, no
        // `Wake`, no `Activate`. A source arriving at t+30 s would therefore
        // be active and **silent** until the user touched something — the
        // device would look broken while everything is wired.
        let (mut core, mut state_rx, dir) = setup_without_source();
        core.set_locale("fr".into()).await.unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        assert!(
            !core
                .hotplug_source(
                    "radio".into(),
                    Arc::new(FakeSource { name: "radio", calls: seen.clone() })
                )
                .await
                .unwrap(),
            "first wiring, not a replacement"
        );

        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["radio:SetLocale(\"fr\")".to_string(), "radio:Wake".into()],
            "the language BEFORE the wake, exactly as at startup"
        );
        // The `Play` returned by `Wake` was applied: something plays.
        assert!(core.player.calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert_eq!(state_rx.borrow_and_update().source, "radio");
        drop(dir);
    }

    #[tokio::test]
    async fn the_first_hot_wired_source_does_not_wake_a_core_in_standby() {
        // Standby is a **wanted** state: a plugin's arrival does not relaunch
        // the device. Only the language is due, so that the source does not
        // compose its first frame in the language of its launch.
        let (mut core, _rx, dir) = setup_without_source();
        core.set_locale("fr".into()).await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.hotplug_source(
            "radio".into(),
            Arc::new(FakeSource { name: "radio", calls: seen.clone() }),
        )
        .await
        .unwrap();

        assert_eq!(seen.lock().unwrap().as_slice(), ["radio:SetLocale(\"fr\")".to_string()]);
        assert!(
            !core.player.calls.lock().unwrap().iter().any(|c| c.starts_with("play")),
            "nothing must start playing during standby"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn select_is_relayed_to_the_active_source_without_changing_active_source() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://inter".to_string()));
        // Select acts on the already active source; only SourceCycle changes active_source.
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "radio");
    }

    #[tokio::test]
    async fn source_cycle_switches_and_persists() {
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Deactivate"));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Activate"));
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
    }

    #[tokio::test]
    async fn the_source_cycle_behaves_exactly_as_before_the_extraction() {
        // Safety net of the extraction: the body changed function, not
        // meaning. Same assertions as `source_cycle_switches_and_persists`,
        // the proof that `cycle_source` replays exactly the behavior of the
        // block it replaces.
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Deactivate"));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Activate"));
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
    }

    #[tokio::test]
    async fn the_source_by_its_name_switches_like_the_cycle() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        assert_eq!(core.active_source(), "cd");
    }

    #[tokio::test]
    async fn an_unknown_source_is_ignored_without_cutting_anything() {
        // The guard that matters: without it, a stray name would empty the active source.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SelectSource("doesnotexist".into())).await.unwrap();
        assert_eq!(core.active_source(), "radio");
    }

    #[tokio::test]
    async fn selecting_the_already_active_source_does_not_cut_what_plays() {
        // This is exactly what an MPD client sends when reopening its screen:
        // a redundant `load` must not stop playback.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Playing);
        // The full switch (stop then Activate) would also bring back
        // `Playing` for this fake source: the `playback` field alone does not
        // distinguish a redundant call treated as a no-op from one that cut
        // then restarted. The absence of any new `stop` call is the proof
        // that bites.
        player_calls.lock().unwrap().clear();
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();
        assert_eq!(core.player_state().playback, Playback::Playing);
        assert!(
            !player_calls.lock().unwrap().iter().any(|c| c == "stop"),
            "a redundant load must not even stop then relaunch mpv"
        );
    }

    #[tokio::test]
    async fn changing_source_stops_playback_even_if_the_new_one_has_nothing_to_play() {
        // Regression (review 2026-07-27): the action returned by `Deactivate`
        // was ignored and stopping relied on the `Play` of the following
        // `Activate` — which the cd without a disc does not return (`Noop`).
        // The radio kept playing under a display announcing "cd", ICY titles
        // included.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        sources.insert("cd".into(), Arc::new(EmptySource));
        let (state_tx, state_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataWiring {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            state: state_tx,
        };
        let (covers, cover_tx) = test_covers();
        let mut core = Core::new(player, Wiring { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::SourceCycle).await.unwrap();
        // It is the core that stopped mpv, without depending on the plugins.
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
        // And a late ICY title from the old stream reaches nobody anymore: no
        // stream is expected any longer.
        core.handle_event(Event::IcyTitle("late title".into())).await;
        assert_eq!(state_rx.borrow().track.title, None);
    }

    #[tokio::test]
    async fn an_activation_failure_leaves_the_switch_consistent() {
        // Regression (review 2026-07-27): `persist()` was only called after a
        // successful `Activate`. Its failure left the switch half done: "cd"
        // in memory and on screen, "radio" in state.json, and the old stream
        // still audible.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        sources.insert("cd".into(), Arc::new(FailingSource));
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, cover_tx) = test_covers();
        let mut core = Core::new(player, Wiring { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: silent_wiring(vec![]), sources_catalog: watch::channel(SourcesCatalog::default()).0 }, covers, cover_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert!(core.handle_command(Command::SourceCycle).await.is_err());
        // The state is consistent: new source everywhere, and nothing plays.
        assert_eq!(core.active_source(), "cd");
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
    }

    #[tokio::test]
    async fn a_hot_wired_source_enters_the_catalog() {
        // A plugin that missed the rendezvous must appear in the list clients
        // query, without a restart — so `add_source` publishes.
        let (mut core, _rx, dir) = setup_without_source();
        let mut cat_rx = core.sources_catalog_tx.subscribe();
        assert!(core.sources_catalog().sources.is_empty(), "no source at startup");
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.hotplug_source("radio".into(), Arc::new(FakeSource { name: "radio", calls }))
            .await
            .unwrap();
        assert!(cat_rx.has_changed().unwrap(), "the displays must learn about it");
        assert_eq!(names(&cat_rx.borrow_and_update()), vec!["radio".to_string()]);
        drop(dir);
    }

    #[tokio::test]
    async fn a_hot_wired_source_ends_up_with_its_presets() {
        // The complete path of the plugin that missed the rendezvous: it
        // enters the sources_catalog with an empty list, then its answer to
        // `ListPresets` — which hot wiring now requests, like startup — fills
        // it.
        //
        // The source wired second is **not** the active one, which is the
        // real case (a late `radio` while the `cd` plays): the list must
        // therefore pass the active-source guard, and the publication must
        // replace the empty list instead of being deduplicated.
        let (mut core, _rx, dir) = setup_without_source();
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.hotplug_source("cd".into(), Arc::new(FakeSource { name: "cd", calls: calls.clone() }))
            .await
            .unwrap();
        core.hotplug_source("radio".into(), Arc::new(FakeSource { name: "radio", calls }))
            .await
            .unwrap();
        assert_eq!(core.active_source(), "cd", "the first wired stays the active one");
        let mut cat_rx = core.sources_catalog_tx.subscribe();
        assert_eq!(names(&cat_rx.borrow()), vec!["cd".to_string(), "radio".into()]);

        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP"), preset_of(9, "OUI FM")]));
        assert!(cat_rx.has_changed().unwrap(), "the displays must learn about it");
        let cat = cat_rx.borrow_and_update().clone();
        let radio = cat.sources.iter().find(|s| s.name == "radio").expect("radio is declared");
        assert_eq!(radio.presets, vec![preset_of(1, "FIP"), preset_of(9, "OUI FM")]);
        drop(dir);
    }

    #[tokio::test]
    async fn the_old_source_status_does_not_survive_a_source_change() {
        // Regression I2 (branch review): `source_status` was only cleared at
        // the new Source's next frame. A "cd" without a disc declares "no
        // disc"; the user switches to "radio" which has no configured preset
        // (a transient frame does not touch the remembered status): without
        // this fix, the screen kept displaying "no disc" under the "radio"
        // source.
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update = bare_update();
        update.status = Some("pas de disque".into());
        core.handle_source_update("radio", update);
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("pas de disque"));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(
            state_rx.borrow_and_update().status,
            None,
            "the old source's status must not survive the source change"
        );
    }

    #[tokio::test]
    async fn changing_source_broadcasts_the_new_source() {
        // Trap: `SourceCycle` calls `set_identity(None)`, which returns
        // without publishing anything when the identity was **already** null
        // — the case of the cd without a disc. The active source has
        // nevertheless changed. This is what justifies publishing on the way
        // out of the command rather than from `set_identity`.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        assert_eq!(state_rx.borrow().source, "");
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(state_rx.borrow().source, "cd");
    }
}
