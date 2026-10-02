//! What the root mount helper and the `files` plugin have in common.
//!
//! The helper (`ritornello-media-mount`, run as root by systemd) and the
//! plugin (unprivileged, writes the configuration) must read exactly the same
//! grammar. This library is that single reading: `roots` (the declared shares),
//! `mount_options` (the options the helper passes to `mount.cifs`) and `mounts`
//! (the `/proc/mounts` table).
//!
//! Anything only the plugin needs stays in the plugin. Anything added here is
//! compiled into a root binary: keep the dependencies few.

pub mod mount_options;
pub mod mounts;
pub mod roots;
