//! Everything the core does about updating itself and its plugins.
//!
//! Split by responsibility rather than by layer: what a release says
//! (`release`), what an archive contains (`archive`), how bytes get onto the
//! disk (`download`), when that happens by itself (`schedule`), and what the
//! page is told (`routes`).

// No caller yet: the HTTP client that drives `releases_url`, `fold`,
// `parse_checksums` and `differs` arrives in a later task. Until then only
// this module's own tests reach its public items, and a binary crate (unlike
// a library) does not treat `pub` as "reachable from outside" on its own.
#[allow(dead_code)]
pub mod release;

// Same story: `read` and `installable_from_ui` get their first caller in a
// later task.
#[allow(dead_code)]
pub mod archive;

// Same story: the download path gets its first caller in a later task.
#[allow(dead_code)]
pub mod download;

// `component_offers` and its types get their first caller outside this
// module's own tests in Task 12 (the check gesture) and Task 13/18 (the
// install and third-party gestures).
#[allow(dead_code)]
pub mod state;

pub mod routes;

/// What the update worker is asked to do. One enum rather than one channel per
/// gesture: they are serialised against each other on purpose — two installs
/// at once would race on the staging directory.
#[derive(Debug, Clone)]
pub enum Job {
    Check,
}
