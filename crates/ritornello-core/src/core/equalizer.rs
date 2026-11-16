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
//! - otherwise, while enabled, one `af-command` per band whose gain moved,
//!   then the preamp in steps of at most 1 dB, one every `RAMP_STEP`;
//! - while disabled, nothing reaches mpv at all: the chain is empty, there is
//!   no filter to command;
//! - at each file start (`reapply_equalizer`), the whole chain is rewritten
//!   with the current gains and preamp: mpv rebuilds it then anyway, so it
//!   costs no extra click.
//!
//! The ramp is not a task of its own: like the sleep timer's fade, its
//! state lives here and `main` wakes the core at `eq_ramp_deadline`. A new
//! change replaces that state, starting from the last preamp actually sent
//! (`eq_preamp_sent`) — that is how a running ramp is cancelled.

use super::*;
use std::collections::VecDeque;

/// The interval between two preamp steps.
const RAMP_STEP: Duration = Duration::from_millis(20);

/// The preamp values still to send, and when the next one is due.
#[derive(Debug, Clone)]
pub(super) struct PreampRamp {
    steps: VecDeque<f64>,
    next_step: tokio::time::Instant,
}

impl<P: Player> Core<P> {
    /// Applies a new equalizer setting and persists it. A player error is
    /// returned **after** the setting is saved: it is not lost, and the next
    /// file start rewrites the chain from it anyway.
    ///
    /// The first preamp step goes out at once; the rest from
    /// `on_eq_ramp_deadline`.
    #[cfg_attr(not(test), expect(dead_code, reason = "called by the HTTP route, not wired yet"))]
    pub async fn set_equalizer(&mut self, new: crate::equalizer::EqualizerSettings) -> Result<()> {
        let old = std::mem::replace(&mut self.equalizer, new.clone());
        let mut result = Ok(());
        if old.enabled != new.enabled {
            self.eq_ramp = None;
            self.eq_preamp_sent = crate::equalizer::effective_preamp_db(&new);
            result = self.player.set_audio_filters(&crate::equalizer::af_chain(&new)).await;
        } else if new.enabled {
            for (band, (&was, &now)) in old.gains.iter().zip(&new.gains).enumerate() {
                if was != now {
                    let sent = self.player.audio_filter_command(&crate::equalizer::band_command(band, now)).await;
                    if result.is_ok() {
                        result = sent;
                    }
                }
            }
            let target = crate::equalizer::effective_preamp_db(&new);
            let steps: VecDeque<f64> = crate::equalizer::preamp_ramp(self.eq_preamp_sent, target).into();
            self.eq_ramp = (!steps.is_empty())
                .then(|| PreampRamp { steps, next_step: tokio::time::Instant::now() });
            let sent = self.send_preamp_step().await;
            if result.is_ok() {
                result = sent;
            }
        }
        self.persist();
        result
    }

    /// A file is starting: rewrite the chain with the current gains and
    /// preamp, which mpv would otherwise rebuild without what `af-command`
    /// set. A running ramp is moot then: the chain carries its target.
    /// Disabled: nothing, the chain is already empty.
    pub async fn reapply_equalizer(&mut self) {
        if !self.equalizer.enabled {
            return;
        }
        self.eq_ramp = None;
        self.eq_preamp_sent = crate::equalizer::effective_preamp_db(&self.equalizer);
        let chain = crate::equalizer::af_chain(&self.equalizer);
        if let Err(e) = self.player.set_audio_filters(&chain).await {
            tracing::warn!("equalizer at file start: {e}");
        }
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

    /// Sends the ramp's next step if it is due, and schedules the one after.
    /// A failed step abandons the ramp: the next file start rewrites the
    /// whole chain anyway.
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
        if ramp.steps.is_empty() {
            self.eq_ramp = None;
        }
        match self.player.audio_filter_command(&crate::equalizer::preamp_command(db)).await {
            Ok(()) => {
                self.eq_preamp_sent = db;
                Ok(())
            }
            Err(e) => {
                self.eq_ramp = None;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::*;
    use crate::equalizer::{af_chain, effective_preamp_db, preamp_command, EqualizerSettings};
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

    /// The preamp values sent, in order.
    fn preamps(calls: &Mutex<Vec<String>>) -> Vec<f64> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| c.strip_prefix("af_cmd eqpre volume ")?.strip_suffix("dB volume")?.parse().ok())
            .collect()
    }

    fn band_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.starts_with("af_cmd eq") && !c.starts_with("af_cmd eqpre")).cloned().collect()
    }

    fn chain_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.starts_with("af ")).cloned().collect()
    }

    /// Runs the ramp to its end, one step per `RAMP_STEP`, checking that a
    /// wake-up before a step is due sends nothing.
    async fn run_ramp(core: &mut Core<FakePlayer>, calls: &Mutex<Vec<String>>) {
        let mut guard = 0;
        while core.eq_ramp_deadline().is_some() {
            let before = calls.lock().unwrap().len();
            tokio::time::advance(RAMP_STEP - Duration::from_millis(1)).await;
            core.on_eq_ramp_deadline().await;
            assert_eq!(calls.lock().unwrap().len(), before, "a step before its 20 ms");
            tokio::time::advance(Duration::from_millis(1)).await;
            core.on_eq_ramp_deadline().await;
            assert_eq!(calls.lock().unwrap().len(), before + 1, "one step per 20 ms");
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
        assert_eq!(calls_of(&calls)[0], "af_cmd eq5 g 6 equalizer");
        run_ramp(&mut core, &calls).await;

        assert_eq!(band_calls(&calls), vec!["af_cmd eq5 g 6 equalizer".to_string()]);
        assert!(chain_calls(&calls).is_empty(), "never a chain rewrite: {:?}", calls_of(&calls));
        let target = effective_preamp_db(&new);
        assert!(target < -5.0, "{target}");
        let sent = preamps(&calls);
        assert!(sent.len() >= 6, "{sent:?}");
        assert_eq!(calls_of(&calls).last().unwrap(), &format!("af_cmd eqpre volume {} volume", preamp_command(target).arg));
        assert_gentle(0.0, &sent);
        assert_eq!(disk_equalizer(&dir), new);
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
        assert_eq!(calls_of(&calls).last().unwrap(), &format!("af_cmd eqpre volume {} volume", preamp_command(target).arg));
        // The second ramp starts where the first one stood, not where the
        // first was heading.
        assert_gentle(0.0, &sent);
        assert!(sent.iter().all(|v| *v >= -4.5), "the first ramp went on: {sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_changes_touch_no_player_but_persist() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(EqualizerSettings::default());
        let new = with_band(EqualizerSettings { auto_preamp: false, ..Default::default() }, 2, -7);
        core.set_equalizer(new.clone()).await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        core.on_eq_ramp_deadline().await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert_eq!(core.eq_ramp_deadline(), None);
        assert_eq!(disk_equalizer(&dir), new);
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
    async fn file_start_rewrites_the_chain_with_current_gains() {
        let (mut core, calls, _sc, _rx, _dir) = launched_with(enabled());
        let current = with_band(with_band(enabled(), 5, 6), 0, -3);
        core.set_equalizer(current.clone()).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&current))]);
        assert_eq!(core.eq_ramp_deadline(), None, "the chain carries the target preamp");

        // A later change ramps from the chain's preamp, not from where the
        // abandoned ramp stood.
        calls.lock().unwrap().clear();
        let next = with_band(current.clone(), 0, -2);
        core.set_equalizer(next.clone()).await.unwrap();
        run_ramp(&mut core, &calls).await;
        assert_gentle(effective_preamp_db(&current), &preamps(&calls));

        let (mut core, calls, _sc, _rx, _dir) = launched_with(EqualizerSettings::default());
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
    }
}
