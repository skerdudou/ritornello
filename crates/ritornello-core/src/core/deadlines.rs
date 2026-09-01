//! Overlays (volume, mute, tens) and deadlines: what the main.rs loop must wake up for, and when.

use super::*;

impl<P: Player> Core<P> {
    /// Shows (or extends) the temporary volume/mute overlay: line 1 the
    /// "volume" label, line 2 the current percentage or the "muted" message
    /// depending on `self.muted`. Each call pushes the deadline back by
    /// `overlay_ms` (one more press keeps the overlay visible).
    ///
    /// `overlay_ms`, distinct from `tens_window_ms` (see the comment on
    /// `Settings`): this overlay hides the "now playing" view and might want
    /// to get shorter one day, without affecting the time left to compose a
    /// `+NN`. `expire_overlay` does not need to know which of the two
    /// durations set the deadline it disarms: it is stored with the message,
    /// in `self.overlay`.
    pub(super) async fn show_overlay(&mut self) {
        let word = if self.muted {
            let cat = self.catalog.read().await;
            cat.get("muted").to_string()
        } else {
            format!("{} %", self.volume)
        };
        let label = self.catalog.read().await.get("volume_label").to_string();
        let deadline = Instant::now() + Duration::from_millis(self.settings.overlay_ms.into());
        self.overlay = Some((
            Overlay::Volume {
                level: self.volume,
                muted: self.muted,
                text: format!("{label} {word}"),
                remaining_ms: self.settings.overlay_ms,
            },
            deadline,
        ));
    }

    /// Overlay for the pending tens offset ("+10", "+20"): same slot as the
    /// volume overlay, but its own deadline from `tens_window_ms` — the
    /// time left to press the second digit, independent from
    /// `overlay_ms` (see `Settings`). Each press pushes the deadline back,
    /// and `expire_overlay` clears the overlay and the offset together
    /// regardless of which duration is stored here: it reads whatever
    /// deadline is in `self.overlay`, never which field produced it, so
    /// the two stay aligned by construction whatever values the two
    /// settings take.
    pub(super) async fn show_tens_overlay(&mut self) {
        let label = self.catalog.read().await.get("preset_label").to_string();
        let deadline = Instant::now() + Duration::from_millis(self.settings.tens_window_ms.into());
        self.overlay = Some((
            Overlay::Tens {
                offset: self.pending_tens,
                text: format!("{label} +{}", self.pending_tens),
                remaining_ms: self.settings.tens_window_ms,
            },
            deadline,
        ));
    }

    /// Deadline of the active overlay, if there is one (to be read in `main`
    /// before the `select!`, like `retry_at`, to build the timer).
    pub fn overlay_deadline(&self) -> Option<Instant> {
        self.overlay.as_ref().map(|(_, deadline)| *deadline)
    }

    /// Does the core want to be called back in one second to refresh the
    /// position?
    ///
    /// Armed only when there actually is a position to publish: playback in
    /// progress, not in standby, AND (finite content — so mpv has the say on
    /// its position — OR an anchor set by a `metadata` plugin).
    /// `!self.standby && self.playback` alone armed wrongly in two cases
    /// found in review: a stream that no `metadata` plugin follows (nothing
    /// will ever provide a position, the anchor is never set) and pause
    /// (which does not reset `playback` to false). No frame came out of it —
    /// `publish_state` deduplicates — but the device polled mpv twice a
    /// second indefinitely, with nothing to display.
    pub fn tick_position(&self) -> bool {
        !self.standby && self.playback && (!self.expecting_stream || self.position_anchor.is_some())
    }

    /// Clears the expired overlay and lets the permanent state reappear
    /// (source, preset, status, track), kept up to date in the meantime by
    /// the core's other paths.
    ///
    /// Only caller: the `main` loop, with no other publication afterwards —
    /// unlike commands, which publish themselves on the way out of
    /// `handle_command`. Forgetting it here breaks nothing at compile time,
    /// but the screen stops updating on expiry.
    pub fn expire_overlay(&mut self) {
        self.overlay = None;
        self.pending_tens = 0;
        self.publish_state();
    }
}

/// Next deadline of the position tick, from the armed state and the current
/// deadline.
///
/// A pure function, and that is its whole point: the `select!` loop of `main`
/// is covered by no test, and the defect this logic fixes — a **relative**
/// deadline, recreated on every turn, which restarted from zero at every wake
/// of the loop and pushed the tick back indefinitely on an active device —
/// cannot be seen by reading the calling code.
///
/// `armed` = the core wants to be called back; `current` = the deadline
/// already set, if any; `now` = the reference instant, injected so the test
/// has no clock to wait on.
pub fn next_deadline(armed: bool, current: Option<Instant>, now: Instant) -> Option<Instant> {
    match (armed, current) {
        (false, _) => None,
        (true, Some(at)) => Some(at),
        (true, None) => Some(now + Duration::from_secs(1)),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    pub(super) async fn volume_up_shows_the_volume_temporarily() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        state_rx.borrow_and_update();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let e = state_rx.borrow_and_update().clone();
        // PersistedState::default().volume == 60, VolumeUp += 5.
        assert_eq!(e.volume, 65);
        match e.overlay {
            Some(Overlay::Volume { level, muted, text, .. }) => {
                assert_eq!(level, 65);
                assert!(!muted);
                assert_eq!(text, "VOLUME 65 %");
            }
            other => panic!("expected a Volume overlay, got {other:?}"),
        }
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    pub(super) async fn mute_shows_the_muted_overlay() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        state_rx.borrow_and_update();
        core.handle_command(Command::Mute).await.unwrap();
        match state_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Volume { muted, text, .. }) => {
                assert!(muted);
                assert_eq!(text, "VOLUME MUTED");
            }
            other => panic!("expected a Volume overlay, got {other:?}"),
        }
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    pub(super) async fn a_source_update_during_the_overlay_does_not_replace_it_and_reappears_on_expiry() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let with_overlay = state_rx.borrow_and_update().clone();
        assert!(matches!(with_overlay.overlay, Some(Overlay::Volume { .. })));

        // The source update arrives during the overlay: it is remembered
        // (the preset name changes) but the overlay stays displayed.
        let mut update = bare_update();
        update.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update);
        let during = state_rx.borrow().clone();
        assert!(matches!(during.overlay, Some(Overlay::Volume { .. })), "the overlay stays displayed");
        assert_eq!(during.preset_name.as_deref(), Some("FIP"), "but the underlying state is already up to date");

        // On expiry, the overlay disappears and the remembered update is visible.
        core.expire_overlay();
        let after = state_rx.borrow_and_update().clone();
        assert!(after.overlay.is_none());
        assert_eq!(after.preset_name.as_deref(), Some("FIP"));
        assert!(core.overlay_deadline().is_none());
    }

    #[test]
    pub(super) fn overlay_deadline_is_none_without_an_active_overlay() {
        let (core, _pc, _sc, _rx, _d) = setup();
        assert!(core.overlay_deadline().is_none());
    }

    #[tokio::test]
    pub(super) async fn a_new_press_pushes_the_overlay_deadline_back() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let d1 = core.overlay_deadline().unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let d2 = core.overlay_deadline().unwrap();
        // Strictly greater: `>=` would also pass with a deadline that is
        // never pushed back (`d2 == d1`), which is exactly the defect this
        // test claims to catch. Two successive `Instant::now()` are always
        // distinct on the monotonic clocks targeted.
        assert!(d2 > d1);
    }

    #[tokio::test]
    pub(super) async fn entering_standby_clears_the_volume_overlay() {
        // Regression (review 2026-07-27): the overlay keeps priority in
        // `player_state`, so "VOLUME 65 %" stayed displayed for up to 2 s
        // after power-off before the standby word appeared.
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        assert!(matches!(state_rx.borrow_and_update().overlay, Some(Overlay::Volume { .. })));
        core.handle_command(Command::Power).await.unwrap();
        let standby = state_rx.borrow_and_update().clone();
        assert!(standby.overlay.is_none());
        assert_eq!(standby.status.as_deref(), Some("STANDBY"));
        assert!(core.overlay_deadline().is_none());
    }

    #[tokio::test]
    pub(super) async fn the_tick_does_not_arm_when_nothing_plays() {
        let (mut core, _, _, _, _dir) = setup();
        assert!(!core.tick_position(), "nothing plays: nothing to refresh");
        // Switch to `cd`, finite content: mpv has the say on its position,
        // so the tick has something to publish.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(core.tick_position(), "finite content being played: we follow its position");
        core.handle_command(Command::Stop).await.unwrap();
        assert!(!core.tick_position());
    }

    /// Case found in review: `radio` is not finite content (mpv does not
    /// provide its position) and no `metadata` plugin has set an anchor —
    /// nobody follows this stream, there is nothing to publish. Without this
    /// guard, the device would poll mpv twice a second indefinitely for a
    /// frame that deduplication systematically absorbs.
    #[tokio::test]
    pub(super) async fn a_stream_without_an_anchor_does_not_arm_the_tick() {
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(!core.tick_position(), "stream without an anchor: nothing to publish");
    }

    #[tokio::test]
    pub(super) async fn the_tick_does_not_arm_in_standby() {
        let (mut core, _, _, _, _dir) = setup();
        // Switch to `cd`, finite content: the tick has a position to publish.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(core.tick_position());
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.tick_position(), "the device is asleep");
        // The `!standby` guard is defensive: no reachable path today enters
        // standby while leaving `playback` true (`Command::Power` resets
        // both). So the state is built by hand, otherwise this test would
        // pass unchanged if the guard disappeared. `expecting_stream` stays
        // `false` (finite content) to isolate precisely the standby guard.
        core.playback = true;
        core.standby = true;
        assert!(!core.tick_position(), "standby wins, even if playback was not reset");
    }

    /// The deadline already set **survives** loop turns: that is the whole
    /// point of the fix. A relative deadline recreated at every wake of the
    /// `select!` — command, mpv event, enrichment — restarted from zero, and
    /// the tick never arrived on an active device.
    #[test]
    pub(super) fn a_set_deadline_does_not_move_on_following_turns() {
        let t0 = Instant::now();
        let set = next_deadline(true, None, t0).unwrap();
        assert_eq!(set, t0 + Duration::from_secs(1));
        // Three loop turns later, on a very busy device:
        for delay in [10, 200, 900] {
            let later = t0 + Duration::from_millis(delay);
            assert_eq!(
                next_deadline(true, Some(set), later),
                Some(set),
                "the deadline drifted by {delay} ms"
            );
        }
    }

    #[test]
    pub(super) fn disarmed_the_deadline_is_forgotten() {
        let t0 = Instant::now();
        assert_eq!(next_deadline(false, Some(t0), t0), None);
        assert_eq!(next_deadline(false, None, t0), None);
    }

    /// The rule that protects ephemeral messages: the tick republishes the
    /// state **with** the current overlay, intact, and without touching its
    /// deadline. The display decides whether to put it on top or beside; the
    /// core stays the sole master of when it disappears.
    #[tokio::test]
    pub(super) async fn a_position_refresh_leaves_the_overlay_intact() {
        let (mut core, _, _, _, _dir) = setup();
        // **Finite** content: the only case where mpv provides a position,
        // hence the only one where the refresh has something to publish.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let deadline_before = core.overlay_deadline();
        assert!(core.player_state().overlay.is_some(), "the volume overlay is there");
        core.set_progress(Some(30.0), Some(254.0));
        core.refresh_position().await;
        assert!(core.player_state().overlay.is_some(), "and it stays there");
        assert_eq!(core.overlay_deadline(), deadline_before, "its deadline has not moved");
        assert_eq!(core.player_state().position_s, Some(30));
    }

    #[tokio::test]
    pub(super) async fn an_enrichment_during_the_overlay_does_not_replace_it() {
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_command(Command::VolumeUp).await.unwrap();
        let with_overlay = state_rx.borrow_and_update().clone();
        assert!(matches!(with_overlay.overlay, Some(Overlay::Volume { .. })));

        core.handle_enrichment("ouifm", enrichment(id, "Miles Davis", "So What"));
        let during = state_rx.borrow().clone();
        assert!(matches!(during.overlay, Some(Overlay::Volume { .. })), "the volume overlay stays displayed");
        assert_eq!(during.track.title.as_deref(), Some("So What"), "but the track is already up to date underneath");
        // ... and the title stays available as soon as it expires.
        core.expire_overlay();
        assert_eq!(state_rx.borrow_and_update().track.title.as_deref(), Some("So What"));
    }

    #[tokio::test]
    pub(super) async fn volume_and_tens_overlays_have_independent_deadlines() {
        // The test that matters (brief): with two different durations, the
        // volume overlay follows `overlay_ms` and the offset one follows
        // `tens_window_ms`. This is the assertion that would fail if someone
        // recoupled the two durations behind a single field. Deadlines
        // compared to `Instant::now()`, no sleep.
        //
        // The durations are **deliberately huge** relative to what the test
        // does. With `overlay_ms: 1000` and a pivot at 2000 ms, the assertion
        // implicitly required `handle_command` to return in under one second:
        // a fast-execution assumption, hence a potential flake as soon as the
        // machine is loaded by the other test binaries. The 300 s pivot
        // between 60 s and 600 s proves exactly the same property, leaving
        // four minutes of margin to a command that takes microseconds.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(crate::state::Settings {
            overlay_ms: 60_000,
            tens_window_ms: 600_000,
            ..Default::default()
        });

        let before = Instant::now();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let volume_deadline = core.overlay_deadline().unwrap();
        assert!(
            volume_deadline < before + Duration::from_millis(300_000),
            "the volume overlay must follow overlay_ms (60 s), not tens_window_ms"
        );

        core.handle_command(Command::Plus10).await.unwrap();
        let tens_deadline = core.overlay_deadline().unwrap();
        assert!(
            tens_deadline > before + Duration::from_millis(300_000),
            "the offset overlay must follow tens_window_ms (600 s), not overlay_ms"
        );
    }

    #[tokio::test]
    pub(super) async fn volume_deadline_does_not_survive_standby() {
        // A deadline armed before standby must not let a held key step the
        // volume after waking: it has to re-press first.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(quick_settings());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65, arms the deadline
        core.handle_command(Command::Power).await.unwrap();    // standby, clears it
        core.handle_command(Command::Power).await.unwrap();    // wake
        // The absence of a deadline is asserted directly, instead of being
        // deduced from a 40 ms sleep: it is what we test, and an assertion
        // on the state depends on no clock.
        assert!(core.volume_deadline.is_none(), "standby must have cleared the deadline");
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 65, "no remaining deadline: held does nothing");
    }
}
