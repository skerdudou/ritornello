//! When an automatic check or install happens.
//!
//! The decision is a **pure function** and the clock is a thin wrapper around
//! `libc`, because that is where the bugs live: an hour that does not exist on
//! a spring-forward day, one that happens twice in autumn, midnight, a weekly
//! cadence, a device that was off when its hour passed. Every one of those is
//! a table entry below rather than something to reason about.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePolicy {
    /// Nothing happens by itself. The default: a device that starts phoning
    /// home because it was updated is not a behaviour to inherit silently.
    #[default]
    Off,
    Check,
    CheckAndInstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    /// Maps `tm_wday` (0 for Sunday, as `localtime_r` gives it) to a
    /// `Weekday`.
    ///
    /// A pure function over `0..=6`, deliberately pulled out of `local_now`:
    /// it is the one piece of logic in this file's only untested function
    /// that is not time-zone-dependent, and an off-by-one here would mean a
    /// weekly cadence silently firing on the wrong day, every week, forever.
    ///
    /// `None` outside `0..=6` — `localtime_r` never produces such a value,
    /// but the signature admits one, and answering `None` rather than
    /// panicking or guessing keeps that promise honest for any other caller.
    fn from_tm(wday: i32) -> Option<Self> {
        Some(match wday {
            0 => Self::Sunday,
            1 => Self::Monday,
            2 => Self::Tuesday,
            3 => Self::Wednesday,
            4 => Self::Thursday,
            5 => Self::Friday,
            6 => Self::Saturday,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "day")]
pub enum UpdateCadence {
    #[default]
    Daily,
    Weekly(Weekday),
}

/// The local wall clock, decomposed. Everything the decision needs and nothing
/// more, so it can be built by hand in a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNow {
    pub hour: u32,
    pub minute: u32,
    pub weekday: Weekday,
    /// Identifies the local day. See `day_key`.
    pub day_key: i64,
}

/// An **identity** for a local day, not an ordinal.
///
/// Equality is all the decision needs — "has today already had its turn" — so
/// there is no reason to write days-from-civil arithmetic and no reason to be
/// able to order two keys. `tm_year` (years since 1900) and `tm_yday` (0-365),
/// combined so no two days collide, which a plain sum would not achieve across
/// a leap year boundary.
pub fn day_key(tm_year: i32, tm_yday: i32) -> i64 {
    i64::from(tm_year) * 1000 + i64::from(tm_yday)
}

/// The local wall clock for a Unix instant, honouring `/etc/localtime` and
/// summer time.
///
/// `localtime_r` through `libc`, already a dependency: no date crate enters
/// the graph for this. `None` when the conversion fails, which the caller
/// treats as "do nothing this minute" — a clock that cannot be read is not a
/// reason to update at an unexpected time.
///
/// **Deliberately untested.** Exercising this function for real means
/// depending on the machine's time zone — the exact trap a threshold
/// elsewhere in this repository turned out to be measuring instead of the
/// code it claimed to test — or mocking `localtime_r`, which nothing here
/// attempts. That risk used to include the `tm_wday` → `Weekday` mapping,
/// which is exactly the kind of thing an untested function should not be
/// allowed to hide; it has been pulled out to `Weekday::from_tm` and is
/// tested directly, by name, for all seven days. What is left in this
/// function — the FFI call and two numeric conversions `localtime_r` cannot
/// make fail in practice — is not logic worth a test of its own.
pub fn local_now(unix_s: i64) -> Option<LocalNow> {
    // SAFETY: `tm` is fully initialised by `localtime_r`, which is given a
    // valid pointer to it and to `unix_s`. The `_r` variant is the reentrant
    // one: no shared static, so nothing here races with another thread
    // formatting a time.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = unix_s as libc::time_t;
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(LocalNow {
        hour: u32::try_from(tm.tm_hour).ok()?,
        minute: u32::try_from(tm.tm_min).ok()?,
        weekday: Weekday::from_tm(tm.tm_wday)?,
        day_key: day_key(tm.tm_year, tm.tm_yday),
    })
}

/// Is a run due at this minute?
///
/// `>= hour` and not `== hour`, and that single choice covers two cases at
/// once: a device that was off when its hour passed catches up at its next
/// tick, and an hour that does not exist on a spring-forward day still fires.
/// `last_run_day` is what keeps it to once — including on the autumn day when
/// the hour happens twice.
pub fn due(
    now: &LocalNow,
    policy: UpdatePolicy,
    hour: u32,
    cadence: UpdateCadence,
    last_run_day: Option<i64>,
) -> bool {
    if policy == UpdatePolicy::Off {
        return false;
    }
    if let UpdateCadence::Weekly(day) = cadence
        && now.weekday != day
    {
        return false;
    }
    if now.hour < hour {
        return false;
    }
    last_run_day != Some(now.day_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32, minute: u32, weekday: Weekday, day: i64) -> LocalNow {
        LocalNow { hour, minute, weekday, day_key: day }
    }

    /// Seven stated pairs, not a loop over `0..7` mapped by the same rule
    /// the function under test uses: a loop like that would reimplement
    /// `from_tm` to check itself and could not catch an off-by-one shared by
    /// both. Each pair here is the one and only place that says what `2`
    /// means, so a swap — Tuesday and Wednesday trading places, say — has
    /// somewhere to be caught.
    #[test]
    fn from_tm_maps_every_day_of_the_week() {
        assert_eq!(Weekday::from_tm(0), Some(Weekday::Sunday));
        assert_eq!(Weekday::from_tm(1), Some(Weekday::Monday));
        assert_eq!(Weekday::from_tm(2), Some(Weekday::Tuesday));
        assert_eq!(Weekday::from_tm(3), Some(Weekday::Wednesday));
        assert_eq!(Weekday::from_tm(4), Some(Weekday::Thursday));
        assert_eq!(Weekday::from_tm(5), Some(Weekday::Friday));
        assert_eq!(Weekday::from_tm(6), Some(Weekday::Saturday));
    }

    /// `localtime_r` never produces these, but the function's signature
    /// admits any `i32` — `None` here is the documented answer, not silence.
    #[test]
    fn from_tm_rejects_what_localtime_r_never_produces() {
        assert_eq!(Weekday::from_tm(7), None);
        assert_eq!(Weekday::from_tm(-1), None);
    }

    #[test]
    fn nothing_is_due_when_the_policy_is_off() {
        let now = at(3, 0, Weekday::Wednesday, 100);
        assert!(!due(&now, UpdatePolicy::Off, 3, UpdateCadence::Daily, None));
    }

    #[test]
    fn the_scheduled_hour_fires_once_and_then_not_again_that_day() {
        let now = at(3, 0, Weekday::Wednesday, 100);
        assert!(due(&now, UpdatePolicy::Check, 3, UpdateCadence::Daily, None));
        // Having run, the same day says no — for every one of the 1439 other
        // minutes the ticker will ask.
        assert!(!due(&now, UpdatePolicy::Check, 3, UpdateCadence::Daily, Some(100)));
        assert!(!due(&at(3, 59, Weekday::Wednesday, 100), UpdatePolicy::Check, 3, UpdateCadence::Daily, Some(100)));
        assert!(!due(&at(23, 59, Weekday::Wednesday, 100), UpdatePolicy::Check, 3, UpdateCadence::Daily, Some(100)));
    }

    #[test]
    fn nothing_fires_before_the_scheduled_hour() {
        assert!(!due(&at(2, 59, Weekday::Wednesday, 100), UpdatePolicy::Check, 3, UpdateCadence::Daily, None));
        assert!(!due(&at(0, 0, Weekday::Wednesday, 100), UpdatePolicy::Check, 3, UpdateCadence::Daily, None));
    }

    /// Without this, a device switched off at 3 a.m. would never update — and
    /// this one is a radio in a living room.
    #[test]
    fn a_missed_hour_is_caught_up_at_the_next_tick() {
        assert!(due(&at(9, 17, Weekday::Wednesday, 100), UpdatePolicy::Check, 3, UpdateCadence::Daily, None));
        // Yesterday having run does not count for today.
        assert!(due(&at(9, 17, Weekday::Wednesday, 100), UpdatePolicy::Check, 3, UpdateCadence::Daily, Some(99)));
    }

    #[test]
    fn a_weekly_cadence_only_fires_on_its_day() {
        let policy = UpdatePolicy::CheckAndInstall;
        let cadence = UpdateCadence::Weekly(Weekday::Saturday);
        assert!(!due(&at(3, 0, Weekday::Friday, 100), policy, 3, cadence, None));
        assert!(due(&at(3, 0, Weekday::Saturday, 101), policy, 3, cadence, None));
        assert!(!due(&at(3, 0, Weekday::Sunday, 102), policy, 3, cadence, None));
    }

    /// Spring forward: 02:00 becomes 03:00, so an hour scheduled at 2 never
    /// exists that day. `>=` is what saves it — the tick at 03:00 catches up.
    #[test]
    fn an_hour_that_does_not_exist_on_a_spring_forward_day_still_fires() {
        assert!(due(&at(3, 0, Weekday::Sunday, 120), UpdatePolicy::Check, 2, UpdateCadence::Daily, None));
    }

    /// Fall back: 02:00 happens twice. The day key is what stops the second
    /// pass from running it again.
    #[test]
    fn an_hour_that_happens_twice_on_a_fall_back_day_fires_once() {
        let first = at(2, 30, Weekday::Sunday, 300);
        assert!(due(&first, UpdatePolicy::Check, 2, UpdateCadence::Daily, None));
        let second = at(2, 30, Weekday::Sunday, 300);
        assert!(!due(&second, UpdatePolicy::Check, 2, UpdateCadence::Daily, Some(300)));
    }

    #[test]
    fn the_day_key_only_has_to_identify_a_day_not_order_them() {
        // tm_year and tm_yday as libc gives them. Two different days must
        // differ; the same day must match. Nothing here compares with `<`.
        assert_eq!(day_key(126, 250), day_key(126, 250));
        assert_ne!(day_key(126, 250), day_key(126, 251));
        assert_ne!(day_key(126, 250), day_key(127, 250));
        // The actual trap this shape avoids: a naive `tm_year + tm_yday` sum
        // collides here — 126 + 251 == 127 + 250 == 377 — while multiplying
        // the year keeps them apart. This is the pair that makes a
        // plain-sum mutation of `day_key` fail this test.
        assert_ne!(day_key(126, 251), day_key(127, 250));
        // A year boundary in its own right: the last day of one year against
        // an early day of the next. It happens not to collide under a plain
        // sum either (126 + 366 = 492, 127 + 1 = 128), so it proves nothing
        // about that failure mode — it is kept because the boundary itself
        // is worth pinning regardless.
        assert_ne!(day_key(126, 366), day_key(127, 1));
    }
}
