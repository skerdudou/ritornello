//! The sleep timer: a countdown to standby, ended at once or by a fade.
//!
//! One timer at a time, never persisted (a restart loses it, by the owner's
//! decision). At the deadline the device goes to standby exactly as the
//! Power key puts it there (`enter_standby`); `Fade` first ramps mpv's
//! volume down to zero.
//!
//! **The volume is never left low.** The fade steps write to mpv directly
//! (`Player::set_volume`), never through `Core::set_volume`, which would
//! persist each step and show the overlay: `self.volume`, the persisted
//! value, stays the volume to restore throughout. Every way out of a fade —
//! its end, Cancel, Extend, a new timer, Pause, Stop, Power — sets mpv back
//! to that volume.
//!
//! Deadlines are `tokio::time::Instant`s, like `last_pass_reopen` and for the
//! same reason: `main` waits on them with `tokio::time::sleep_until`, and the
//! tests drive them with tokio's paused clock.

use super::*;
use ritornello_proto::{SleepMode, SleepPhase, SleepTimerState};

/// A fade lasts `FADE_STEPS × FADE_STEP` = 10 s.
const FADE_STEPS: u8 = 20;
const FADE_STEP: Duration = Duration::from_millis(500);
/// What the SPA's "+15 min" sends. The core accepts any value in
/// `ExtendSleepTimer`; this pins the step the menu is designed around.
#[cfg_attr(not(test), expect(dead_code, reason = "the step the SPA sends, pinned by the tests"))]
const EXTEND_MINUTES: u16 = 15;

/// A running sleep timer.
#[derive(Debug, Clone, Copy)]
pub(super) struct SleepTimer {
    mode: SleepMode,
    phase: Phase,
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    /// Counting down. `ends_at_unix_s` is the same instant on the wall clock,
    /// for the displays.
    Armed { deadline: tokio::time::Instant, ends_at_unix_s: u64 },
    /// Ramping mpv's volume from `from` down to zero, one step every
    /// `FADE_STEP`. `step` is the number of steps already applied.
    ///
    /// `from` is the level the ramp started at, fixed for the whole fade;
    /// `restore_volume` is the volume to set back once in standby, which a
    /// volume key moves during the fade (`retarget_fade`). Two fields because
    /// the ramp must never climb back up when the user raises the volume to
    /// restore.
    Fading { next_step: tokio::time::Instant, step: u8, from: u8, restore_volume: u8, ends_at_unix_s: u64 },
}

impl SleepTimer {
    fn armed(minutes: u16, mode: SleepMode) -> Self {
        let span = minutes_span(minutes);
        SleepTimer {
            mode,
            phase: Phase::Armed {
                deadline: tokio::time::Instant::now() + span,
                ends_at_unix_s: crate::update::now_unix_s() + span.as_secs(),
            },
        }
    }

    /// The volume mpv must get back, if this timer was fading.
    fn restore_volume(&self) -> Option<u8> {
        match self.phase {
            Phase::Fading { restore_volume, .. } => Some(restore_volume),
            Phase::Armed { .. } => None,
        }
    }
}

fn minutes_span(minutes: u16) -> Duration {
    Duration::from_secs(u64::from(minutes) * 60)
}

/// mpv's level after `step` steps of a fade that started at `from`: a
/// straight line down to zero, reached at the last step.
fn fade_level(from: u8, step: u8) -> u8 {
    let left = u16::from(FADE_STEPS.saturating_sub(step));
    (u16::from(from) * left / u16::from(FADE_STEPS)) as u8
}

impl<P: Player> Core<P> {
    /// When `main` must wake the core for the timer: the countdown's deadline,
    /// or the next step of a fade.
    pub fn sleep_deadline(&self) -> Option<Instant> {
        self.sleep.map(|t| match t.phase {
            Phase::Armed { deadline, .. } => deadline.into_std(),
            Phase::Fading { next_step, .. } => next_step.into_std(),
        })
    }

    /// The remembered menu choice, for `main` to keep the HTTP copy of the
    /// settings in step with the core's (`settings_current`).
    pub fn sleep_timer_mode(&self) -> SleepMode {
        self.settings.sleep_timer_mode
    }

    /// The timer's deadline (or a fade step) has come. Does nothing when it
    /// has not, so a spurious wake-up is harmless.
    pub async fn on_sleep_deadline(&mut self) {
        let Some(timer) = self.sleep else { return };
        let now = tokio::time::Instant::now();
        match timer.phase {
            Phase::Armed { deadline, .. } if now >= deadline => {
                // Already paused or stopped: nothing to fade, standby at once
                // whatever the mode.
                let playing = self.playback && !self.paused;
                match timer.mode {
                    _ if !playing => self.sleep_into_standby().await,
                    SleepMode::Immediate => self.sleep_into_standby().await,
                    // Task 3: `EndOfTrack` waits for the track to end first.
                    SleepMode::Fade | SleepMode::EndOfTrack => {
                        self.sleep = Some(SleepTimer {
                            mode: timer.mode,
                            phase: Phase::Fading {
                                next_step: now + FADE_STEP,
                                step: 0,
                                from: self.volume,
                                restore_volume: self.volume,
                                ends_at_unix_s: crate::update::now_unix_s()
                                    + (FADE_STEP * u32::from(FADE_STEPS)).as_secs(),
                            },
                        });
                    }
                }
            }
            Phase::Fading { next_step, step, from, restore_volume, ends_at_unix_s } if now >= next_step => {
                let step = step + 1;
                // Straight to mpv: see the module's doc.
                if let Err(e) = self.player.set_volume(fade_level(from, step)).await {
                    tracing::warn!("sleep timer fade step: {e}");
                }
                if step >= FADE_STEPS {
                    self.sleep_into_standby().await;
                } else {
                    self.sleep = Some(SleepTimer {
                        mode: timer.mode,
                        phase: Phase::Fading { next_step: now + FADE_STEP, step, from, restore_volume, ends_at_unix_s },
                    });
                }
            }
            _ => return,
        }
        self.publish_state();
    }

    async fn sleep_into_standby(&mut self) {
        if let Err(e) = self.enter_standby().await {
            tracing::warn!("sleep timer standby: {e}");
        }
    }

    /// Arms a timer `minutes` from now, replacing any running one — and
    /// setting mpv back to the volume to restore if that one was fading.
    pub(super) async fn arm_sleep_timer(&mut self, minutes: u16, mode: SleepMode) {
        self.restore_after_fade(self.sleep).await;
        self.sleep = Some(SleepTimer::armed(minutes, mode));
    }

    /// Pushes the deadline back by `minutes`. A fade is abandoned — volume
    /// restored — and the timer re-armed from now. No timer: nothing.
    pub(super) async fn extend_sleep_timer(&mut self, minutes: u16) {
        let Some(timer) = self.sleep else { return };
        match timer.phase {
            Phase::Armed { deadline, ends_at_unix_s } => {
                let span = minutes_span(minutes);
                self.sleep = Some(SleepTimer {
                    mode: timer.mode,
                    phase: Phase::Armed { deadline: deadline + span, ends_at_unix_s: ends_at_unix_s + span.as_secs() },
                });
            }
            Phase::Fading { .. } => self.arm_sleep_timer(minutes, timer.mode).await,
        }
    }

    /// Clears the timer, setting mpv back to the volume to restore if it was
    /// fading. Harmless without a timer.
    pub(super) async fn cancel_sleep_timer(&mut self) {
        let timer = self.sleep.take();
        self.restore_after_fade(timer).await;
    }

    async fn restore_after_fade(&mut self, timer: Option<SleepTimer>) {
        if let Some(v) = timer.and_then(|t| t.restore_volume())
            && let Err(e) = self.player.set_volume(v).await
        {
            tracing::warn!("sleep timer volume restore: {e}");
        }
    }

    /// Clears the timer and returns the volume mpv must get back, if it was
    /// fading. For `enter_standby`, which restores it once the player is
    /// stopped.
    pub(super) fn take_sleep_timer(&mut self) -> Option<u8> {
        self.sleep.take().and_then(|t| t.restore_volume())
    }

    /// During a fade, a new volume (key, `SetVolume`, MPD) becomes the volume
    /// to restore, and mpv is left to the fade. True when it was taken here.
    pub(super) fn retarget_fade(&mut self, v: u8) -> bool {
        match &mut self.sleep {
            Some(SleepTimer { phase: Phase::Fading { restore_volume, .. }, .. }) => {
                *restore_volume = v;
                true
            }
            _ => false,
        }
    }

    /// Pause and Stop during a fade send the device to standby at once
    /// (Power does too, through its own arm). Every other command applies as
    /// usual and the fade goes on — the owner's decision: only the timer's
    /// own menu stops an extinction.
    pub(super) fn ends_fade(&self, cmd: &Command) -> bool {
        matches!(self.sleep, Some(SleepTimer { phase: Phase::Fading { .. }, .. }))
            && matches!(cmd, Command::PlayPause | Command::Stop)
    }

    /// The timer as displays see it.
    pub(super) fn sleep_timer_state(&self) -> Option<SleepTimerState> {
        self.sleep.map(|t| {
            let (phase, ends_at_unix_s) = match t.phase {
                Phase::Armed { ends_at_unix_s, .. } => (SleepPhase::Armed, ends_at_unix_s),
                Phase::Fading { ends_at_unix_s, .. } => (SleepPhase::Fading, ends_at_unix_s),
            };
            SleepTimerState { ends_at_unix_s, mode: t.mode, phase }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::*;
    use std::sync::Mutex;

    /// A core playing the radio at `volume`.
    async fn playing_at(volume: u8) -> Rig {
        let rig = setup_persisted(PersistedState { volume, ..Default::default() });
        let (mut core, calls, sc, rx, dir) = rig;
        core.resume().await.unwrap();
        assert!(core.playback && !core.paused);
        calls.lock().unwrap().clear();
        (core, calls, sc, rx, dir)
    }

    /// Every volume mpv was given, in order.
    fn volumes(calls: &Mutex<Vec<String>>) -> Vec<u8> {
        calls.lock().unwrap().iter().filter_map(|c| c.strip_prefix("vol ")?.parse().ok()).collect()
    }

    fn phase(core: &Core<FakePlayer>) -> Option<SleepPhase> {
        core.sleep_timer_state().map(|s| s.phase)
    }

    fn disk_volume(dir: &tempfile::TempDir) -> u8 {
        crate::state::load(&dir.path().join("state.json")).volume
    }

    async fn arm(core: &mut Core<FakePlayer>, minutes: u16, mode: SleepMode) {
        core.handle_command(Command::SetSleepTimer { minutes, mode }).await.unwrap();
    }

    /// Advances the clock by `span` and lets the core see its deadline.
    async fn after(core: &mut Core<FakePlayer>, span: Duration) {
        tokio::time::advance(span).await;
        core.on_sleep_deadline().await;
    }

    /// Arms a one-minute fade and reaches its deadline.
    async fn fading(core: &mut Core<FakePlayer>) {
        arm(core, 1, SleepMode::Fade).await;
        after(core, Duration::from_secs(60)).await;
        assert_eq!(phase(core), Some(SleepPhase::Fading));
    }

    async fn fade_steps(core: &mut Core<FakePlayer>, n: u8) {
        for _ in 0..n {
            after(core, FADE_STEP).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_mode_enters_standby_at_deadline() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        arm(&mut core, 1, SleepMode::Immediate).await;
        // A wake-up before the deadline does nothing.
        after(&mut core, Duration::from_secs(59)).await;
        assert!(!core.standby);
        assert_eq!(phase(&core), Some(SleepPhase::Armed));
        after(&mut core, Duration::from_secs(1)).await;
        assert!(core.standby);
        assert_eq!(core.sleep_timer_state(), None);
        assert_eq!(core.sleep_deadline(), None);
        assert!(volumes(&calls).is_empty(), "no fade: {:?}", calls.lock().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn fade_steps_mpv_volume_down_and_restores_it() {
        let (mut core, calls, _sc, _rx, dir) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 1).await;
        // A wake-up before the next step does nothing.
        core.on_sleep_deadline().await;
        assert_eq!(volumes(&calls).len(), 1);
        fade_steps(&mut core, 9).await;
        // Mid-fade, the persisted volume is still the one to restore.
        assert_eq!(core.volume, 40);
        assert_eq!(disk_volume(&dir), 40);
        assert!(!core.standby);
        fade_steps(&mut core, 9).await;
        assert!(!core.standby, "not before the twentieth step");
        fade_steps(&mut core, 1).await;
        assert!(core.standby);
        assert_eq!(core.sleep_timer_state(), None);
        let v = volumes(&calls);
        let (restore, ramp) = v.split_last().unwrap();
        assert_eq!(ramp.len(), usize::from(FADE_STEPS), "{v:?}");
        assert!(ramp.windows(2).all(|w| w[0] > w[1]), "strictly decreasing: {v:?}");
        assert_eq!(ramp.last(), Some(&0));
        assert_eq!(*restore, 40);
        assert_eq!(core.volume, 40);
        assert_eq!(disk_volume(&dir), 40);
        // mpv stopped before it got its volume back.
        let log = calls.lock().unwrap().clone();
        let stop = log.iter().position(|c| c == "stop").unwrap();
        assert_eq!(log.last().map(String::as_str), Some("vol 40"), "{log:?}");
        assert!(stop < log.len() - 1);
    }

    #[tokio::test(start_paused = true)]
    async fn volume_key_during_fade_changes_restored_volume_only() {
        let (mut core, calls, _sc, _rx, dir) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 5).await;
        core.handle_command(Command::VolumeUp).await.unwrap();
        // Persisted at once: a restart mid-fade wakes at the new volume.
        assert_eq!(disk_volume(&dir), 45);
        fade_steps(&mut core, 15).await;
        assert!(core.standby);
        let v = volumes(&calls);
        let (restore, ramp) = v.split_last().unwrap();
        assert!(ramp.windows(2).all(|w| w[0] > w[1]), "mpv never went up during the fade: {v:?}");
        assert_eq!(ramp.len(), usize::from(FADE_STEPS), "the key reached mpv: {v:?}");
        assert_eq!(*restore, 45);
        assert_eq!(disk_volume(&dir), 45);
    }

    #[tokio::test(start_paused = true)]
    async fn pause_during_fade_enters_standby_at_restore_volume() {
        // Pause, Stop and Power alike: each is its own branch.
        for cmd in [Command::PlayPause, Command::Stop, Command::Power] {
            let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
            fading(&mut core).await;
            fade_steps(&mut core, 6).await;
            core.handle_command(cmd.clone()).await.unwrap();
            assert!(core.standby, "{cmd:?}");
            assert_eq!(core.sleep_timer_state(), None, "{cmd:?}");
            let log = calls.lock().unwrap().clone();
            assert!(!log.contains(&"pause".to_string()), "{cmd:?}: {log:?}");
            assert_eq!(log.last().map(String::as_str), Some("vol 40"), "{cmd:?}: {log:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn source_switch_during_fade_keeps_fading() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 4).await;
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        assert_eq!(core.active_source, "cd");
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
        fade_steps(&mut core, 16).await;
        assert!(core.standby);
        let v = volumes(&calls);
        let (restore, ramp) = v.split_last().unwrap();
        assert_eq!(ramp.len(), usize::from(FADE_STEPS), "{v:?}");
        assert!(ramp.windows(2).all(|w| w[0] > w[1]), "{v:?}");
        assert_eq!(*restore, 40);
    }

    #[tokio::test(start_paused = true)]
    async fn mute_during_fade_keeps_fading() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 5).await;
        core.handle_command(Command::Mute).await.unwrap();
        assert!(core.muted);
        assert!(calls.lock().unwrap().contains(&"mute true".to_string()));
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
        fade_steps(&mut core, 15).await;
        assert!(core.standby);
        let v = volumes(&calls);
        let (restore, ramp) = v.split_last().unwrap();
        assert_eq!(ramp.len(), usize::from(FADE_STEPS), "{v:?}");
        assert!(ramp.windows(2).all(|w| w[0] > w[1]), "{v:?}");
        assert_eq!(*restore, 40);
    }

    #[tokio::test(start_paused = true)]
    async fn timer_standby_drops_a_pending_tens_offset() {
        // The deadline's standby does not pass through `apply_command`, whose
        // abandon guard is what drops the offset on the Power key.
        for mode in [SleepMode::Immediate, SleepMode::Fade] {
            let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
            arm(&mut core, 1, mode).await;
            after(&mut core, Duration::from_secs(59)).await;
            if mode == SleepMode::Fade {
                after(&mut core, Duration::from_secs(1)).await;
                fade_steps(&mut core, FADE_STEPS - 1).await;
            }
            core.handle_command(Command::Plus10).await.unwrap();
            assert_eq!(core.pending_tens, 10, "{mode:?}");
            assert!(!core.standby, "{mode:?}");
            // The last step: the deadline, or the fade's last step.
            after(&mut core, if mode == SleepMode::Fade { FADE_STEP } else { Duration::from_secs(1) }).await;
            assert!(core.standby, "{mode:?}");
            assert_eq!(core.pending_tens, 0, "{mode:?}");
            assert!(core.overlay.is_none(), "{mode:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_fade_restores_volume_and_keeps_playing() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 8).await;
        core.handle_command(Command::CancelSleepTimer).await.unwrap();
        assert_eq!(volumes(&calls).last(), Some(&40));
        assert_eq!(core.sleep_timer_state(), None);
        assert!(!core.standby && core.playback && !core.paused);
        let before = calls.lock().unwrap().len();
        after(&mut core, Duration::from_secs(60)).await;
        assert_eq!(calls.lock().unwrap().len(), before, "nothing left to fire");
        // Harmless when there is no timer.
        core.handle_command(Command::CancelSleepTimer).await.unwrap();
        assert_eq!(calls.lock().unwrap().len(), before);
    }

    #[tokio::test(start_paused = true)]
    async fn extend_during_fade_restores_volume_and_rearms_from_now() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 8).await;
        let wall = crate::update::now_unix_s();
        core.handle_command(Command::ExtendSleepTimer(EXTEND_MINUTES)).await.unwrap();
        assert_eq!(volumes(&calls).last(), Some(&40));
        let st = core.sleep_timer_state().unwrap();
        assert_eq!((st.phase, st.mode), (SleepPhase::Armed, SleepMode::Fade));
        assert!(st.ends_at_unix_s.abs_diff(wall + 900) <= 2, "{st:?} vs {wall}");
        after(&mut core, Duration::from_secs(899)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Armed));
        after(&mut core, Duration::from_secs(1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn extend_while_armed_pushes_deadline() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        arm(&mut core, 30, SleepMode::Fade).await;
        let before = core.sleep_timer_state().unwrap().ends_at_unix_s;
        core.handle_command(Command::ExtendSleepTimer(EXTEND_MINUTES)).await.unwrap();
        let st = core.sleep_timer_state().unwrap();
        assert_eq!(st.ends_at_unix_s, before + 900);
        assert_eq!(st.phase, SleepPhase::Armed);
        assert!(volumes(&calls).is_empty(), "nothing to restore while armed");
        after(&mut core, Duration::from_secs(30 * 60)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Armed), "the old deadline no longer fires");
        after(&mut core, Duration::from_secs(15 * 60)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn extend_without_timer_is_ignored() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        core.handle_command(Command::ExtendSleepTimer(EXTEND_MINUTES)).await.unwrap();
        assert_eq!(core.sleep_timer_state(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_while_paused_goes_straight_to_standby_even_in_fade_mode() {
        // Paused, then stopped: the two halves of "not playing".
        for cmd in [Command::PlayPause, Command::Stop] {
            let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
            arm(&mut core, 1, SleepMode::Fade).await;
            core.handle_command(cmd.clone()).await.unwrap();
            assert!(!core.standby);
            after(&mut core, Duration::from_secs(60)).await;
            assert!(core.standby, "{cmd:?}");
            assert_eq!(core.sleep_timer_state(), None, "{cmd:?}");
            assert!(volumes(&calls).is_empty(), "{cmd:?}: no fade {:?}", calls.lock().unwrap());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn set_sleep_timer_in_standby_is_ignored() {
        let (mut core, _calls, _sc, _rx, dir) = playing_at(40).await;
        core.handle_command(Command::Power).await.unwrap();
        assert!(core.standby);
        arm(&mut core, 15, SleepMode::Immediate).await;
        assert_eq!(core.sleep_timer_state(), None);
        assert_eq!(core.sleep_deadline(), None);
        assert_eq!(core.settings.sleep_timer_mode, SleepMode::Fade, "not even remembered");
        assert_eq!(crate::state::load(&dir.path().join("state.json")).settings.sleep_timer_mode, SleepMode::Fade);
        core.handle_command(Command::ExtendSleepTimer(EXTEND_MINUTES)).await.unwrap();
        core.handle_command(Command::CancelSleepTimer).await.unwrap();
        assert!(core.standby, "nothing woke the device");
        assert_eq!(core.sleep_timer_state(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn manual_power_cancels_timer() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        arm(&mut core, 30, SleepMode::Fade).await;
        core.handle_command(Command::Power).await.unwrap();
        assert!(core.standby);
        assert_eq!(core.sleep_timer_state(), None);
        assert!(!volumes(&calls).contains(&40), "nothing to restore while armed");
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.standby);
        assert_eq!(core.sleep_deadline(), None, "the wake does not bring it back");
    }

    #[tokio::test(start_paused = true)]
    async fn set_sleep_timer_remembers_mode_in_settings() {
        let (mut core, _calls, _sc, _rx, dir) = playing_at(40).await;
        arm(&mut core, 15, SleepMode::EndOfTrack).await;
        assert_eq!(core.sleep_timer_mode(), SleepMode::EndOfTrack);
        let disk = || crate::state::load(&dir.path().join("state.json")).settings.sleep_timer_mode;
        assert_eq!(disk(), SleepMode::EndOfTrack);
        // A config page PUTting back an older copy does not undo the choice.
        core.set_settings(crate::state::Settings::default());
        assert_eq!(core.sleep_timer_mode(), SleepMode::EndOfTrack);
        assert_eq!(disk(), SleepMode::EndOfTrack);
        arm(&mut core, 15, SleepMode::Immediate).await;
        assert_eq!(disk(), SleepMode::Immediate);
    }

    #[tokio::test(start_paused = true)]
    async fn new_timer_during_fade_restores_volume() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 8).await;
        arm(&mut core, 30, SleepMode::Immediate).await;
        assert_eq!(volumes(&calls).last(), Some(&40));
        let st = core.sleep_timer_state().unwrap();
        assert_eq!((st.phase, st.mode), (SleepPhase::Armed, SleepMode::Immediate));
    }

    #[tokio::test(start_paused = true)]
    async fn published_state_carries_sleep_timer_and_phase() {
        let (mut core, _calls, _sc, mut rx, _d) = playing_at(40).await;
        let wall = crate::update::now_unix_s();
        arm(&mut core, 1, SleepMode::Fade).await;
        let st = rx.borrow_and_update().sleep_timer.unwrap();
        assert_eq!((st.phase, st.mode), (SleepPhase::Armed, SleepMode::Fade));
        assert!(st.ends_at_unix_s.abs_diff(wall + 60) <= 2, "{st:?} vs {wall}");
        // The phase change at the deadline is published without a command.
        // (The paused clock moves tokio's time, not the wall clock: the end of
        // the fade is read against the wall clock at the deadline.)
        after(&mut core, Duration::from_secs(60)).await;
        let wall = crate::update::now_unix_s();
        let st = rx.borrow_and_update().sleep_timer.unwrap();
        assert_eq!(st.phase, SleepPhase::Fading);
        assert!(st.ends_at_unix_s.abs_diff(wall + 10) <= 2, "the end of the fade: {st:?} vs {wall}");
        fade_steps(&mut core, FADE_STEPS).await;
        let state = rx.borrow_and_update().clone();
        assert!(state.standby);
        assert_eq!(state.sleep_timer, None);
    }
}
