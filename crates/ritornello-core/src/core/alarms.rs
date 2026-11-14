//! Alarm clocks. The pure part first: which alarms are due at a given
//! minute, which one rings next, with no clock and no I/O, so each rule is
//! testable by building a `LocalNow` by hand. Then the core's side: the
//! minute check `main` wakes for, and what an alarm does to the device.

use super::*;
use crate::state::{Alarm, AlarmTarget};
use crate::update::schedule::{LocalNow, Weekday};
use ritornello_proto::NextAlarm;

/// Weekdays in `Weekday`'s own order (Sunday first), to step through a week.
const WEEK: [Weekday; 7] = [
    Weekday::Sunday,
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
];

const MINUTES_PER_DAY: i64 = 24 * 60;

fn index(day: Weekday) -> i64 {
    WEEK.iter().position(|d| *d == day).expect("WEEK lists every weekday") as i64
}

/// The index of the minute `unix_s` falls in. A minute is the unit an alarm
/// fires in, so "already fired this minute" is an equality on this number.
/// `div_euclid`: floors for a time before the epoch too.
pub fn minute_index(unix_s: i64) -> i64 {
    unix_s.div_euclid(60)
}

/// How long `main` sleeps before the next minute check: until one second
/// past the next minute boundary. An absolute deadline recomputed after each
/// check, never an `interval(60 s)`: an interval drifts against the wall
/// clock, and a tick that lands at :59.9 then the next at :01.0 skips a minute
/// whole. The second of margin keeps a check that wakes a hair early from
/// still reading the previous minute.
pub fn secs_until_next_check(now_unix_s: i64) -> u64 {
    // In 1..=60: from :00 exactly, the next boundary is a whole minute away.
    let to_boundary = 60 - now_unix_s.rem_euclid(60);
    (to_boundary + 1) as u64
}

/// The alarms that ring at `at`'s minute: enabled, that weekday among their
/// days, same hour and minute.
pub fn due_at<'a>(alarms: &'a [Alarm], at: &LocalNow) -> Vec<&'a Alarm> {
    alarms
        .iter()
        .filter(|a| {
            a.enabled
                && a.days.contains(&at.weekday)
                && u32::from(a.hour) == at.hour
                && u32::from(a.minute) == at.minute
        })
        .collect()
}

/// The earliest enabled alarm **strictly after** `now`'s minute, within the
/// coming seven days. An alarm at exactly `now`'s minute is firing now, so it
/// counts for next week.
pub fn next_alarm(alarms: &[Alarm], now: &LocalNow) -> Option<NextAlarm> {
    let now_min = i64::from(now.hour) * 60 + i64::from(now.minute);
    let today = index(now.weekday);
    let mut best: Option<(i64, NextAlarm)> = None;
    for a in alarms.iter().filter(|a| a.enabled) {
        let at_min = i64::from(a.hour) * 60 + i64::from(a.minute);
        for day in &a.days {
            let mut day_off = (index(*day) - today).rem_euclid(7);
            // Today, and not strictly later than now: the same time next week.
            if day_off == 0 && at_min <= now_min {
                day_off = 7;
            }
            let distance = day_off * MINUTES_PER_DAY + at_min - now_min;
            if best.as_ref().is_none_or(|(d, _)| distance < *d) {
                best = Some((distance, NextAlarm { weekday: *day, hour: a.hour, minute: a.minute }));
            }
        }
    }
    best.map(|(_, n)| n)
}

impl<P: Player> Core<P> {
    /// The minute check `main` wakes for, just past each minute boundary.
    ///
    /// The current minute is evaluated only if it is later than the last one
    /// an alarm fired in: a check that runs twice in one minute (a late
    /// wake-up, then the regular one) rings nothing twice, and one that runs
    /// 40 s into its minute still rings. Only the current minute: no catch-up
    /// of minutes the device slept through.
    ///
    /// Several alarms due in the same minute: the first in the list rings,
    /// the others are that same minute's, already answered.
    ///
    /// Republishes in any case, so `next_alarm` moves on past the minute just
    /// checked; deduplication makes that free when nothing changed.
    pub async fn check_alarms(&mut self, now_unix_s: i64) {
        let minute = minute_index(now_unix_s);
        // The clock read once: the alarm to fire and the next one to publish
        // come from the same instant.
        let local = crate::update::schedule::local_now(now_unix_s);
        if self.last_alarm_minute.is_none_or(|last| minute > last)
            && let Some(at) = &local
            && let Some(alarm) = due_at(&self.settings.alarms, at).first().map(|a| (*a).clone())
        {
            // Written before the alarm acts: firing can wait up to 5 s on a
            // plugin, and a restart meanwhile must not ring it again.
            self.last_alarm_minute = Some(minute);
            self.persist();
            self.fire_alarm(&alarm).await;
        }
        self.next_alarm = local.and_then(|at| next_alarm(&self.settings.alarms, &at));
        self.publish_state();
    }

    /// Recomputes the next alarm as displays see it. `None` when the clock
    /// cannot be read.
    pub(super) fn refresh_next_alarm(&mut self, now_unix_s: i64) {
        self.next_alarm =
            crate::update::schedule::local_now(now_unix_s).and_then(|at| next_alarm(&self.settings.alarms, &at));
    }

    /// What an alarm does to the device.
    ///
    /// Already playing (awake, not paused, muted or not), it does nothing at
    /// all: no volume, no source, no timer. Otherwise it unmutes, sets its
    /// volume (applied and persisted, with no overlay), then plays its
    /// target: in standby, the waking half of Power (`resume`), or `wake_on`
    /// its source; awake, paused or stopped, the Play half of Play/Pause
    /// (never Power, which would put the device to sleep), or a switch to its
    /// source if another one is active. A source that is not wired falls
    /// back to "resume". Last, the sleep timer it carries is armed.
    ///
    /// Best effort throughout: a step that fails is logged and the next one
    /// still runs, so an mpv that misses the unmute does not cost the wake.
    pub(super) async fn fire_alarm(&mut self, alarm: &Alarm) {
        if !self.standby && self.playback && !self.paused {
            tracing::info!("alarm {} ignored: already playing", alarm.id);
            return;
        }
        tracing::info!("alarm {} rings", alarm.id);
        self.muted = false;
        if let Err(e) = self.player.set_mute(false).await {
            tracing::warn!("alarm unmute: {e}");
        }
        self.volume = alarm.volume.min(100);
        if let Err(e) = self.player.set_volume(self.volume).await {
            tracing::warn!("alarm volume: {e}");
        }
        self.persist();
        let source = match &alarm.target {
            AlarmTarget::Source { name } if self.source_order.contains(name) => Some(name.clone()),
            AlarmTarget::Source { name } => {
                tracing::info!("alarm source {name} not available: resuming instead");
                None
            }
            AlarmTarget::Resume => None,
        };
        let outcome = match (self.standby, source) {
            (true, Some(name)) => self.wake_on(Some(name)).await,
            (true, None) => {
                // The flag first, then the wake: see `leave_standby`.
                self.leave_standby();
                self.resume().await
            }
            (false, Some(name)) if name != self.active_source => {
                self.cycle_source(Some(name), SourceReq::Activate).await
            }
            (false, _) => self.play().await,
        };
        if let Err(e) = outcome {
            tracing::warn!("alarm wake: {e}");
        }
        if let Some(sleep) = alarm.sleep_after {
            self.arm_sleep_timer(sleep.minutes, sleep.mode).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alarm(id: u32, hour: u8, minute: u8, days: &[Weekday]) -> Alarm {
        Alarm {
            id,
            enabled: true,
            hour,
            minute,
            days: days.to_vec(),
            target: AlarmTarget::Resume,
            volume: 30,
            sleep_after: None,
        }
    }

    fn at(hour: u32, minute: u32, weekday: Weekday) -> LocalNow {
        LocalNow { hour, minute, weekday, day_key: 0 }
    }

    #[test]
    fn due_at_matches_enabled_alarm_on_its_day_and_minute() {
        let alarms = [alarm(1, 7, 30, &[Weekday::Monday, Weekday::Friday])];
        assert_eq!(due_at(&alarms, &at(7, 30, Weekday::Friday)).len(), 1);
        assert_eq!(due_at(&alarms, &at(7, 30, Weekday::Monday))[0].id, 1);
    }

    #[test]
    fn due_at_ignores_disabled_and_other_days() {
        let mut off = alarm(1, 7, 30, &[Weekday::Monday]);
        off.enabled = false;
        let alarms = [
            off,
            alarm(2, 7, 30, &[Weekday::Tuesday]),
            alarm(3, 7, 31, &[Weekday::Monday]),
            alarm(4, 8, 30, &[Weekday::Monday]),
        ];
        assert!(due_at(&alarms, &at(7, 30, Weekday::Monday)).is_empty());
    }

    #[test]
    fn next_alarm_picks_earliest_in_the_week() {
        let alarms = [
            alarm(1, 9, 0, &[Weekday::Friday]),
            alarm(2, 6, 15, &[Weekday::Wednesday, Weekday::Saturday]),
            alarm(3, 23, 0, &[Weekday::Tuesday]),
        ];
        // Tuesday 10:00: Tuesday 23:00 comes before Wednesday 06:15.
        let n = next_alarm(&alarms, &at(10, 0, Weekday::Tuesday)).unwrap();
        assert_eq!(n, NextAlarm { weekday: Weekday::Tuesday, hour: 23, minute: 0 });
        // Tuesday 23:30: Wednesday 06:15.
        let n = next_alarm(&alarms, &at(23, 30, Weekday::Tuesday)).unwrap();
        assert_eq!(n, NextAlarm { weekday: Weekday::Wednesday, hour: 6, minute: 15 });
    }

    #[test]
    fn next_alarm_wraps_to_next_week() {
        // Monday 07:00 exactly: it is firing now, the next one is a week away.
        let a = [alarm(1, 7, 0, &[Weekday::Monday])];
        let n = next_alarm(&a, &at(7, 0, Weekday::Monday)).unwrap();
        assert_eq!(n, NextAlarm { weekday: Weekday::Monday, hour: 7, minute: 0 });
        // ... so a later alarm on another day wins over that week-away one.
        let both = [alarm(1, 7, 0, &[Weekday::Monday]), alarm(2, 8, 0, &[Weekday::Sunday])];
        assert_eq!(next_alarm(&both, &at(7, 0, Weekday::Monday)).unwrap().weekday, Weekday::Sunday);
        // Saturday evening, alarm on Sunday morning: crosses the end of the week.
        let sun = [alarm(1, 7, 0, &[Weekday::Sunday])];
        assert_eq!(next_alarm(&sun, &at(20, 0, Weekday::Saturday)).unwrap().weekday, Weekday::Sunday);
        // Sunday 07:01 with a Sunday 07:00 alarm: next Sunday. One minute
        // earlier, it is still ahead today, and a nearer one beats it.
        let pair = [alarm(1, 7, 0, &[Weekday::Sunday]), alarm(2, 9, 0, &[Weekday::Wednesday])];
        assert_eq!(next_alarm(&pair, &at(7, 1, Weekday::Sunday)).unwrap().weekday, Weekday::Wednesday);
        assert_eq!(next_alarm(&pair, &at(6, 59, Weekday::Sunday)).unwrap().weekday, Weekday::Sunday);
    }

    #[test]
    fn next_alarm_none_when_all_disabled() {
        let mut a = alarm(1, 7, 0, &[Weekday::Monday]);
        a.enabled = false;
        assert_eq!(next_alarm(&[a], &at(0, 0, Weekday::Monday)), None);
        assert_eq!(next_alarm(&[], &at(0, 0, Weekday::Monday)), None);
    }

    #[test]
    fn minute_index_floors_and_is_stable_inside_a_minute() {
        assert_eq!(minute_index(0), 0);
        assert_eq!(minute_index(59), 0);
        assert_eq!(minute_index(60), 1);
        assert_eq!(minute_index(-1), -1);
    }

    #[test]
    fn next_check_is_one_second_past_the_next_minute_boundary() {
        assert_eq!(secs_until_next_check(T), 61);
        assert_eq!(secs_until_next_check(T + 1), 60);
        assert_eq!(secs_until_next_check(T + 59), 2);
        assert_eq!(secs_until_next_check(-1), 2);
    }

    // --- Firing ---

    use crate::core::test_support::*;
    use ritornello_proto::{Playback, SleepMode, SleepPhase};
    use std::sync::Mutex;

    /// A minute boundary (2023-11-14 22:13 UTC). Every test builds its alarm
    /// from what the local clock says at `T`, so none depends on the zone the
    /// tests run in.
    const T: i64 = 1_699_999_980;

    /// An alarm ringing at `t`'s local minute, on that weekday only.
    fn ringing_at(t: i64, target: AlarmTarget, volume: u8) -> Alarm {
        let now = crate::update::schedule::local_now(t).expect("the test clock reads");
        Alarm {
            id: 1,
            enabled: true,
            hour: now.hour as u8,
            minute: now.minute as u8,
            days: vec![now.weekday],
            target,
            volume,
            sleep_after: None,
        }
    }

    fn source(name: &str) -> AlarmTarget {
        AlarmTarget::Source { name: name.into() }
    }

    fn with_alarms(core: &mut Core<FakePlayer>, alarms: Vec<Alarm>) {
        let mut s = core.settings.clone();
        s.alarms = alarms;
        core.set_settings(s);
    }

    fn clear(calls: &Mutex<Vec<String>>) {
        calls.lock().unwrap().clear();
    }

    fn has(calls: &Mutex<Vec<String>>, call: &str) -> bool {
        calls.lock().unwrap().iter().any(|c| c == call)
    }

    fn on_disk(dir: &tempfile::TempDir) -> PersistedState {
        crate::state::load(&dir.path().join("state.json"))
    }

    /// A device in standby at volume 20, on the radio, its call logs empty.
    async fn in_standby() -> Rig {
        let (mut core, pc, sc, rx, d) = setup_persisted(PersistedState { volume: 20, ..Default::default() });
        core.start_in_standby().await.unwrap();
        clear(&pc);
        clear(&sc);
        (core, pc, sc, rx, d)
    }

    /// A device playing the radio at volume 60, its call logs empty.
    async fn playing() -> Rig {
        let (mut core, pc, sc, rx, d) = setup();
        core.resume().await.unwrap();
        assert!(core.playback && !core.paused);
        clear(&pc);
        clear(&sc);
        (core, pc, sc, rx, d)
    }

    #[tokio::test]
    async fn alarm_in_standby_resumes_with_its_volume() {
        let (mut core, pc, sc, rx, d) = in_standby().await;
        with_alarms(&mut core, vec![ringing_at(T, AlarmTarget::Resume, 35)]);
        core.check_alarms(T).await;
        assert!(!core.standby);
        assert!(has(&sc, "radio:Wake"), "{:?}", sc.lock().unwrap());
        assert!(has(&pc, "mute false") && has(&pc, "vol 35"), "{:?}", pc.lock().unwrap());
        assert_eq!(core.volume, 35);
        let disk = on_disk(&d);
        assert_eq!((disk.volume, disk.standby), (35, false));
        assert_eq!(disk.last_alarm_minute, Some(minute_index(T)));
        let state = rx.borrow().clone();
        assert!(!state.standby);
        assert_eq!(state.volume, 35);
        assert_eq!(state.overlay, None, "an alarm sets its volume without an overlay");
    }

    #[tokio::test]
    async fn alarm_in_standby_on_named_source_wakes_on_it() {
        let (mut core, _pc, sc, _rx, _d) = in_standby().await;
        with_alarms(&mut core, vec![ringing_at(T, source("cd"), 35)]);
        core.check_alarms(T).await;
        assert!(!core.standby);
        assert_eq!(core.active_source, "cd");
        assert!(has(&sc, "cd:Activate"), "{:?}", sc.lock().unwrap());
        assert!(!has(&sc, "radio:Wake"), "the old source is not woken first");
    }

    #[tokio::test]
    async fn alarm_with_unknown_source_resumes() {
        // In standby: the waking half of Power, on the active source.
        let (mut core, _pc, sc, _rx, _d) = in_standby().await;
        with_alarms(&mut core, vec![ringing_at(T, source("spotify"), 35)]);
        core.check_alarms(T).await;
        assert!(!core.standby);
        assert_eq!(core.active_source, "radio");
        assert!(has(&sc, "radio:Wake"), "{:?}", sc.lock().unwrap());
        // Awake and stopped: Play on the active source.
        let (mut core, _pc, sc, _rx, _d) = playing().await;
        core.handle_command(Command::Stop).await.unwrap();
        clear(&sc);
        with_alarms(&mut core, vec![ringing_at(T, source("spotify"), 35)]);
        core.check_alarms(T).await;
        assert_eq!(core.active_source, "radio");
        assert!(has(&sc, "radio:Play"), "{:?}", sc.lock().unwrap());
        assert!(core.playback);
    }

    #[tokio::test]
    async fn alarm_does_nothing_while_playing_even_muted() {
        let (mut core, pc, sc, _rx, d) = playing().await;
        core.handle_command(Command::Mute).await.unwrap();
        clear(&pc);
        let mut alarm = ringing_at(T, source("cd"), 35);
        alarm.sleep_after = Some(crate::state::AlarmSleep { minutes: 30, mode: SleepMode::Fade });
        with_alarms(&mut core, vec![alarm]);
        core.check_alarms(T).await;
        assert!(pc.lock().unwrap().is_empty(), "{:?}", pc.lock().unwrap());
        assert!(sc.lock().unwrap().is_empty(), "{:?}", sc.lock().unwrap());
        assert!(core.muted);
        assert_eq!(core.volume, 60);
        assert_eq!(core.active_source, "radio");
        assert_eq!(core.sleep_timer_state(), None);
        assert_eq!(on_disk(&d).volume, 60);
        // The minute is answered all the same, and written before the alarm
        // acts: nothing else in this path writes the state file.
        assert_eq!(on_disk(&d).last_alarm_minute, Some(minute_index(T)));
    }

    #[tokio::test]
    async fn alarm_while_paused_unmutes_and_plays_without_power() {
        let (mut core, pc, sc, rx, d) = playing().await;
        core.handle_command(Command::PlayPause).await.unwrap();
        core.handle_command(Command::Mute).await.unwrap();
        assert!(core.paused && core.muted);
        clear(&pc);
        clear(&sc);
        with_alarms(&mut core, vec![ringing_at(T, AlarmTarget::Resume, 35)]);
        core.check_alarms(T).await;
        assert!(!core.standby, "Power would have put the device to sleep");
        assert!(core.playback && !core.paused && !core.muted);
        assert!(has(&pc, "mute false") && has(&pc, "vol 35") && has(&pc, "pause"), "{:?}", pc.lock().unwrap());
        assert!(!has(&pc, "stop"), "{:?}", pc.lock().unwrap());
        assert!(!has(&sc, "radio:Deactivate"), "{:?}", sc.lock().unwrap());
        assert_eq!(rx.borrow().playback, Playback::Playing);
        assert_eq!(on_disk(&d).volume, 35);
    }

    #[tokio::test]
    async fn alarm_awake_plays_its_source_or_switches_to_it() {
        // Stopped on the radio, the alarm names the radio: Play.
        let (mut core, _pc, sc, _rx, _d) = playing().await;
        core.handle_command(Command::Stop).await.unwrap();
        clear(&sc);
        with_alarms(&mut core, vec![ringing_at(T, source("radio"), 35)]);
        core.check_alarms(T).await;
        assert!(has(&sc, "radio:Play"), "{:?}", sc.lock().unwrap());
        assert!(!has(&sc, "radio:Activate"));
        // Paused on the radio, the alarm names the cd: a switch, as by hand.
        let (mut core, _pc, sc, _rx, _d) = playing().await;
        core.handle_command(Command::PlayPause).await.unwrap();
        clear(&sc);
        with_alarms(&mut core, vec![ringing_at(T, source("cd"), 35)]);
        core.check_alarms(T).await;
        assert!(!core.standby);
        assert_eq!(core.active_source, "cd");
        assert!(has(&sc, "cd:Activate"), "{:?}", sc.lock().unwrap());
    }

    #[tokio::test]
    async fn alarm_with_sleep_after_arms_timer() {
        let (mut core, _pc, _sc, rx, _d) = in_standby().await;
        let mut alarm = ringing_at(T, AlarmTarget::Resume, 35);
        alarm.sleep_after = Some(crate::state::AlarmSleep { minutes: 30, mode: SleepMode::Fade });
        with_alarms(&mut core, vec![alarm]);
        let wall = crate::update::now_unix_s();
        core.check_alarms(T).await;
        assert!(!core.standby);
        let st = rx.borrow().sleep_timer.expect("the alarm's timer is published");
        assert_eq!((st.phase, st.mode), (SleepPhase::Armed, SleepMode::Fade));
        assert!(st.ends_at_unix_s.abs_diff(wall + 30 * 60) <= 2, "{st:?} vs {wall}");
    }

    #[tokio::test]
    async fn alarm_fires_once_when_checked_twice_in_the_same_minute() {
        let (mut core, _pc, sc, _rx, d) = in_standby().await;
        with_alarms(&mut core, vec![ringing_at(T, AlarmTarget::Resume, 35)]);
        core.check_alarms(T).await;
        assert!(!core.standby);
        // Switched off by hand 20 s later, still inside the alarm's minute.
        core.handle_command(Command::Power).await.unwrap();
        assert!(core.standby);
        clear(&sc);
        core.check_alarms(T + 30).await;
        assert!(core.standby, "the same minute rang twice");
        assert!(sc.lock().unwrap().is_empty(), "{:?}", sc.lock().unwrap());
        // Across a restart too: the minute that rang is on disk.
        let (mut core, _pc, sc, _rx, _d) = setup_persisted(on_disk(&d));
        core.start_in_standby().await.unwrap();
        clear(&sc);
        core.check_alarms(T + 50).await;
        assert!(core.standby, "a restart rang the same minute again");
    }

    #[tokio::test]
    async fn alarm_fires_when_checked_40_seconds_late_in_its_minute() {
        let (mut core, _pc, sc, _rx, _d) = in_standby().await;
        with_alarms(&mut core, vec![ringing_at(T, AlarmTarget::Resume, 35)]);
        // The minute before rings nothing...
        core.check_alarms(T - 20).await;
        assert!(core.standby);
        // ... and a check that wakes 40 s into the alarm's minute still rings.
        core.check_alarms(T + 40).await;
        assert!(!core.standby);
        assert!(has(&sc, "radio:Wake"));
    }

    #[tokio::test]
    async fn published_state_carries_next_alarm() {
        let (mut core, _pc, _sc, mut rx, _d) = in_standby().await;
        let mut alarm = ringing_at(T, AlarmTarget::Resume, 35);
        alarm.days = WEEK.to_vec();
        let alarms = vec![alarm];
        let at_t = crate::update::schedule::local_now(T).unwrap();
        with_alarms(&mut core, alarms.clone());
        // Two minutes before: the next one is `T`'s own minute.
        core.check_alarms(T - 120).await;
        let expected = NextAlarm { weekday: at_t.weekday, hour: at_t.hour as u8, minute: at_t.minute as u8 };
        assert_eq!(rx.borrow_and_update().next_alarm, Some(expected));
        // Once its minute is checked, it moves on to the next day's.
        core.check_alarms(T).await;
        let moved = rx.borrow_and_update().next_alarm;
        assert_eq!(moved, next_alarm(&alarms, &at_t));
        assert_ne!(moved, Some(expected));
        // A settings change republishes it at once, without a minute check.
        with_alarms(&mut core, vec![]);
        assert_eq!(rx.borrow_and_update().next_alarm, None);
        with_alarms(&mut core, alarms);
        assert!(rx.borrow_and_update().next_alarm.is_some());
    }
}
