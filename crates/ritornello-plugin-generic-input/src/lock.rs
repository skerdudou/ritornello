//! The long press that toggles the remote lock.
//!
//! The plugin times the hold itself rather than counting autorepeats: a
//! remote's receiver may repeat a held key every 30 ms, every 250 ms, or not
//! at all (some IR receivers send one press and one release), and the
//! duration the owner feels must not depend on that. So a press arms a
//! deadline, a release disarms it, and an autorepeat only gives the loop a
//! chance to notice the deadline has passed.
//!
//! `Instant` is tokio's, so the paused clock of the tests drives it.

use std::time::Duration;
use tokio::time::Instant;

/// How long the lock key must stay down before it toggles. Long enough that a
/// pocketed or fumbled remote never locks itself; short enough that an owner
/// does not wonder whether it worked.
pub const LONG_PRESS: Duration = Duration::from_millis(2000);

/// The state of one lock key's current hold, on one device.
#[derive(Default)]
pub struct LockKeyPress {
    held_since: Option<Instant>,
    /// The hold already toggled: further autorepeats of the same hold must
    /// not toggle back.
    fired: bool,
}

impl LockKeyPress {
    /// Value 1: the key went down. (Re)starts the hold.
    pub fn press(&mut self, now: Instant) {
        self.held_since = Some(now);
        self.fired = false;
    }

    /// Value 0: the key went up. Forgets the hold.
    pub fn release(&mut self) {
        self.held_since = None;
        self.fired = false;
    }

    /// When the hold becomes a toggle; `None` when no hold is armed or it has
    /// already fired.
    pub fn deadline(&self) -> Option<Instant> {
        if self.fired {
            return None;
        }
        self.held_since.map(|t| t + LONG_PRESS)
    }

    /// `true` exactly once per hold: the first time `now` reaches the
    /// deadline.
    pub fn due(&mut self, now: Instant) -> bool {
        match self.deadline() {
            Some(deadline) if now >= deadline => {
                self.fired = true;
                true
            }
            _ => false,
        }
    }
}

/// The shortest time between two `InputLockRefused` sent to the core. A
/// child drumming on a locked remote must not re-arm the displays' overlay
/// nor bother the core on every key; one answer every few seconds is enough
/// to say why nothing happens. It does not take LOCKED off the screen: with
/// the default `overlay_ms` (5 s, longer than this interval) the message
/// stays up while keys keep coming, and goes `overlay_ms` after the last
/// refusal sent.
pub const REFUSAL_INTERVAL: Duration = Duration::from_secs(3);

/// When a refused key last made the plugin say so, shared by every device:
/// one answer per interval in all, whichever receiver the press came from.
#[derive(Default)]
pub struct Refusals {
    last_said: Option<Instant>,
}

impl Refusals {
    /// `true` when a refusal at `now` must be sent: the first ever, or one
    /// at least `REFUSAL_INTERVAL` after the last one **sent** (a refusal
    /// kept silent does not push the next one back).
    pub fn say(&mut self, now: Instant) -> bool {
        match self.last_said {
            Some(at) if now < at + REFUSAL_INTERVAL => false,
            _ => {
                self.last_said = Some(now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_said_at_most_once_per_interval() {
        let t0 = Instant::now();
        let mut r = Refusals::default();
        assert!(r.say(t0), "the first is said");
        assert!(!r.say(t0 + Duration::from_millis(1)));
        assert!(!r.say(t0 + REFUSAL_INTERVAL - Duration::from_millis(1)));
        assert!(r.say(t0 + REFUSAL_INTERVAL), "counted from the last one said");
        assert!(!r.say(t0 + REFUSAL_INTERVAL + Duration::from_secs(2)));
        assert!(r.say(t0 + REFUSAL_INTERVAL * 2));
    }

    #[test]
    fn a_press_shorter_than_the_threshold_never_fires() {
        let t0 = Instant::now();
        let mut p = LockKeyPress::default();
        p.press(t0);
        assert!(!p.due(t0 + Duration::from_millis(1999)));
        p.release();
        assert_eq!(p.deadline(), None);
        assert!(!p.due(t0 + Duration::from_secs(5)));
    }

    #[test]
    fn holding_past_the_threshold_toggles_once() {
        let t0 = Instant::now();
        let mut p = LockKeyPress::default();
        p.press(t0);
        assert!(p.due(t0 + Duration::from_millis(2000)));
        assert!(!p.due(t0 + Duration::from_secs(4)));
        assert!(!p.due(t0 + Duration::from_secs(60)));
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn a_new_press_starts_a_new_hold() {
        let t0 = Instant::now();
        let mut p = LockKeyPress::default();
        p.press(t0);
        assert!(p.due(t0 + Duration::from_secs(2)));
        p.release();
        let t1 = t0 + Duration::from_secs(10);
        p.press(t1);
        assert!(p.due(t1 + Duration::from_secs(2)));
    }
}
