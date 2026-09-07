//! What the unprivileged side is allowed to ask, and nothing more.
//!
//! The whole file is written from one assumption: **the core may be
//! compromised**. Whoever reaches the web UI reaches the core, and the core is
//! what writes this request. So this grammar is not a convenience for a
//! trusted caller — it is the boundary. Every string it carries is turned into
//! a path by joining, which is why `valid_name` forbids the separator, the dot
//! and everything else that can steer a join, instead of trying to detect
//! traversal after the fact.

use serde::{Deserialize, Serialize};

/// Version of this request format, refused when it does not match.
///
/// A core and an installer can be at different versions: the installer is
/// deliberately excluded from what an update may replace, so it is the one
/// piece that only ever changes by hand. Refusing an unknown format is how a
/// newer core finds out, instead of having its intent guessed.
pub const REQUEST_FORMAT: u32 = 1;

/// Longest name accepted. No filesystem constraint behind this number, only
/// the refusal to let an unbounded string through.
const NAME_MAX: usize = 64;

/// One request, one run of the privileged binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub format: u32,
    pub actions: Vec<Action>,
}

/// `staged` and `file` are **bare names**, never paths: `staged` names a file
/// the core wrote in the staging directory, `file` names the binary as it will
/// exist in the plugins directory. Both go through `valid_name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "op", rename_all = "snake_case")]
pub enum Action {
    PlaceCore { staged: String },
    PlacePlugin { file: String, staged: String },
    /// Never `RemoveCore`: erasing the core binary is not a gesture this
    /// product has, and the absence of the variant is how it stays impossible.
    RemovePlugin { file: String },
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`, hand-rolled so as not to pull a regex crate
/// into a root binary.
///
/// No dot: it is what makes `.` and `..` unrepresentable rather than
/// specially-cased. No separator, no uppercase, no non-ASCII — a Unicode
/// lookalike has no business naming a binary here.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > NAME_MAX {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hostile_name_is_refused() {
        for name in tests_support::HOSTILE {
            assert!(!valid_name(name), "{name:?} was accepted");
        }
    }

    #[test]
    fn the_names_we_actually_ship_are_accepted() {
        for name in [
            "ritornello-plugin-radio",
            "ritornello-plugin-nrj-metas",
            "ritornello-plugin-generic-input",
            "someone-elses-plugin",
            "p",
            "plugin9",
        ] {
            assert!(valid_name(name), "{name:?} was refused");
        }
    }

    #[test]
    fn a_name_longer_than_the_cap_is_refused() {
        assert!(!valid_name(&"a".repeat(65)));
        assert!(valid_name(&"a".repeat(64)));
    }
}

/// The hostile name set, shared with `target`'s tests.
///
/// `pub` and `#[cfg(test)]`: two modules must prove the same property against
/// the same inputs, and a second copy of this list is a copy that drifts.
#[cfg(test)]
pub mod tests_support {
    pub const HOSTILE: &[&str] = &[
        "",
        ".",
        "..",
        "../../etc/polkit-1/rules.d/50-ritornello-power.rules",
        "/etc/passwd",
        "a/b",
        "a\\b",
        "ritornello-plugin-radio/../../bin/ritornello-core",
        "ritornello.plugin.radio",
        "Ritornello-Plugin-Radio",
        "-leading-dash",
        "with space",
        "with\ttab",
        "with\nnewline",
        "nul\0byte",
        "ritornello-plugin-café",
    ];
}
