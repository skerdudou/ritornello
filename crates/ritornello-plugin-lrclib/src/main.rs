//! `metadata` plugin: completes the lyrics of the playing track from LRCLIB
//! (lrclib.net), and keeps what it found in a bounded disk cache.
//!
//! This file is a skeleton: the lookup, the matching and the cache are built
//! and tested first; the plugin wiring comes on top of them.

// Not yet called from `main`: the wiring that uses them is the next step, and
// `-D warnings` would otherwise refuse the crate.
#[allow(dead_code)]
mod cache;
#[allow(dead_code)]
mod client;
#[allow(dead_code)]
mod matching;

fn main() {}
