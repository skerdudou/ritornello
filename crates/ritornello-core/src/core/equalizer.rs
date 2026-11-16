//! The equalizer as the core applies it: the fewest mpv commands per change.
//!
//! Measured on a real mpv (the prototype of the equalizer's design):
//! `af-command` on one band does not click; rewriting the chain
//! (`set_property af`) during playback clicks clearly; a jump of several dB
//! of the `volume` filter clicks; every new file rebuilds the chain from the
//! recorded `af`, losing whatever `af-command` set; and an idle mpv answers
//! "error running command" to every `af-command` (there is no live chain),
//! while setting `af` works idle too.
//!
//! Hence:
//! - toggling `enabled` rewrites the chain (empty when disabled) — the only
//!   rewrite tolerated during playback, a brief cut the design accepts;
//! - otherwise, while enabled and a file is loaded, one `af-command` per
//!   band whose gain differs from what mpv holds, and the preamp in steps of
//!   at most `PREAMP_MAX_STEP_DB`, one every `RAMP_STEP`;
//! - **never a boost before its attenuation**: when the preamp must go down
//!   (more compensation), the ramp runs first and the bands follow its last
//!   step; when it goes up or stays, the bands go first. Either way the
//!   signal is never louder than the compensation allows in between;
//! - **the first player error stops the change**: a failed cut followed by
//!   boosts is sustained clipping, and every further command after an IPC
//!   timeout would stall the main loop again. What was not sent stays
//!   pending; the next change or the next file catches up;
//! - while disabled, or while no file is loaded, nothing but the setting
//!   moves: there is no filter to command;
//! - at each file start (`reapply_equalizer`), the whole chain is rewritten
//!   — **only when mpv's `af` no longer holds the current setting**
//!   (`eq_chain_written`). mpv rebuilds the chain from `af` anyway, so a
//!   rewrite is only needed after a change; and since the event crosses the
//!   core's busy loop, a rewrite can land after the audio started, where it
//!   clicks. A failed write leaves `af` unknown, and the next file retries.
//!
//! The ramp is not a task of its own: like the sleep timer's fade, its
//! state lives here and `main` wakes the core at `eq_ramp_deadline`. What
//! mpv holds is tracked (`eq_preamp_sent`, `eq_bands_sent`): a new change
//! replaces the ramp, starting from the last preamp actually sent — that is
//! how a running ramp is cancelled — and keeps the running ramp's next step
//! time, so steps stay `RAMP_STEP` apart however fast the changes come.

use super::*;
use std::collections::VecDeque;

/// The interval between two preamp steps. Its companion, the size of a
/// step, is `crate::equalizer::PREAMP_MAX_STEP_DB`.
const RAMP_STEP: Duration = Duration::from_millis(20);

/// The preamp values still to send, and when the next one is due.
#[derive(Debug, Clone)]
pub(super) struct PreampRamp {
    steps: VecDeque<f64>,
    next_step: tokio::time::Instant,
    /// The bands wait for the last step: the preamp is going down, and a
    /// boost sent before it would clip.
    bands_after: bool,
}

impl<P: Player> Core<P> {
    /// Applies a new equalizer setting and persists it. A player error is
    /// returned **after** the setting is saved: it is not lost, and the next
    /// file start rewrites the chain from it anyway.
    ///
    /// The first preamp step goes out at once when no ramp was running;
    /// otherwise at the running ramp's next step. The rest from
    /// `on_eq_ramp_deadline`.
    pub async fn set_equalizer(&mut self, new: crate::equalizer::EqualizerSettings) -> Result<()> {
        let old_enabled = self.equalizer.enabled;
        self.equalizer = new;
        let result = if old_enabled != self.equalizer.enabled {
            self.eq_ramp = None;
            self.write_chain().await
        } else if self.equalizer.enabled && self.eq_file_loaded {
            self.command_change().await
        } else {
            // Disabled, or nothing loaded: no filter to command. The next
            // file start sees `af` differ from the setting and rewrites it.
            Ok(())
        };
        self.persist();
        result
    }

    /// The live part of `set_equalizer`: bands and preamp ramp, in the order
    /// that never boosts before attenuating.
    async fn command_change(&mut self) -> Result<()> {
        let target = crate::equalizer::effective_preamp_db(&self.equalizer);
        let attenuating = target < self.eq_preamp_sent;
        if !attenuating
            && let Err(e) = self.send_bands().await
        {
            // A cut may be missing: raising the preamp could clip.
            self.eq_ramp = None;
            return Err(e);
        }
        let steps: VecDeque<f64> = crate::equalizer::preamp_ramp(self.eq_preamp_sent, target).into();
        let next_step = match &self.eq_ramp {
            Some(running) => running.next_step,
            None => tokio::time::Instant::now(),
        };
        self.eq_ramp = (!steps.is_empty()).then_some(PreampRamp { steps, next_step, bands_after: attenuating });
        self.send_preamp_step().await
    }

    /// A file is starting: mpv rebuilds the chain from `af`, dropping what
    /// `af-command` set. Rewrite `af` only if it no longer holds the current
    /// setting; either way the chain now carries the setting's targets, so a
    /// running ramp and pending bands are moot.
    pub async fn reapply_equalizer(&mut self) {
        self.eq_file_loaded = true;
        self.eq_ramp = None;
        let chain = crate::equalizer::af_chain(&self.equalizer);
        if self.eq_chain_written.as_deref() == Some(chain.as_str()) {
            self.eq_preamp_sent = crate::equalizer::effective_preamp_db(&self.equalizer);
            self.eq_bands_sent = self.equalizer.gains;
            return;
        }
        if let Err(e) = self.write_chain().await {
            tracing::warn!("equalizer at file start: {e}");
        }
    }

    /// mpv went idle: no live chain any more, so no `af-command` until the
    /// next file (`reapply_equalizer`), and no ramp to finish.
    pub(super) fn equalizer_idle(&mut self) {
        self.eq_file_loaded = false;
        self.eq_ramp = None;
    }

    /// The audio output changed: mpv may rebuild the chain from `af` when it
    /// reloads the output, dropping what `af-command` set. Rewrite it with
    /// the current setting; the change cuts the sound anyway.
    pub(super) async fn reapply_equalizer_after_output_change(&mut self) {
        if !self.equalizer.enabled {
            return;
        }
        self.eq_ramp = None;
        if let Err(e) = self.write_chain().await {
            tracing::warn!("equalizer after an audio output change: {e}");
        }
    }

    /// Writes the whole chain of the current setting (empty when disabled)
    /// and records what mpv then holds. A failure leaves `af` unknown.
    async fn write_chain(&mut self) -> Result<()> {
        let chain = crate::equalizer::af_chain(&self.equalizer);
        let result = self.player.set_audio_filters(&chain).await;
        self.eq_chain_written = result.is_ok().then_some(chain);
        self.eq_preamp_sent = crate::equalizer::effective_preamp_db(&self.equalizer);
        self.eq_bands_sent = self.equalizer.gains;
        result
    }

    /// One `af-command` per band whose gain differs from what mpv holds,
    /// **stopping at the first error**: the bands not sent stay pending
    /// (`eq_bands_sent` unchanged for them).
    ///
    /// **The cuts go first, then the boosts.** Overlapping bands add up: a
    /// raise sent while a neighbour still holds its old boost can peak above
    /// both settings (+12 moved from 1 kHz to 500 Hz peaks several dB above
    /// +12 in between). Each band's response grows with its gain at every
    /// frequency, so after the cuts every gain is min(old, new) and during the
    /// boosts each stays at most its final value: the peak never exceeds the
    /// new setting's, which the preamp in place already compensates. A cut
    /// that failed must therefore not be followed by the boosts.
    async fn send_bands(&mut self) -> Result<()> {
        let order = (0..self.eq_bands_sent.len())
            .filter(|&b| self.equalizer.gains[b] < self.eq_bands_sent[b])
            .chain((0..self.eq_bands_sent.len()).filter(|&b| self.equalizer.gains[b] > self.eq_bands_sent[b]))
            .collect::<Vec<_>>();
        for band in order {
            let gain = self.equalizer.gains[band];
            self.player.audio_filter_command(&crate::equalizer::band_command(band, gain)).await?;
            self.eq_bands_sent[band] = gain;
        }
        Ok(())
    }

    /// When `main` must wake the core for the next preamp step.
    pub fn eq_ramp_deadline(&self) -> Option<Instant> {
        self.eq_ramp.as_ref().map(|r| r.next_step.into_std())
    }

    /// The next preamp step is due. Does nothing when it is not, so a
    /// spurious wake-up is harmless.
    pub async fn on_eq_ramp_deadline(&mut self) {
        if let Err(e) = self.send_preamp_step().await {
            tracing::warn!("equalizer preamp step: {e}");
        }
    }

    /// Sends the ramp's next step if it is due, and schedules the one after;
    /// after the last one, the bands that waited for it. A failed step
    /// abandons the ramp: the next change or file start catches up.
    async fn send_preamp_step(&mut self) -> Result<()> {
        let now = tokio::time::Instant::now();
        let Some(ramp) = self.eq_ramp.as_mut() else { return Ok(()) };
        if now < ramp.next_step {
            return Ok(());
        }
        let Some(db) = ramp.steps.pop_front() else {
            self.eq_ramp = None;
            return Ok(());
        };
        ramp.next_step = now + RAMP_STEP;
        let last = ramp.steps.is_empty();
        let bands_after = ramp.bands_after;
        if let Err(e) = self.player.audio_filter_command(&crate::equalizer::preamp_command(db)).await {
            self.eq_ramp = None;
            return Err(e);
        }
        self.eq_preamp_sent = db;
        if last {
            self.eq_ramp = None;
            if bands_after {
                return self.send_bands().await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::*;
    use crate::equalizer::{af_chain, effective_preamp_db, preamp_command, preamp_ramp, EqualizerSettings, PREAMP_MAX_STEP_DB};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    fn enabled() -> EqualizerSettings {
        EqualizerSettings { enabled: true, ..Default::default() }
    }

    fn with_band(mut s: EqualizerSettings, band: usize, gain: i8) -> EqualizerSettings {
        s.gains[band] = gain;
        s
    }

    /// A core launched with `eq` in `state.json`, its player calls cleared.
    fn launched_with(eq: EqualizerSettings) -> Rig {
        let rig = setup_persisted(PersistedState { equalizer: eq, ..Default::default() });
        rig.1.lock().unwrap().clear();
        rig
    }

    /// `launched_with`, then a file started: mpv holds a live chain that
    /// `af-command` can reach (an idle mpv refuses every one).
    async fn playing_with(eq: EqualizerSettings) -> Rig {
        let mut rig = setup_persisted(PersistedState { equalizer: eq, ..Default::default() });
        rig.0.handle_event(Event::FileStarting).await;
        rig.1.lock().unwrap().clear();
        rig
    }

    fn calls_of(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    fn preamp_of(call: &str) -> Option<f64> {
        call.strip_prefix("af_cmd eqpre volume ")?.strip_suffix("dB volume")?.parse().ok()
    }

    /// The preamp values sent, in order.
    fn preamps(calls: &Mutex<Vec<String>>) -> Vec<f64> {
        calls.lock().unwrap().iter().filter_map(|c| preamp_of(c)).collect()
    }

    fn band_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.starts_with("af_cmd eq") && !c.starts_with("af_cmd eqpre")).cloned().collect()
    }

    fn chain_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.starts_with("af ")).cloned().collect()
    }

    fn preamp_call(db: f64) -> String {
        format!("af_cmd eqpre volume {} volume", preamp_command(db).arg)
    }

    /// Runs the ramp to its end, one step per `RAMP_STEP`, checking that a
    /// wake-up before a step is due sends nothing.
    async fn run_ramp(core: &mut Core<FakePlayer>, calls: &Mutex<Vec<String>>) {
        let mut guard = 0;
        while core.eq_ramp_deadline().is_some() {
            let before = preamps(calls).len();
            tokio::time::advance(RAMP_STEP - Duration::from_millis(1)).await;
            core.on_eq_ramp_deadline().await;
            assert_eq!(preamps(calls).len(), before, "a step before its RAMP_STEP");
            tokio::time::advance(Duration::from_millis(1)).await;
            core.on_eq_ramp_deadline().await;
            assert_eq!(preamps(calls).len(), before + 1, "one step per RAMP_STEP");
            guard += 1;
            assert!(guard < 1000, "the ramp never ends");
        }
    }

    /// No two consecutive values (starting from `from`) more than
    /// `PREAMP_MAX_STEP_DB` apart.
    fn assert_gentle(from: f64, values: &[f64]) {
        let mut prev = from;
        for v in values {
            assert!((v - prev).abs() <= PREAMP_MAX_STEP_DB + 1e-9, "jump {prev} -> {v} in {values:?}");
            prev = *v;
        }
    }

    /// The fewest steps a ramp from `from` to `to` can take.
    fn min_steps(from: f64, to: f64) -> usize {
        ((to - from).abs() / PREAMP_MAX_STEP_DB - 1e-9).ceil() as usize
    }

    fn disk_equalizer(dir: &tempfile::TempDir) -> EqualizerSettings {
        crate::state::load(&dir.path().join("state.json")).equalizer
    }

    #[tokio::test(start_paused = true)]
    async fn enabling_sets_the_chain() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(EqualizerSettings::default());
        let new = with_band(enabled(), 5, 6);
        core.set_equalizer(new.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
        assert_eq!(core.eq_ramp_deadline(), None, "the chain carries the preamp");
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test(start_paused = true)]
    async fn disabling_clears_the_chain() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(with_band(enabled(), 5, 6));
        let new = EqualizerSettings { enabled: false, ..with_band(enabled(), 5, 6) };
        core.set_equalizer(new.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec!["af ".to_string()]);
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test(start_paused = true)]
    async fn moving_one_band_sends_one_band_command_and_a_preamp_ramp() {
        let (mut core, calls, _sc, _rx, dir) = playing_with(enabled()).await;
        let new = with_band(enabled(), 5, 6);
        core.set_equalizer(new.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;

        assert_eq!(band_calls(&calls), vec!["af_cmd eq5 g 6 equalizer".to_string()]);
        assert!(chain_calls(&calls).is_empty(), "never a chain rewrite: {:?}", calls_of(&calls));
        let target = effective_preamp_db(&new);
        assert!(target < -5.0, "{target}");
        let sent = preamps(&calls);
        assert!(sent.len() >= min_steps(0.0, target), "{sent:?}");
        assert_eq!(*sent.last().unwrap(), preamp_of(&preamp_call(target)).unwrap());
        assert_gentle(0.0, &sent);
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test(start_paused = true)]
    async fn a_boost_waits_for_its_attenuation_and_a_cut_goes_first() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        let boosted = with_band(enabled(), 5, 12);
        core.set_equalizer(boosted.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        let all = calls_of(&calls);
        let target = effective_preamp_db(&boosted);
        // The boost only once the whole attenuation is in place.
        assert_eq!(
            all[all.len() - 2..],
            [preamp_call(target), "af_cmd eq5 g 12 equalizer".to_string()],
            "{all:?}"
        );
        assert_eq!(band_calls(&calls).len(), 1, "{all:?}");
        assert!(preamps(&calls).len() >= min_steps(0.0, target), "{all:?}");

        // Back down: the cut first, then the preamp climbs.
        calls.lock().unwrap().clear();
        core.set_equalizer(enabled()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        let all = calls_of(&calls);
        assert_eq!(all[0], "af_cmd eq5 g 0 equalizer", "{all:?}");
        assert_eq!(band_calls(&calls).len(), 1, "{all:?}");
        let sent = preamps(&calls);
        assert_eq!(*sent.last().unwrap(), 0.0);
        assert_gentle(preamp_of(&preamp_call(target)).unwrap(), &sent);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_change_cancels_the_running_ramp() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        let first = with_band(enabled(), 5, 12);
        core.set_equalizer(first.clone()).await.unwrap();
        for _ in 0..3 {
            tokio::time::advance(RAMP_STEP).await;
            core.on_eq_ramp_deadline().await;
        }
        // The fourth step of the first ramp.
        let midway = *preamps(&calls).last().unwrap();
        let expected = preamp_ramp(0.0, effective_preamp_db(&first))[3];
        assert_eq!(midway, preamp_of(&preamp_call(expected)).unwrap());

        let second = with_band(enabled(), 4, 3);
        core.set_equalizer(second.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;

        let sent = preamps(&calls);
        let target = effective_preamp_db(&second);
        assert_eq!(calls_of(&calls).last().unwrap(), &preamp_call(target));
        // The second ramp starts where the first one stood, not where the
        // first was heading.
        assert_gentle(0.0, &sent);
        assert!(sent.iter().all(|v| *v >= midway), "the first ramp went on: {sent:?}");
        // The pending +12 never reached mpv; the new setting's band did.
        assert_eq!(band_calls(&calls), vec!["af_cmd eq4 g 3 equalizer".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn changes_in_quick_succession_keep_steps_ramp_step_apart() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        let start = tokio::time::Instant::now();
        let mut times: Vec<Duration> = Vec::new();
        let note = |calls: &Mutex<Vec<String>>, times: &mut Vec<Duration>| {
            for _ in times.len()..preamps(calls).len() {
                times.push(tokio::time::Instant::now() - start);
            }
        };
        core.set_equalizer(with_band(enabled(), 5, 6)).await.unwrap();
        note(&calls, &mut times);
        tokio::time::advance(Duration::from_millis(5)).await;
        core.set_equalizer(with_band(enabled(), 5, 12)).await.unwrap();
        note(&calls, &mut times);
        let target = effective_preamp_db(&with_band(enabled(), 5, 12));
        for _ in 0..(min_steps(0.0, target) + 2) as u128 * RAMP_STEP.as_millis() {
            tokio::time::advance(Duration::from_millis(1)).await;
            core.on_eq_ramp_deadline().await;
            note(&calls, &mut times);
        }
        assert_eq!(core.eq_ramp_deadline(), None);
        assert!(times.len() >= min_steps(0.0, target), "{times:?}");
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= RAMP_STEP, "steps too close: {times:?}");
        }
        assert_gentle(0.0, &preamps(&calls));
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_changes_touch_no_player_but_persist() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(EqualizerSettings::default());
        let new = with_band(EqualizerSettings { auto_preamp: false, ..Default::default() }, 2, -7);
        core.set_equalizer(new.clone()).await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        core.on_eq_ramp_deadline().await;
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert_eq!(core.eq_ramp_deadline(), None);
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test(start_paused = true)]
    async fn disabling_mid_ramp_stops_the_ramp() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        core.set_equalizer(with_band(enabled(), 5, 12)).await.unwrap();
        tokio::time::advance(RAMP_STEP).await;
        core.on_eq_ramp_deadline().await;
        assert_eq!(preamps(&calls).len(), 2);
        core.set_equalizer(EqualizerSettings { enabled: false, ..with_band(enabled(), 5, 12) }).await.unwrap();
        for _ in 0..50 {
            tokio::time::advance(RAMP_STEP).await;
            core.on_eq_ramp_deadline().await;
        }
        let all = calls_of(&calls);
        assert_eq!(all.last().unwrap(), "af ", "{all:?}");
        assert_eq!(core.eq_ramp_deadline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn auto_preamp_toggle_ramps_preamp_only() {
        let start = with_band(enabled(), 5, 6);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(start.clone()).await;
        let new = EqualizerSettings { auto_preamp: false, ..start.clone() };
        core.set_equalizer(new).await.unwrap();
        run_ramp(&mut core, &calls).await;

        assert!(band_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert!(chain_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        let sent = preamps(&calls);
        assert_eq!(*sent.last().unwrap(), 0.0);
        assert_gentle(effective_preamp_db(&start), &sent);
        assert!(sent.len() >= min_steps(effective_preamp_db(&start), 0.0), "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn auto_preamp_on_ramps_down_from_zero() {
        let manual = EqualizerSettings { auto_preamp: false, ..with_band(enabled(), 5, 6) };
        let (mut core, calls, _sc, _rx, _dir) = playing_with(manual.clone()).await;
        let auto = EqualizerSettings { auto_preamp: true, ..manual };
        core.set_equalizer(auto.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;

        assert!(band_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert!(chain_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        let sent = preamps(&calls);
        assert!(sent.len() >= min_steps(0.0, effective_preamp_db(&auto)), "{sent:?}");
        assert_eq!(*sent.last().unwrap(), preamp_of(&preamp_call(effective_preamp_db(&auto))).unwrap());
        assert_gentle(0.0, &sent);
    }

    #[tokio::test(start_paused = true)]
    async fn a_player_error_mid_ramp_ends_it_and_keeps_the_setting() {
        let (mut core, calls, _sc, _rx, dir) = playing_with(enabled()).await;
        let new = with_band(enabled(), 5, 12);
        core.set_equalizer(new.clone()).await.unwrap();
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        tokio::time::advance(RAMP_STEP).await;
        core.on_eq_ramp_deadline().await;
        assert_eq!(core.eq_ramp_deadline(), None);
        let n = calls_of(&calls).len();
        tokio::time::advance(Duration::from_secs(1)).await;
        core.on_eq_ramp_deadline().await;
        assert_eq!(calls_of(&calls).len(), n, "nothing more after the failure");
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_disable_is_repaired_at_the_next_file_start() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(with_band(enabled(), 5, 6));
        core.player.af_fails.store(true, Ordering::SeqCst);
        let off = EqualizerSettings { enabled: false, ..with_band(enabled(), 5, 6) };
        assert!(core.set_equalizer(off.clone()).await.is_err());
        assert_eq!(disk_equalizer(&dir), off, "saved despite the error");

        core.player.af_fails.store(false, Ordering::SeqCst);
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec!["af ".to_string()]);
        // Repaired: a healthy disabled equalizer costs nothing per file.
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
    }

    /// Two overlapping octave-wide bands both at +12 peak well above +12:
    /// the cut must reach mpv before the boost, or the preamp in place is
    /// not enough for the time between the two commands.
    fn assert_cut_before_boost(calls: &Mutex<Vec<String>>) {
        let all = calls_of(calls);
        let at = |c: &str| all.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c} missing: {all:?}"));
        assert!(at("af_cmd eq5 g 0 equalizer") < at("af_cmd eq4 g 12 equalizer"), "{all:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn band_cuts_reach_mpv_before_band_boosts() {
        // Preamp unchanged (≈ −12 both sides): the bands go at once.
        let (mut core, calls, _sc, _rx, _dir) = playing_with(with_band(enabled(), 5, 12)).await;
        core.set_equalizer(with_band(enabled(), 4, 12)).await.unwrap();
        run_ramp(&mut core, &calls).await;
        assert_cut_before_boost(&calls);

        // Preamp going down: the bands follow the ramp's last step, in the
        // same order.
        let (mut core, calls, _sc, _rx, _dir) = playing_with(with_band(enabled(), 5, 6)).await;
        core.set_equalizer(with_band(enabled(), 4, 12)).await.unwrap();
        run_ramp(&mut core, &calls).await;
        let (from, to) = (effective_preamp_db(&with_band(enabled(), 5, 6)), effective_preamp_db(&with_band(enabled(), 4, 12)));
        assert!(preamps(&calls).len() >= min_steps(from, to), "{:?}", calls_of(&calls));
        assert_cut_before_boost(&calls);
    }

    #[tokio::test(start_paused = true)]
    async fn file_start_rewrites_the_chain_with_current_gains() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        let current = with_band(with_band(enabled(), 5, 6), 0, -3);
        core.set_equalizer(current.clone()).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&current))]);
        assert_eq!(core.eq_ramp_deadline(), None, "the chain carries the target preamp");

        // A later change ramps from the chain's preamp, not from where the
        // abandoned ramp stood, and sends only the band that moved since.
        calls.lock().unwrap().clear();
        let next = with_band(current.clone(), 0, -2);
        core.set_equalizer(next.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        assert_gentle(effective_preamp_db(&current), &preamps(&calls));
        assert_eq!(band_calls(&calls), vec!["af_cmd eq0 g -2 equalizer".to_string()]);

        let (mut core, calls, _sc, _rx, _dir) = launched_with(EqualizerSettings::default());
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
    }

    /// An idle mpv answers "error running command" to every `af-command`
    /// (measured): adjusting the equalizer with nothing playing used to
    /// return that error, logged as a warning into "Recent errors".
    #[tokio::test(start_paused = true)]
    async fn an_idle_mpv_gets_no_band_command_and_the_next_file_carries_the_change() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(enabled());
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        let new = with_band(enabled(), 5, 6);
        core.set_equalizer(new.clone()).await.unwrap();
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert_eq!(core.eq_ramp_deadline(), None, "no ramp on an idle mpv");
        assert_eq!(disk_equalizer(&dir), new);

        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
    }

    #[tokio::test(start_paused = true)]
    async fn playback_idle_ends_the_ramp_and_the_commands_until_the_next_file() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        core.set_equalizer(with_band(enabled(), 5, 12)).await.unwrap();
        assert!(core.eq_ramp_deadline().is_some());
        core.handle_event(Event::PlaybackIdle).await;
        assert_eq!(core.eq_ramp_deadline(), None, "a ramp on an idle mpv only fails");

        calls.lock().unwrap().clear();
        let new = with_band(enabled(), 4, 3);
        core.set_equalizer(new.clone()).await.unwrap();
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
    }

    /// A rewrite crossing the core's busy loop can land after the audio
    /// started, and then it clicks: mpv's own `af` already holds the right
    /// chain unless the setting moved since it was last written.
    #[tokio::test(start_paused = true)]
    async fn a_file_start_rewrites_only_a_chain_that_changed() {
        let start = with_band(enabled(), 5, 6);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(start.clone()).await;
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "nothing changed: {:?}", calls_of(&calls));

        let next = with_band(start.clone(), 0, -3);
        core.set_equalizer(next.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&next))]);
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "written once: {:?}", calls_of(&calls));

        // A change undone within the file: mpv's `af` still holds the
        // setting, and the next file needs nothing.
        core.set_equalizer(start.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        core.set_equalizer(next.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        // …and a later change ramps from the setting the file started with.
        core.set_equalizer(start.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        assert_eq!(band_calls(&calls), vec!["af_cmd eq0 g 0 equalizer".to_string()]);
        assert_gentle(effective_preamp_db(&next), &preamps(&calls));
    }

    /// A ramp cut short by a file start whose `af` already holds the
    /// setting: mpv rebuilds the chain at the setting's preamp, and the next
    /// change must ramp from there, not from where the abandoned ramp stood.
    #[tokio::test(start_paused = true)]
    async fn a_file_start_without_rewrite_counts_the_setting_as_sent() {
        let start = with_band(enabled(), 5, 6);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(start.clone()).await;
        // Out and back before the ramp ends: the preamp was left low.
        core.set_equalizer(with_band(enabled(), 5, 12)).await.unwrap();
        for _ in 0..3 {
            tokio::time::advance(RAMP_STEP).await;
            core.on_eq_ramp_deadline().await;
        }
        let low = *preamps(&calls).last().unwrap();
        assert!(low < effective_preamp_db(&start) - 2.0 * PREAMP_MAX_STEP_DB, "{low}");
        core.set_equalizer(start.clone()).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "af already holds the setting: {:?}", calls_of(&calls));

        core.set_equalizer(with_band(start.clone(), 0, -3)).await.unwrap();
        run_ramp(&mut core, &calls).await;
        assert_gentle(effective_preamp_db(&start), &preamps(&calls));
    }

    /// A failed cut followed by boosts is sustained clipping, and each
    /// further command after a 5 s IPC timeout stalls the main loop again.
    #[tokio::test(start_paused = true)]
    async fn a_failed_band_command_stops_the_change_and_the_next_file_repairs_it() {
        let (mut core, calls, _sc, _rx, dir) = playing_with(with_band(enabled(), 5, 12)).await;
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        // Less boost: the preamp would climb after the bands.
        let new = with_band(enabled(), 4, 6);
        assert!(effective_preamp_db(&new) > effective_preamp_db(&with_band(enabled(), 5, 12)));
        assert!(core.set_equalizer(new.clone()).await.is_err());
        assert_eq!(calls_of(&calls), vec!["af_cmd eq5 g 0 equalizer".to_string()], "nothing after the failed cut");
        assert_eq!(core.eq_ramp_deadline(), None);
        assert_eq!(disk_equalizer(&dir), new);

        core.player.af_cmd_fails.store(false, Ordering::SeqCst);
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
    }
}
