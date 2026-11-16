//! The equalizer as the core applies it: the fewest mpv commands per change.
//!
//! Measured on a real mpv (the prototype of the equalizer's design):
//! `af-command` on one band does not click; rewriting the chain
//! (`set_property af`) during playback clicks clearly; a jump of several dB
//! of the `volume` filter clicks; and every new file rebuilds the chain from
//! the recorded `af`, losing whatever `af-command` set.
//!
//! Hence:
//! - toggling `enabled` rewrites the chain (empty when disabled) — the only
//!   rewrite tolerated during playback, a brief cut the design accepts;
//! - otherwise, while enabled, one `af-command` per band whose gain differs
//!   from what mpv holds, and the preamp in steps of at most 1 dB, one every
//!   `RAMP_STEP`;
//! - **never a boost before its attenuation**: when the preamp must go down
//!   (more compensation), the ramp runs first and the bands follow its last
//!   step; when it goes up or stays, the bands go first. Either way the
//!   signal is never louder than the compensation allows in between;
//! - while disabled, nothing reaches mpv: the chain is empty, there is no
//!   filter to command;
//! - at each file start (`reapply_equalizer`), the whole chain is rewritten
//!   with the current gains and preamp: mpv rebuilds it then anyway, so it
//!   costs no extra click. Disabled, nothing is written — unless an earlier
//!   chain write failed (`eq_chain_stale`), and the empty chain is retried.
//!
//! The ramp is not a task of its own: like the sleep timer's fade, its
//! state lives here and `main` wakes the core at `eq_ramp_deadline`. What
//! mpv holds is tracked (`eq_preamp_sent`, `eq_bands_sent`): a new change
//! replaces the ramp, starting from the last preamp actually sent — that is
//! how a running ramp is cancelled — and keeps the running ramp's next step
//! time, so steps stay `RAMP_STEP` apart however fast the changes come.

use super::*;
use std::collections::VecDeque;

/// The interval between two preamp steps.
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
        } else if self.equalizer.enabled {
            let target = crate::equalizer::effective_preamp_db(&self.equalizer);
            let attenuating = target < self.eq_preamp_sent;
            let mut result = Ok(());
            if !attenuating {
                result = self.send_bands().await;
            }
            let steps: VecDeque<f64> = crate::equalizer::preamp_ramp(self.eq_preamp_sent, target).into();
            let next_step = match &self.eq_ramp {
                Some(running) => running.next_step,
                None => tokio::time::Instant::now(),
            };
            self.eq_ramp = (!steps.is_empty()).then_some(PreampRamp { steps, next_step, bands_after: attenuating });
            let sent = self.send_preamp_step().await;
            result.and(sent)
        } else {
            Ok(())
        };
        self.persist();
        result
    }

    /// A file is starting: rewrite the chain with the current gains and
    /// preamp, which mpv would otherwise rebuild without what `af-command`
    /// set. A running ramp and pending bands are moot then: the chain
    /// carries their targets. Disabled: nothing, unless the last chain write
    /// failed — the empty chain is then retried.
    pub async fn reapply_equalizer(&mut self) {
        if !self.equalizer.enabled && !self.eq_chain_stale {
            return;
        }
        self.eq_ramp = None;
        if let Err(e) = self.write_chain().await {
            tracing::warn!("equalizer at file start: {e}");
        }
    }

    /// Writes the whole chain of the current setting (empty when disabled)
    /// and records what mpv then holds. A failure leaves the chain stale.
    async fn write_chain(&mut self) -> Result<()> {
        let result = self.player.set_audio_filters(&crate::equalizer::af_chain(&self.equalizer)).await;
        self.eq_chain_stale = result.is_err();
        self.eq_preamp_sent = crate::equalizer::effective_preamp_db(&self.equalizer);
        self.eq_bands_sent = self.equalizer.gains;
        result
    }

    /// One `af-command` per band whose gain differs from what mpv holds.
    /// Every band is tried; the first error is returned.
    ///
    /// **The cuts go first, then the boosts.** Overlapping bands add up: a
    /// raise sent while a neighbour still holds its old boost can peak above
    /// both settings (+12 moved from 1 kHz to 500 Hz peaks several dB above
    /// +12 in between). Each band's response grows with its gain at every
    /// frequency, so after the cuts every gain is min(old, new) and during the
    /// boosts each stays at most its final value: the peak never exceeds the
    /// new setting's, which the preamp in place already compensates.
    async fn send_bands(&mut self) -> Result<()> {
        let mut result = Ok(());
        let order = (0..self.eq_bands_sent.len())
            .filter(|&b| self.equalizer.gains[b] < self.eq_bands_sent[b])
            .chain((0..self.eq_bands_sent.len()).filter(|&b| self.equalizer.gains[b] > self.eq_bands_sent[b]))
            .collect::<Vec<_>>();
        for band in order {
            let gain = self.equalizer.gains[band];
            match self.player.audio_filter_command(&crate::equalizer::band_command(band, gain)).await {
                Ok(()) => self.eq_bands_sent[band] = gain,
                Err(e) => {
                    if result.is_ok() {
                        result = Err(e);
                    }
                }
            }
        }
        result
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
    /// abandons the ramp: the next file start rewrites the whole chain.
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
    use crate::equalizer::{af_chain, effective_preamp_db, preamp_command, EqualizerSettings};
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
            assert_eq!(preamps(calls).len(), before, "a step before its 20 ms");
            tokio::time::advance(Duration::from_millis(1)).await;
            core.on_eq_ramp_deadline().await;
            assert_eq!(preamps(calls).len(), before + 1, "one step per 20 ms");
            guard += 1;
            assert!(guard < 100, "the ramp never ends");
        }
    }

    /// No two consecutive values (starting from `from`) more than 1 dB apart.
    fn assert_gentle(from: f64, values: &[f64]) {
        let mut prev = from;
        for v in values {
            assert!((v - prev).abs() <= 1.0 + 1e-9, "jump {prev} -> {v} in {values:?}");
            prev = *v;
        }
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
        let (mut core, calls, _sc, _rx, dir) = launched_with(enabled());
        let new = with_band(enabled(), 5, 6);
        core.set_equalizer(new.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;

        assert_eq!(band_calls(&calls), vec!["af_cmd eq5 g 6 equalizer".to_string()]);
        assert!(chain_calls(&calls).is_empty(), "never a chain rewrite: {:?}", calls_of(&calls));
        let target = effective_preamp_db(&new);
        assert!(target < -5.0, "{target}");
        let sent = preamps(&calls);
        assert!(sent.len() >= 6, "{sent:?}");
        assert_eq!(*sent.last().unwrap(), preamp_of(&preamp_call(target)).unwrap());
        assert_gentle(0.0, &sent);
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test(start_paused = true)]
    async fn a_boost_waits_for_its_attenuation_and_a_cut_goes_first() {
        let (mut core, calls, _sc, _rx, _dir) = launched_with(enabled());
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
        assert!(preamps(&calls).len() >= 12, "{all:?}");

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
        let (mut core, calls, _sc, _rx, _dir) = launched_with(enabled());
        core.set_equalizer(with_band(enabled(), 5, 12)).await.unwrap();
        for _ in 0..3 {
            tokio::time::advance(RAMP_STEP).await;
            core.on_eq_ramp_deadline().await;
        }
        let midway = *preamps(&calls).last().unwrap();
        assert!((-4.5..=-3.5).contains(&midway), "{midway}");

        let second = with_band(enabled(), 4, 3);
        core.set_equalizer(second.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;

        let sent = preamps(&calls);
        let target = effective_preamp_db(&second);
        assert_eq!(calls_of(&calls).last().unwrap(), &preamp_call(target));
        // The second ramp starts where the first one stood, not where the
        // first was heading.
        assert_gentle(0.0, &sent);
        assert!(sent.iter().all(|v| *v >= -4.5), "the first ramp went on: {sent:?}");
        // The pending +12 never reached mpv; the new setting's band did.
        assert_eq!(band_calls(&calls), vec!["af_cmd eq4 g 3 equalizer".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn changes_in_quick_succession_keep_steps_20_ms_apart() {
        let (mut core, calls, _sc, _rx, _dir) = launched_with(enabled());
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
        for _ in 0..500 {
            tokio::time::advance(Duration::from_millis(1)).await;
            core.on_eq_ramp_deadline().await;
            note(&calls, &mut times);
        }
        assert_eq!(core.eq_ramp_deadline(), None);
        assert!(times.len() >= 12, "{times:?}");
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
        let (mut core, calls, _sc, _rx, _dir) = launched_with(enabled());
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
        let (mut core, calls, _sc, _rx, _dir) = launched_with(start.clone());
        let new = EqualizerSettings { auto_preamp: false, ..start.clone() };
        core.set_equalizer(new).await.unwrap();
        run_ramp(&mut core, &calls).await;

        assert!(band_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert!(chain_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        let sent = preamps(&calls);
        assert_eq!(*sent.last().unwrap(), 0.0);
        assert_gentle(effective_preamp_db(&start), &sent);
        assert!(sent.len() >= 6, "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn auto_preamp_on_ramps_down_from_zero() {
        let manual = EqualizerSettings { auto_preamp: false, ..with_band(enabled(), 5, 6) };
        let (mut core, calls, _sc, _rx, _dir) = launched_with(manual.clone());
        let auto = EqualizerSettings { auto_preamp: true, ..manual };
        core.set_equalizer(auto.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;

        assert!(band_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert!(chain_calls(&calls).is_empty(), "{:?}", calls_of(&calls));
        let sent = preamps(&calls);
        assert!(sent.len() >= 6, "{sent:?}");
        assert_eq!(*sent.last().unwrap(), preamp_of(&preamp_call(effective_preamp_db(&auto))).unwrap());
        assert_gentle(0.0, &sent);
    }

    #[tokio::test(start_paused = true)]
    async fn a_player_error_mid_ramp_ends_it_and_keeps_the_setting() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(enabled());
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
        let (mut core, calls, _sc, _rx, _dir) = launched_with(with_band(enabled(), 5, 12));
        core.set_equalizer(with_band(enabled(), 4, 12)).await.unwrap();
        run_ramp(&mut core, &calls).await;
        assert_cut_before_boost(&calls);

        // Preamp going down: the bands follow the ramp's last step, in the
        // same order.
        let (mut core, calls, _sc, _rx, _dir) = launched_with(with_band(enabled(), 5, 6));
        core.set_equalizer(with_band(enabled(), 4, 12)).await.unwrap();
        run_ramp(&mut core, &calls).await;
        assert!(preamps(&calls).len() >= 6, "{:?}", calls_of(&calls));
        assert_cut_before_boost(&calls);
    }

    #[tokio::test(start_paused = true)]
    async fn file_start_rewrites_the_chain_with_current_gains() {
        let (mut core, calls, _sc, _rx, _dir) = launched_with(enabled());
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
}
