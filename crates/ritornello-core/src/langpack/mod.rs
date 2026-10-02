//! Language packs: text a device installs, updates and removes on its own.
//!
//! **Nothing here ever reaches the privileged installer.** A pack carries no
//! binary, so `update::Worker::install_one` is not its path, no
//! `ritornello_updater::request::Action` is ever formed for it, and
//! `crates/ritornello-updater/src/target.rs` keeps its two -- and only two --
//! path shapes. The core writes a pack with its own, unprivileged hands,
//! into a root it alone chooses; the archive never names a destination.

pub mod archive;
pub mod store;

/// Where installed packs live. One directory per pack, each holding its own
/// `pack.toml` and one `<module>.toml` per module it covers.
///
/// Its own root, separate from anything a component archive writes: an
/// install must never overwrite what it did not put there, and a removal
/// must remove exactly what it installed.
pub const DEFAULT_PACKS_ROOT: &str = "/etc/ritornello/language-packs";

/// The environment variable that moves that root, for tests and for the e2e
/// harness.
pub const PACKS_ROOT_ENV: &str = "RITORNELLO_LANGUAGE_PACKS";
