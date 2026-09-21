//! Language packs: text a device installs, updates and removes on its own.
//!
//! **Nothing here ever reaches the privileged installer.** A pack carries no
//! binary, so `update::Worker::install_one` is not its path, no
//! `ritornello_updater::request::Action` is ever formed for it, and
//! `crates/ritornello-updater/src/target.rs` keeps its two -- and only two --
//! path shapes. The core writes a pack with its own, unprivileged hands,
//! into a root it alone chooses; the archive never names a destination.

pub mod archive;

/// Where installed packs live. One directory per pack, each holding its own
/// `pack.toml` and one `<module>.toml` per module it covers.
///
/// Deliberately **not** under the operator's own locales root: that one holds
/// what a person wrote by hand, and an install must never write there. The
/// separation is what lets a removal be exact and a hand-written pack
/// survive -- which it does not today, where an update overwrites it.
#[expect(dead_code, reason = "consumed by langpack::store, task 5")]
pub const DEFAULT_PACKS_ROOT: &str = "/etc/ritornello/language-packs";

/// The environment variable that moves that root, for tests and for the e2e
/// harness. Same idiom as `RITORNELLO_LOCALES`.
#[expect(dead_code, reason = "consumed by the language pack registry, task 6")]
pub const PACKS_ROOT_ENV: &str = "RITORNELLO_LANGUAGE_PACKS";
