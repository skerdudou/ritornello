//! The two — and only two — paths this binary can form.
//!
//! Read this file as the security boundary it is. There is no table to
//! maintain and no name to keep in sync with the rest of the repository:
//! `/usr/local/bin/ritornello-core` is a constant, and everything else is the
//! plugins directory joined with a name that went through
//! `request::valid_name`. Adding a third location would be a design decision,
//! not an oversight to fix.

use crate::request::{valid_name, Action};
use std::path::{Path, PathBuf};

/// Why a target could not be formed. Carried to the log verbatim: an operator
/// reading the journal after a refusal needs the name that was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// The name did not survive `valid_name`.
    InvalidName(String),
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(n) => write!(f, "refusing the name {n:?}: it is not a bare lowercase identifier"),
        }
    }
}

impl std::error::Error for TargetError {}

/// `<prefix>/usr/local/lib/ritornello/plugins`.
///
/// `prefix` exists for one reason and it is not flexibility: it is what lets
/// every test drive the real code against a temporary directory. In service it
/// is always `/`.
pub fn plugins_dir(prefix: &Path) -> PathBuf {
    prefix.join("usr/local/lib/ritornello/plugins")
}

/// `<prefix>/usr/local/bin/ritornello-core`.
pub fn core_binary(prefix: &Path) -> PathBuf {
    prefix.join("usr/local/bin/ritornello-core")
}

/// The path an action names, or a refusal.
pub fn target_of(prefix: &Path, action: &Action) -> Result<PathBuf, TargetError> {
    match action {
        Action::PlaceCore { .. } => Ok(core_binary(prefix)),
        Action::PlacePlugin { file, .. } | Action::RemovePlugin { file } => {
            if !valid_name(file) {
                return Err(TargetError::InvalidName(file.clone()));
            }
            Ok(plugins_dir(prefix).join(file))
        }
    }
}

/// Where the core staged a file, for an action that carries one.
///
/// Separate from `target_of` so the two can never be confused at a call site:
/// one is read, the other is written.
pub fn staged_of(staging: &Path, action: &Action) -> Result<Option<PathBuf>, TargetError> {
    let staged = match action {
        Action::PlaceCore { staged } | Action::PlacePlugin { staged, .. } => staged,
        Action::RemovePlugin { .. } => return Ok(None),
    };
    if !valid_name(staged) {
        return Err(TargetError::InvalidName(staged.clone()));
    }
    Ok(Some(staging.join(staged)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn under(prefix: &Path, child: &Path) -> bool {
        // Lexical, on purpose: `target_of` builds paths by joining validated
        // names, so there is nothing to canonicalize away. Calling
        // `canonicalize` here would ALSO make the test pass for a path that
        // exists, hiding the very traversal we are proving impossible.
        child.starts_with(prefix)
    }

    #[test]
    fn no_hostile_name_escapes_the_plugins_directory() {
        let prefix = Path::new("/tmp/root");
        for name in crate::request::tests_support::HOSTILE {
            let action = Action::PlacePlugin {
                file: (*name).to_string(),
                staged: "staged-plugin".to_string(),
            };
            match target_of(prefix, &action) {
                Err(_) => {}
                Ok(path) => panic!("{name:?} produced {path:?} instead of being refused"),
            }
        }
    }

    #[test]
    fn a_valid_plugin_name_lands_in_the_plugins_directory_and_nowhere_else() {
        let prefix = Path::new("/tmp/root");
        let action = Action::PlacePlugin {
            file: "ritornello-plugin-radio".to_string(),
            staged: "staged-plugin".to_string(),
        };
        let path = target_of(prefix, &action).expect("a valid name resolves");
        assert_eq!(path, prefix.join("usr/local/lib/ritornello/plugins/ritornello-plugin-radio"));
        assert!(under(&plugins_dir(prefix), &path));
    }

    #[test]
    fn the_core_target_is_a_constant_and_ignores_every_string_in_the_action() {
        let prefix = Path::new("/tmp/root");
        let action = Action::PlaceCore { staged: "staged-core".to_string() };
        assert_eq!(target_of(prefix, &action).unwrap(), prefix.join("usr/local/bin/ritornello-core"));
    }

    /// The property that matters most, stated as a test rather than as a
    /// comment: neither a polkit rule nor a unit file is expressible.
    #[test]
    fn no_action_can_ever_name_a_polkit_rule_or_a_unit() {
        let prefix = Path::new("/tmp/root");
        let forbidden = [
            prefix.join("etc/polkit-1/rules.d/50-ritornello-power.rules"),
            prefix.join("etc/polkit-1/rules.d/51-ritornello-media.rules"),
            prefix.join("etc/polkit-1/rules.d/52-ritornello-update.rules"),
            prefix.join("etc/systemd/system/ritornello.service"),
            prefix.join("etc/systemd/system/ritornello-update.service"),
            prefix.join("usr/local/lib/ritornello/ritornello-media-mount"),
            prefix.join("usr/local/lib/ritornello/ritornello-update"),
        ];
        // Every name that validates, joined into either location, and none of
        // them can equal one of the paths above — the plugins directory and
        // /usr/local/bin/ritornello-core are simply elsewhere.
        for name in ["ritornello-media-mount", "ritornello-update", "ritornello", "50-ritornello-power"] {
            let candidates = [
                Action::PlacePlugin { file: name.to_string(), staged: "s".to_string() },
                Action::RemovePlugin { file: name.to_string() },
            ];
            for action in candidates {
                if let Ok(path) = target_of(prefix, &action) {
                    assert!(
                        !forbidden.contains(&path),
                        "{name:?} reached {path:?}"
                    );
                    assert!(under(&plugins_dir(prefix), &path));
                }
            }
        }
    }
}
