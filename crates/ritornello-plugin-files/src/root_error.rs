//! The user-facing text of a root refusal.
//!
//! Lives in the plugin and not next to `RootError`: the keys name entries of
//! this plugin's own catalog, and `ritornello-files-mount` is a root helper
//! that must depend on no shared crate (so that its code changes only when
//! its own directory does, which is what the coupled-change guard watches).

use ritornello_files_mount::roots::RootError;
use ritornello_proto::Text;
use std::collections::HashMap;

/// One named parameter, the shape every `RootError` variant needs: a single
/// interpolated value, never a concatenation (see `AGENTS.md`'s rule against
/// a number glued to a label -- the same trap for a path or a name).
fn keyed(key: &str, param: &str, value: &str) -> Text {
    Text::Keyed { key: key.to_string(), params: HashMap::from([(param.to_string(), value.to_string())]) }
}

/// Unresolved refusal surfaced to the user (body of the admin-side
/// refusal), resolved by the core against this plugin's announced catalog.
pub trait RootErrorText {
    fn text(&self) -> Text;
}

impl RootErrorText for RootError {
    fn text(&self) -> Text {
        match self {
            RootError::BadName { name } => keyed("bad_root_name", "name", name),
            RootError::BadHost { host } => keyed("bad_host", "host", host),
            RootError::BadShare { share } => keyed("bad_share", "share", share),
            RootError::BadSubpath { subpath } => keyed("bad_subpath", "path", subpath),
            RootError::DuplicateName { name } => keyed("duplicate_root", "name", name),
            RootError::RelativeLocalPath { path } => keyed("relative_local_path", "path", path),
        }
    }
}
