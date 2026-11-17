//! Whether the remote lock has an effect anywhere: the count of connected
//! input relays whose plugin opted in, published as
//! `PlayerState::input_lock_available`.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The number of connected input relays whose plugin announced
/// `Announcement::input_lock`, shared between `main`'s relays and the core.
///
/// Without it the page's padlock lied: with generic-input disabled or not yet
/// reconnected, the page said locked and played the chime, and nothing was
/// locked. A count rather than a set of names: two incarnations of the same
/// plugin can overlap while one is rewired, and each holds its own share
/// until its relay ends.
///
/// Shared, not sent: a relay counts itself in synchronously when spawned and
/// out when its task ends, and whoever publishes next reads the total. The
/// two places that change it publish right after — `hotplug` once the relay
/// is spawned, the `unreachable_rx` arm once a relay reported its end (the
/// relay leaves the count **before** it reports, see `spawn_input_relay`).
#[derive(Clone, Default)]
pub struct InputLockRelays(Arc<AtomicUsize>);

/// One relay's share of `InputLockRelays`, given back when dropped: the end
/// of the relay's task is the one signal that holds whatever the cause — EOF,
/// a failed connection, a panic, the core's channel closing.
#[must_use = "the relay is counted only while its share is held"]
pub struct InputLockShare(Arc<AtomicUsize>);

impl InputLockRelays {
    /// Counts one more relay in, until the returned share is dropped.
    pub fn join(&self) -> InputLockShare {
        self.0.fetch_add(1, Ordering::SeqCst);
        InputLockShare(self.0.clone())
    }

    /// At least one relay whose plugin honours the lock is connected.
    pub fn any(&self) -> bool {
        self.0.load(Ordering::SeqCst) > 0
    }
}

impl Drop for InputLockShare {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl<P: Player> Core<P> {
    /// The count the core reads when it publishes, for `main` to hand to the
    /// relays it spawns after the core exists (`hotplug`).
    pub fn input_lock_relays(&self) -> InputLockRelays {
        self.input_lock_relays.clone()
    }

    /// Adopts the count the startup relays joined: they are spawned while the
    /// plugins are gathered, before `Core::new`.
    pub fn set_input_lock_relays(&mut self, relays: InputLockRelays) {
        self.input_lock_relays = relays;
        self.publish_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_counts_until_it_is_dropped() {
        let relays = InputLockRelays::default();
        assert!(!relays.any());
        let a = relays.join();
        let b = relays.clone().join();
        assert!(relays.any());
        drop(a);
        assert!(relays.any(), "one relay is still connected");
        drop(b);
        assert!(!relays.any(), "the last relay gone, nothing honours the lock");
    }
}
