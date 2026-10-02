//! What `ritornello-install` recorded placing, read and never written.
//!
//! `/var/lib/ritornello-install/installed.toml` is root's file (root:root
//! 0644): the installer writes it, over ssh, as root. The core reads it for
//! one question only — are the privileged files a new archive of a
//! privileged plugin carries byte-identical to the ones the installer
//! placed? — which is what lets the web UI update such a plugin without a
//! root step beyond placing its binary (see
//! `update::privileged_update_allowed`).
//!
//! A parser of its own, and deliberately not the installer's crate: the
//! core is not linked with the program that runs as root on a device over
//! ssh, and needs three of its fields. Lenient where the installer is
//! strict (`deny_unknown_fields` there, not here), because an unknown field
//! only ever reaches this reader from a newer installer, and what decides
//! is `format` and the two fields below — any failure to read them answers
//! "cannot prove unchanged", which refuses, exactly as before this reader
//! existed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where the registry lives, below the worker's filesystem root. The same
/// literal as `ritornello-install`'s `names::REGISTRY`.
const REGISTRY: &str = "var/lib/ritornello-install/installed.toml";

/// The only format this reader understands, the installer's own.
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct InstallRegistry {
    pub format: u32,
    #[serde(default)]
    pub components: BTreeMap<String, Recorded>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Recorded {
    /// Every privileged dest the installer placed for this component.
    #[serde(default)]
    pub privileged: Vec<String>,
    /// Per privileged dest, the lowercase hex sha256 of what was placed. A
    /// registry written before the installer recorded hashes has none, and
    /// a dest without one proves nothing.
    #[serde(default)]
    pub sha256: BTreeMap<String, String>,
}

/// The registry's path below `root` (`/` in service).
pub fn path(root: &Path) -> PathBuf {
    root.join(REGISTRY)
}

/// Parses the registry's text, or `None` for anything but a format-1 file.
pub fn parse(text: &str) -> Option<InstallRegistry> {
    let registry: InstallRegistry = toml::from_str(text).ok()?;
    (registry.format == FORMAT).then_some(registry)
}

/// Reads the registry under `root`. `None` when it is absent, unreadable or
/// unparsable: all three mean the same thing to the one caller — nothing
/// can be proved, so nothing is allowed. A small local file, read once per
/// install of a privileged plugin, from the worker and never from a route.
pub fn read(root: &Path) -> Option<InstallRegistry> {
    let file = path(root);
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let parsed = parse(&text);
            if parsed.is_none() {
                tracing::warn!("update: {} does not parse as a format-1 registry", file.display());
            }
            parsed
        }
        Err(e) => {
            tracing::info!("update: reading {}: {e}", file.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry in the shape `ritornello-install` renders
    /// (`Registry::render`, `toml::to_string_pretty`: a `sha256` sub-table
    /// per component, keyed by quoted dests).
    const RENDERED: &str = r#"format = 1

[components.core]
version = "0.2.0-beta.2"
privileged = ["/etc/systemd/system/ritornello.service"]

[components.core.sha256]
"/etc/systemd/system/ritornello.service" = "aaaa"

[components.files]
version = "0.2.0-beta.2"
privileged = [
    "/etc/systemd/system/ritornello-media-mount.service",
    "/etc/polkit-1/rules.d/51-ritornello-media.rules",
]

[components.files.sha256]
"/etc/polkit-1/rules.d/51-ritornello-media.rules" = "bbbb"
"/etc/systemd/system/ritornello-media-mount.service" = "cccc"
"#;

    #[test]
    fn the_installer_s_rendering_is_read() {
        let r = parse(RENDERED).expect("parses");
        let files = &r.components["files"];
        assert_eq!(files.privileged.len(), 2);
        assert_eq!(files.sha256["/etc/systemd/system/ritornello-media-mount.service"], "cccc");
    }

    #[test]
    fn a_registry_without_hashes_reads_with_none() {
        let text = "format = 1\n[components.files]\nversion = \"0.2.0\"\nprivileged = [\"/x\"]\n";
        let r = parse(text).expect("parses");
        assert!(r.components["files"].sha256.is_empty());
    }

    /// **[MUTATION]**: drop the format check in `parse` — this test fails.
    #[test]
    fn another_format_or_garbage_reads_as_nothing() {
        assert!(parse("format = 2\n").is_none());
        assert!(parse("not toml at all [").is_none());
    }

    #[test]
    fn an_absent_registry_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_none());
        std::fs::create_dir_all(path(dir.path()).parent().unwrap()).unwrap();
        std::fs::write(path(dir.path()), RENDERED).unwrap();
        assert!(read(dir.path()).is_some(), "the path is the one the installer writes");
    }
}
