//! Everything the core does about updating itself and its plugins.
//!
//! Split by responsibility rather than by layer: what a release says
//! (`release`), what an archive contains (`archive`), how bytes get onto the
//! disk (`download`), when that happens by itself (`schedule`), and what the
//! page is told (`routes`).

// No caller yet: the HTTP client that drives `latest_url`, `classify_asset`,
// `parse_checksums` and `differs` arrives in a later task. Until then only
// this module's own tests reach its public items, and a binary crate (unlike
// a library) does not treat `pub` as "reachable from outside" on its own.
#[allow(dead_code)]
pub mod release;
