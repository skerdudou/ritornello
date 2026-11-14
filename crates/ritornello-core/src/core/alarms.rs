//! Alarm clocks: the pure part. Which alarms are due at a given minute, and
//! which one rings next. No clock, no I/O, so each rule is testable by
//! building a `LocalNow` by hand.

// The scheduler that consumes these lands in the next task; until then only the
// tests call them. Remove this line with that wiring.
#![allow(dead_code)]

use crate::state::Alarm;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AlarmTarget;

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
}
