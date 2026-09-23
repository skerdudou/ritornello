//! `ritornello-install`: installs, updates and removes Ritornello on a
//! device over ssh, from a workstation. This is the foundation laid by
//! task 9 — names and places, the release inventory, and the device
//! registry — wired into an actual command by later tasks.

// Not called from `main` yet: wired by task 15. `cfg_attr(not(test), ...)`
// rather than a bare `expect`, since each module's own tests already use
// its contents, which would leave the expectation unfulfilled there.
#[cfg_attr(not(test), expect(dead_code, reason = "wired by task 15"))]
mod inventory;
#[cfg_attr(not(test), expect(dead_code, reason = "wired by task 15"))]
mod names;
#[cfg_attr(not(test), expect(dead_code, reason = "wired by task 15"))]
mod registry;

fn main() {}
