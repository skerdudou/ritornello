//! The privileged half of Ritornello's updater.
//!
//! It knows how to do exactly one thing, repeated: **place a file at one of
//! two paths it computes itself.** It reads no archive, opens no socket, and
//! has no third path. The polkit rules, the systemd units, the root mount
//! helper and this binary itself are therefore not "excluded from a list" —
//! they are unreachable, because no path this code can form leads to them.
//!
//! Same doctrine as `ritornello-media-mount`, its neighbour: the privileged
//! side revalidates everything rather than trusting the side that asked.

pub mod apply;
pub mod marker;
pub mod request;
pub mod rollback;
pub mod target;
