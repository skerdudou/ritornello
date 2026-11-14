//! The sleep timer: a countdown to standby, ended at once or by a fade.
//!
//! One timer at a time. A restart loses it, by the owner's decision —
//! except the restart an update provokes, so that an automatic update at
//! 3 a.m. does not leave the device playing all night: the timer is written
//! to `state.json` on every change (`persisted_sleep`), and `restore_sleep`
//! puts it back after that restart only. At the deadline the device goes to standby exactly as the
//! Power key puts it there (`enter_standby`); `Fade` first ramps mpv's
//! volume down to zero; `EndOfTrack` first waits for the playing track to
//! end (`sleep_on_boundary`), five minutes at most, then fades.
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
/// The longest `EndOfTrack` waits for the track to end before fading anyway.
const END_OF_TRACK_CAP: Duration = Duration::from_secs(300);

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
    /// `EndOfTrack` past its deadline: waiting for a track boundary
    /// (`sleep_on_boundary`), at most until `cap`. The volume is untouched.
    WaitingEndOfTrack { cap: tokio::time::Instant, ends_at_unix_s: u64 },
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

    /// A timer restored after an update restart, `ends_at_unix_s` still
    /// ahead of `now_unix_s`: armed for what was left of it.
    fn restored(ends_at_unix_s: u64, now_unix_s: u64, mode: SleepMode) -> Self {
        let left = Duration::from_secs(ends_at_unix_s.saturating_sub(now_unix_s));
        SleepTimer { mode, phase: Phase::Armed { deadline: tokio::time::Instant::now() + left, ends_at_unix_s } }
    }

    /// The volume mpv must get back, if this timer was fading.
    fn restore_volume(&self) -> Option<u8> {
        match self.phase {
            Phase::Fading { restore_volume, .. } => Some(restore_volume),
            Phase::Armed { .. } | Phase::WaitingEndOfTrack { .. } => None,
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
            Phase::WaitingEndOfTrack { cap, .. } => cap.into_std(),
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
                    SleepMode::EndOfTrack if self.has_track_boundaries() => {
                        self.sleep = Some(SleepTimer {
                            mode: timer.mode,
                            phase: Phase::WaitingEndOfTrack {
                                cap: now + END_OF_TRACK_CAP,
                                ends_at_unix_s: crate::update::now_unix_s() + END_OF_TRACK_CAP.as_secs(),
                            },
                        });
                        self.persist();
                    }
                    // Nothing will ever say the track ended: fade now.
                    SleepMode::Fade | SleepMode::EndOfTrack => self.start_fade(timer.mode),
                }
            }
            // Paused (or stopped) by a way that is not a command, e.g. a
            // source's own action: nothing to fade, as at the deadline.
            Phase::WaitingEndOfTrack { cap, .. } if now >= cap => {
                if self.playback && !self.paused {
                    self.start_fade(timer.mode);
                } else {
                    self.sleep_into_standby().await;
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

    /// Can the playing content say when its track ends? Finite content (a
    /// disc, a file list) always can: `TrackChanged`, its end. A stream only
    /// through its titles: one announced since it started playing (its own
    /// ICY, or a title from its tags or a `metadata` plugin — the slate is
    /// cleared at every new play, and the published title is drawn from
    /// those three) is taken as the promise of a next one. Decided at the
    /// deadline, not when the timer is armed: the source may have changed in
    /// between.
    fn has_track_boundaries(&self) -> bool {
        !self.expecting_stream || self.metadata.title().is_some()
    }

    fn start_fade(&mut self, mode: SleepMode) {
        self.sleep = Some(SleepTimer {
            mode,
            phase: Phase::Fading {
                next_step: tokio::time::Instant::now() + FADE_STEP,
                step: 0,
                from: self.volume,
                restore_volume: self.volume,
                ends_at_unix_s: crate::update::now_unix_s() + (FADE_STEP * u32::from(FADE_STEPS)).as_secs(),
            },
        });
        self.persist();
    }

    /// Is the timer past its deadline, waiting for the track to end?
    pub(super) fn waiting_end_of_track(&self) -> bool {
        matches!(self.sleep, Some(SleepTimer { phase: Phase::WaitingEndOfTrack { .. }, .. }))
    }

    /// A track ended (or a stream announced its next title): a timer waiting
    /// for it starts its fade. Nothing otherwise, so every boundary calls it
    /// unconditionally.
    ///
    /// Synchronous, unlike most of this file: starting a fade touches
    /// nothing but the timer (its first step is `on_sleep_deadline`'s), and
    /// the metadata handlers that see a stream's titles are synchronous.
    pub(super) fn sleep_on_boundary(&mut self) {
        if let Some(timer) = self.sleep
            && self.waiting_end_of_track()
        {
            self.start_fade(timer.mode);
            self.publish_state();
        }
    }

    /// mpv says it is on track `n`: the sleep timer's "end of track" only
    /// when it moves away from the reference (`track_ref`).
    ///
    /// - `n < 0` is no track: mpv sets `playlist-pos` to -1 as a list runs
    ///   out, possibly before it reports idle. The end of the content
    ///   (`PlaybackIdle`) decides then — standby, with no fade on silence.
    /// - The first notification since a `Play` names the track that content
    ///   opened on: a source switched to during the wait, whose track's end
    ///   is still to come. It only sets the reference.
    /// - A `PlayerChapter` the core applied (the cd plugin's resumed track or
    ///   random first pick, answered to that first notification once mpv
    ///   has the disc open) moved the reference already: its own
    ///   notification confirms the seek and matches it.
    pub(super) fn sleep_on_track_changed(&mut self, n: i64) {
        if n < 0 {
            return;
        }
        let previous = self.track_ref.replace(n);
        if previous.is_some_and(|p| p != n) {
            self.sleep_on_boundary();
        }
    }

    /// The published title, when a timer waits for it to change: what a
    /// metadata handler reads before applying its frame and hands back to
    /// `sleep_on_title_change` after. `None` otherwise, so the composed text
    /// is not rebuilt for every frame of every evening.
    pub(super) fn title_before_frame(&self) -> Option<String> {
        if self.waiting_end_of_track() { self.metadata.title() } else { None }
    }

    /// A boundary when the published title moved from one title to another.
    /// The same title restated is not one (mpv and the plugins repeat
    /// themselves), and neither is a first title: on a stream just switched
    /// to, it names the track already playing, whose end is still to come.
    pub(super) fn sleep_on_title_change(&mut self, before: Option<String>) {
        if let Some(before) = before
            && self.metadata.title().is_some_and(|t| t != before)
        {
            self.sleep_on_boundary();
        }
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
        self.persist();
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
                self.persist();
            }
            // Past the deadline: "+15 min" counts from now.
            Phase::Fading { .. } | Phase::WaitingEndOfTrack { .. } => self.arm_sleep_timer(minutes, timer.mode).await,
        }
    }

    /// Clears the timer, setting mpv back to the volume to restore if it was
    /// fading. Harmless without a timer.
    pub(super) async fn cancel_sleep_timer(&mut self) {
        let timer = self.sleep.take();
        self.restore_after_fade(timer).await;
        if timer.is_some() {
            self.persist();
        }
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
    /// stopped, and persists the cleared timer with the standby flag.
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

    /// Pause and Stop during a fade, or while waiting for the end of the
    /// track, send the device to standby at once (Power does too, through
    /// its own arm). Every other command applies as usual and the timer goes
    /// on — the owner's decision: only the timer's own menu stops an
    /// extinction. A source switch while waiting keeps waiting, on the new
    /// source, under the same cap.
    pub(super) fn ends_sleep_wind_down(&self, cmd: &Command) -> bool {
        matches!(self.sleep, Some(SleepTimer { phase: Phase::Fading { .. } | Phase::WaitingEndOfTrack { .. }, .. }))
            && matches!(cmd, Command::PlayPause | Command::Stop)
    }

    /// The timer as `state.json` keeps it. A timer past its deadline —
    /// waiting for the end of the track, or fading — is written with the
    /// moment of writing as its deadline, never with its cap or its fade's
    /// end: the restore after an update must read it as expired (the spec
    /// treats both phases as a deadline already passed), and a restart
    /// quicker than the fade or the cap would otherwise re-arm it.
    pub(super) fn persisted_sleep(&self) -> Option<crate::state::PersistedSleep> {
        self.sleep.map(|t| {
            let ends_at_unix_s = match t.phase {
                Phase::Armed { ends_at_unix_s, .. } => ends_at_unix_s,
                Phase::WaitingEndOfTrack { .. } | Phase::Fading { .. } => crate::update::now_unix_s(),
            };
            crate::state::PersistedSleep { ends_at_unix_s, mode: t.mode }
        })
    }

    /// After the restart an update provoked, on a device awake again: the
    /// timer `state.json` held at launch, armed for what is left of it. Only
    /// a deadline still ahead reaches here — `startup` sends a device whose
    /// timer expired meanwhile straight to standby — and nothing happens on
    /// a device that starts in standby: a timer never survives an entry into
    /// standby, so one found there is a stale file, not a running timer.
    pub(super) fn restore_sleep(&mut self, persisted: crate::state::PersistedSleep, now_unix_s: u64) {
        if self.standby || persisted.ends_at_unix_s <= now_unix_s {
            return;
        }
        self.sleep = Some(SleepTimer::restored(persisted.ends_at_unix_s, now_unix_s, persisted.mode));
        self.persist();
        self.publish_state();
    }

    /// The timer as displays see it.
    pub(super) fn sleep_timer_state(&self) -> Option<SleepTimerState> {
        self.sleep.map(|t| {
            let (phase, ends_at_unix_s) = match t.phase {
                Phase::Armed { ends_at_unix_s, .. } => (SleepPhase::Armed, ends_at_unix_s),
                Phase::WaitingEndOfTrack { ends_at_unix_s, .. } => (SleepPhase::WaitingEndOfTrack, ends_at_unix_s),
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

    // --- End of track ---

    /// The core switched to the disc (finite content), which mpv confirms
    /// is sounding, on track 1.
    async fn on_the_disc(core: &mut Core<FakePlayer>) {
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        core.handle_event(Event::PlaybackActive).await;
        core.handle_event(Event::TrackChanged(1)).await;
        assert!(core.playback && !core.expecting_stream);
        assert_eq!(core.track_ref, Some(1));
    }

    async fn icy(core: &mut Core<FakePlayer>, title: &str) {
        core.handle_event(Event::IcyTitle(title.into())).await;
    }

    /// Arms a one-minute `EndOfTrack` timer and reaches its deadline.
    async fn end_of_track_deadline(core: &mut Core<FakePlayer>) {
        arm(core, 1, SleepMode::EndOfTrack).await;
        after(core, Duration::from_secs(60)).await;
    }

    async fn waiting(core: &mut Core<FakePlayer>) {
        end_of_track_deadline(core).await;
        assert_eq!(phase(core), Some(SleepPhase::WaitingEndOfTrack));
    }

    /// The radio announcing its titles through its tags (an Ogg stream).
    fn tags_titled(title: &str, album: &str) -> Event {
        let track = ritornello_proto::Track { title: Some(title.into()), album: Some(album.into()), ..Default::default() };
        Event::FileTags { track: Some(Box::new(track)), lyrics: None }
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_track_waits_for_track_change_then_fades() {
        let (mut core, calls, _sc, mut rx, _d) = playing_at(40).await;
        on_the_disc(&mut core).await;
        calls.lock().unwrap().clear();
        let wall = crate::update::now_unix_s();
        // Finite content needs no title to have track boundaries.
        waiting(&mut core).await;
        let st = rx.borrow_and_update().sleep_timer.unwrap();
        assert_eq!((st.phase, st.mode), (SleepPhase::WaitingEndOfTrack, SleepMode::EndOfTrack));
        assert!(st.ends_at_unix_s.abs_diff(wall + 300) <= 2, "the cap: {st:?} vs {wall}");
        assert!(volumes(&calls).is_empty(), "the volume is untouched while waiting");
        core.handle_event(Event::TrackChanged(2)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
        assert_eq!(rx.borrow_and_update().sleep_timer.map(|s| s.phase), Some(SleepPhase::Fading), "published");
        fade_steps(&mut core, FADE_STEPS).await;
        assert!(core.standby);
        let v = volumes(&calls);
        let (restore, ramp) = v.split_last().unwrap();
        assert_eq!(ramp.len(), usize::from(FADE_STEPS), "{v:?}");
        assert_eq!(*restore, 40);
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_track_waits_for_icy_title_change_on_a_stream() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        icy(&mut core, "Song A").await;
        waiting(&mut core).await;
        icy(&mut core, "Song B").await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn same_icy_title_is_not_a_boundary() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        icy(&mut core, "Song A").await;
        waiting(&mut core).await;
        // Icecast repeats the header throughout a track.
        icy(&mut core, "Song A").await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_track_without_any_title_fades_immediately() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        assert!(core.expecting_stream, "a stream that never announced a title");
        end_of_track_deadline(&mut core).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_track_cap_fades_after_five_minutes() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        icy(&mut core, "Song A").await;
        waiting(&mut core).await;
        let cap = core.sleep_deadline().unwrap();
        after(&mut core, END_OF_TRACK_CAP - Duration::from_secs(1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        after(&mut core, Duration::from_secs(1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
        assert!(core.sleep_deadline().unwrap() > cap, "now the fade's steps");
        fade_steps(&mut core, FADE_STEPS).await;
        assert!(core.standby);
        assert_eq!(volumes(&calls).last(), Some(&40));
    }

    #[tokio::test(start_paused = true)]
    async fn pause_while_waiting_enters_standby() {
        // Pause, Stop and Power alike: each is its own branch.
        for cmd in [Command::PlayPause, Command::Stop, Command::Power] {
            let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
            icy(&mut core, "Song A").await;
            waiting(&mut core).await;
            core.handle_command(cmd.clone()).await.unwrap();
            assert!(core.standby, "{cmd:?}");
            assert_eq!(core.sleep_timer_state(), None, "{cmd:?}");
            let log = calls.lock().unwrap().clone();
            assert!(!log.contains(&"pause".to_string()), "{cmd:?}: {log:?}");
            assert!(volumes(&calls).is_empty(), "{cmd:?}: nothing to restore {log:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_content_while_waiting_enters_standby() {
        let (mut core, calls, _sc, mut rx, _d) = playing_at(40).await;
        on_the_disc(&mut core).await;
        calls.lock().unwrap().clear();
        waiting(&mut core).await;
        // The disc ran out; the cd source plays nothing after it.
        core.handle_event(Event::PlaybackIdle).await;
        assert!(core.standby);
        assert_eq!(core.sleep_timer_state(), None);
        assert!(volumes(&calls).is_empty(), "no fade on silence: {:?}", calls.lock().unwrap());
        let state = rx.borrow_and_update().clone();
        assert!(state.standby && state.sleep_timer.is_none(), "published");
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_content_with_another_pass_while_waiting_fades() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        // The radio fake answers an ending with another (finite) pass.
        core.expecting_stream = false;
        core.handle_event(Event::PlaybackActive).await;
        waiting(&mut core).await;
        core.handle_event(Event::PlaybackIdle).await;
        assert!(core.playback, "the next pass plays");
        assert!(!core.standby);
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_track_waits_for_tag_title_change_on_a_stream() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        core.handle_event(tags_titled("Song A", "One")).await;
        // The tags' title is a title source: no ICY needed to wait.
        waiting(&mut core).await;
        // New tags, same title: not a boundary.
        core.handle_event(tags_titled("Song A", "Two")).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        core.handle_event(tags_titled("Song B", "Two")).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn end_of_track_waits_for_metadata_title_change() {
        let (mut core, _np, _rx, _d) = setup_metadata(vec!["p".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "fip"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment("p", enrichment(id.clone(), "Artist", "Song A"));
        assert_eq!(core.metadata.icy(), None, "no ICY: the plugin's title alone");
        waiting(&mut core).await;
        // Another answer naming the same song: not a boundary.
        core.handle_enrichment("p", enrichment(id.clone(), "Other artist", "Song A"));
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        core.handle_enrichment("p", enrichment(id, "Artist", "Song B"));
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn icy_change_under_a_plugin_title_is_a_boundary() {
        // The plugin outranks the ICY on screen; a plugin that has stopped
        // answering must not hide the stream's own announcement.
        let (mut core, _np, _rx, _d) = setup_metadata(vec!["p".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "fip"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment("p", enrichment(id, "Artist", "Song A"));
        icy(&mut core, "Song A - Artist").await;
        waiting(&mut core).await;
        icy(&mut core, "Song B - Artist").await;
        assert_eq!(core.metadata.title().as_deref(), Some("Song A"), "the screen did not move");
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn source_switch_while_waiting_keeps_waiting_under_the_same_cap() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        icy(&mut core, "Song A").await;
        waiting(&mut core).await;
        let cap = (core.sleep_deadline(), core.sleep_timer_state().unwrap().ends_at_unix_s);
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        assert_eq!(core.active_source, "cd");
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        // The disc's first notification names the track it opened on, not
        // the end of one.
        core.handle_event(Event::PlaybackActive).await;
        core.handle_event(Event::TrackChanged(1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        assert_eq!((core.sleep_deadline(), core.sleep_timer_state().unwrap().ends_at_unix_s), cap);
        // The same notification restated is not a change either.
        core.handle_event(Event::TrackChanged(1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        core.handle_event(Event::TrackChanged(2)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn resumed_track_seek_is_not_a_boundary() {
        // The real order on the disc (cd plugin, `pending_chapter`): mpv has
        // it open, notifies track 0, the plugin answers with the chapter it
        // owed, and mpv notifies that one.
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        icy(&mut core, "Song A").await;
        waiting(&mut core).await;
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        core.handle_event(Event::PlaybackActive).await;
        core.handle_event(Event::TrackChanged(0)).await;
        assert_eq!(core.track_ref, Some(4), "the fake cd answered PlayerChapter(4)");
        core.handle_event(Event::TrackChanged(4)).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack), "the resumed track plays to its end");
        // Its end is a boundary.
        core.handle_event(Event::TrackChanged(5)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn new_content_resets_the_track_reference() {
        let (mut core, _calls, _sc, _rx, _d) = playing_at(40).await;
        on_the_disc(&mut core).await;
        waiting(&mut core).await;
        // The radio's stream sits at playlist position 0: its first
        // notification is a reference, not a change from the disc's track 1.
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();
        core.handle_event(Event::TrackChanged(0)).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
    }

    #[tokio::test(start_paused = true)]
    async fn no_track_then_end_of_content_enters_standby() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        on_the_disc(&mut core).await;
        calls.lock().unwrap().clear();
        waiting(&mut core).await;
        // mpv leaves the list before it says it is idle.
        core.handle_event(Event::TrackChanged(-1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        core.handle_event(Event::PlaybackIdle).await;
        assert!(core.standby);
        assert!(volumes(&calls).is_empty(), "no fade on silence: {:?}", calls.lock().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn cap_reached_while_not_playing_enters_standby() {
        // Paused, then stopped, by a way that is not a command (which would
        // have ended the wait already): the two halves of "not playing".
        for paused in [true, false] {
            let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
            icy(&mut core, "Song A").await;
            waiting(&mut core).await;
            if paused {
                core.paused = true;
            } else {
                core.playback = false;
            }
            after(&mut core, END_OF_TRACK_CAP).await;
            assert!(core.standby, "paused: {paused}");
            assert_eq!(core.sleep_timer_state(), None, "paused: {paused}");
            assert!(volumes(&calls).is_empty(), "paused: {paused}: no fade {:?}", calls.lock().unwrap());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn first_title_after_a_switch_is_not_a_boundary() {
        // A plugin, then ICY: each path's "before" must be a title.
        let (mut core, _np, _rx, _d) = setup_metadata(vec!["p".into()]);
        core.resume().await.unwrap();
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        core.handle_event(Event::PlaybackActive).await;
        waiting(&mut core).await;
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();
        assert!(core.expecting_stream);
        let id = serde_json::json!({"url": "fip"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment("p", enrichment(id, "Artist", "Song A"));
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack), "plugin");
        icy(&mut core, "Song A").await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack), "ICY");
        icy(&mut core, "Song B").await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn extend_or_cancel_while_waiting() {
        let (mut core, calls, _sc, _rx, _d) = playing_at(40).await;
        icy(&mut core, "Song A").await;
        waiting(&mut core).await;
        core.handle_command(Command::ExtendSleepTimer(EXTEND_MINUTES)).await.unwrap();
        assert_eq!(phase(&core), Some(SleepPhase::Armed), "+15 min counts from now");
        after(&mut core, Duration::from_secs(15 * 60 - 1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Armed));
        after(&mut core, Duration::from_secs(1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::WaitingEndOfTrack));
        core.handle_command(Command::CancelSleepTimer).await.unwrap();
        assert_eq!(core.sleep_timer_state(), None);
        assert!(volumes(&calls).is_empty(), "nothing to restore: {:?}", calls.lock().unwrap());
        icy(&mut core, "Song B").await;
        assert_eq!(core.sleep_timer_state(), None, "a boundary without a timer does nothing");
        assert!(!core.standby);
    }

    // --- Surviving an update restart ---

    use crate::state::PersistedSleep;
    use crate::update::StartupOverride;

    fn disk_sleep(dir: &tempfile::TempDir) -> Option<PersistedSleep> {
        crate::state::load(&dir.path().join("state.json")).sleep_timer
    }

    /// The restart an update provoked, `since` a few seconds ago.
    fn after_update(now: u64) -> StartupOverride {
        StartupOverride::Previous { since_unix_s: now - 5 }
    }

    /// A core restarted from a state file written awake on the radio at
    /// volume 40, carrying `sleep`.
    fn restarted_with(sleep: Option<PersistedSleep>) -> Rig {
        setup_persisted(PersistedState { volume: 40, sleep_timer: sleep, ..Default::default() })
    }

    #[tokio::test(start_paused = true)]
    async fn timer_is_written_on_every_change() {
        let (mut core, _calls, _sc, _rx, dir) = playing_at(40).await;
        assert_eq!(disk_sleep(&dir), None);
        arm(&mut core, 30, SleepMode::EndOfTrack).await;
        let armed = core.sleep_timer_state().unwrap().ends_at_unix_s;
        assert_eq!(disk_sleep(&dir), Some(PersistedSleep { ends_at_unix_s: armed, mode: SleepMode::EndOfTrack }));
        core.handle_command(Command::ExtendSleepTimer(EXTEND_MINUTES)).await.unwrap();
        assert_eq!(disk_sleep(&dir).map(|t| t.ends_at_unix_s), Some(armed + 15 * 60));
        core.handle_command(Command::CancelSleepTimer).await.unwrap();
        assert_eq!(disk_sleep(&dir), None);
        // Expiry: the timer ends with the standby it brings.
        arm(&mut core, 1, SleepMode::Immediate).await;
        assert!(disk_sleep(&dir).is_some());
        after(&mut core, Duration::from_secs(60)).await;
        assert!(core.standby);
        assert_eq!(disk_sleep(&dir), None);
    }

    /// Past its deadline, a timer is written as expired, never with its cap
    /// or its fade's end: a restart quicker than either would re-arm it.
    #[tokio::test(start_paused = true)]
    async fn a_timer_winding_down_is_written_as_expired() {
        let (mut core, _calls, _sc, _rx, dir) = playing_at(40).await;
        icy(&mut core, "Song A").await;
        waiting(&mut core).await;
        let written = disk_sleep(&dir).expect("the wait is written");
        assert!(written.ends_at_unix_s <= crate::update::now_unix_s(), "{written:?}");
        assert!(written.ends_at_unix_s < core.sleep_timer_state().unwrap().ends_at_unix_s);
        let (mut core, _calls, _sc, _rx, dir) = playing_at(40).await;
        fading(&mut core).await;
        let written = disk_sleep(&dir).expect("the fade is written");
        assert!(written.ends_at_unix_s <= crate::update::now_unix_s(), "{written:?}");
        assert_eq!(written.mode, SleepMode::Fade);
    }

    #[tokio::test(start_paused = true)]
    async fn timer_survives_update_restart_with_remaining_time() {
        let now = crate::update::now_unix_s();
        let sleep = PersistedSleep { ends_at_unix_s: now + 600, mode: SleepMode::Fade };
        let (mut core, _calls, sc, rx, dir) = restarted_with(Some(sleep));
        core.startup(after_update(now), now).await.unwrap();
        assert!(!core.standby);
        assert!(sc.lock().unwrap().iter().any(|c| c == "radio:Wake"), "the device is restored first");
        let st = rx.borrow().sleep_timer.expect("the restored timer is published");
        assert_eq!((st.phase, st.mode, st.ends_at_unix_s), (SleepPhase::Armed, SleepMode::Fade, now + 600));
        assert_eq!(disk_sleep(&dir), Some(sleep));
        // Armed for what was left of it, not for a fresh span.
        after(&mut core, Duration::from_secs(599)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Armed));
        after(&mut core, Duration::from_secs(1)).await;
        assert_eq!(phase(&core), Some(SleepPhase::Fading));
    }

    #[tokio::test(start_paused = true)]
    async fn expired_timer_after_update_restart_enters_standby() {
        let now = crate::update::now_unix_s();
        for ends_at_unix_s in [now - 30, now] {
            let sleep = PersistedSleep { ends_at_unix_s, mode: SleepMode::Fade };
            let (mut core, _calls, sc, rx, dir) = restarted_with(Some(sleep));
            core.startup(after_update(now), now).await.unwrap();
            assert!(core.standby, "{ends_at_unix_s}");
            assert!(rx.borrow().standby);
            // Straight to standby: the source is not woken for an instant.
            assert!(sc.lock().unwrap().is_empty(), "{:?}", sc.lock().unwrap());
            assert_eq!(core.sleep_timer_state(), None);
            let disk = crate::state::load(&dir.path().join("state.json"));
            assert_eq!((disk.standby, disk.sleep_timer), (true, None));
        }
    }

    /// The real path: a fade under way, a volume key during it, the state
    /// file it leaves, and a core started from that file.
    #[tokio::test(start_paused = true)]
    async fn fading_timer_after_update_restart_enters_standby_at_restore_volume() {
        let (mut core, _calls, _sc, _rx, dir) = playing_at(40).await;
        fading(&mut core).await;
        fade_steps(&mut core, 8).await;
        core.handle_command(Command::VolumeUp).await.unwrap();
        fade_steps(&mut core, 2).await;
        let left = crate::state::load(&dir.path().join("state.json"));
        let now = crate::update::now_unix_s();
        let (mut core, calls, sc, _rx, dir) = setup_persisted(left);
        core.startup(after_update(now), now).await.unwrap();
        assert!(core.standby);
        assert!(sc.lock().unwrap().is_empty(), "{:?}", sc.lock().unwrap());
        assert_eq!(volumes(&calls), vec![45], "{:?}", calls.lock().unwrap());
        assert_eq!(core.sleep_timer_state(), None);
        let disk = crate::state::load(&dir.path().join("state.json"));
        assert_eq!((disk.volume, disk.standby, disk.sleep_timer), (45, true, None));
    }

    #[tokio::test(start_paused = true)]
    async fn timer_dropped_after_plain_restart() {
        let now = crate::update::now_unix_s();
        let sleep = PersistedSleep { ends_at_unix_s: now + 600, mode: SleepMode::Fade };
        let (mut core, _calls, _sc, _rx, dir) = restarted_with(Some(sleep));
        core.startup(StartupOverride::AsConfigured, now).await.unwrap();
        assert!(!core.standby, "the default Startup setting wakes");
        assert_eq!(core.sleep_timer_state(), None);
        assert_eq!(core.sleep_deadline(), None);
        assert_eq!(disk_sleep(&dir), None, "and the file forgets it");
        // An expired one does not send it to standby either.
        let sleep = PersistedSleep { ends_at_unix_s: now - 30, mode: SleepMode::Fade };
        let (mut core, _calls, _sc, _rx, _dir) = restarted_with(Some(sleep));
        core.startup(StartupOverride::AsConfigured, now).await.unwrap();
        assert!(!core.standby);
    }
}
