//! What `ritornello-install` recorded placing, read and never written.
//!
//! `/var/lib/ritornello-install/installed.toml` is root's file (root:root
//! 0644): the installer writes it, over ssh, as root. The core reads it for
//! one question only — which version of a plugin's companion
//! (`plugins::COMPANIONS`) is installed? — which is what lets the web UI
//! update that plugin while the companion does not move (see
//! `update::companion_allows`).
//!
//! A parser of its own, and deliberately not the installer's crate: the
//! core is not linked with the program that runs as root on a device over
//! ssh, and needs one of its fields. Lenient where the installer is strict
//! (`deny_unknown_fields` there, not here), because an unknown field only
//! ever reaches this reader from a newer installer, and what decides is
//! `format` and `version` — any failure to read them answers "unknown",
//! which refuses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where the registry lives, below the worker's filesystem root. The same
/// literal as `ritornello-install`'s `names::REGISTRY`.
const REGISTRY: &str = "var/lib/ritornello-install/installed.toml";

/// The only format this reader understands, the installer's own.
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct InstallRegistry {
    format: u32,
    #[serde(default)]
    components: BTreeMap<String, Recorded>,
}

/// One component's record. `privileged`, the installer's other field, is
/// not read: nothing here depends on which files were placed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct Recorded {
    version: String,
}

/// The registry's path below `root` (`/` in service).
pub fn path(root: &Path) -> PathBuf {
    root.join(REGISTRY)
}

/// The version a parsed registry records for `component`, if it records one.
fn recorded_version(registry: &InstallRegistry, component: &str) -> Option<String> {
    registry.components.get(component).map(|r| r.version.clone())
}

/// The registry's text, or `None` for anything but a format-1 file.
fn parse(text: &str) -> Option<InstallRegistry> {
    let registry: InstallRegistry = toml::from_str(text).ok()?;
    if registry.format != FORMAT {
        return None;
    }
    Some(registry)
}

/// The version `ritornello-install` recorded placing for `companion`, below
/// `root`. `None` when the registry is absent, unreadable, unparsable, or
/// silent about it: all four mean the same thing to the one caller — the
/// installed version is unknown, so nothing is allowed. A small local file,
/// read once per install of a plugin that has a companion, from the worker
/// and never from a route.
pub fn companion_version(root: &Path, companion: &str) -> Option<String> {
    let file = path(root);
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            // Two different facts, said as two: a file this reader cannot
            // understand, and a readable one that does not record it.
            let Some(registry) = parse(&text) else {
                tracing::warn!("update: {} does not parse as a format-1 registry", file.display());
                return None;
            };
            let version = recorded_version(&registry, companion);
            if version.is_none() {
                tracing::info!("update: {} records no version for {companion}", file.display());
            }
            version
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

    /// `parse` then `recorded_version`, the two halves `companion_version`
    /// runs on the file's text.
    fn version_in(text: &str, component: &str) -> Option<String> {
        recorded_version(&parse(text)?, component)
    }

    /// A registry in the shape `ritornello-install` renders
    /// (`Registry::render`, `toml::to_string_pretty`), with the companion
    /// recorded under its own name as Task 3's installer writes it.
    const RENDERED: &str = r#"format = 1

[components.core]
version = "0.2.0-beta.2"
privileged = ["/etc/systemd/system/ritornello.service"]

[components.files]
version = "0.2.0-beta.3"
privileged = []

[components.files-mount]
version = "0.2.0-beta.2"
privileged = [
    "/usr/local/lib/ritornello/ritornello-media-mount",
    "/etc/systemd/system/ritornello-media-mount.service",
    "/etc/polkit-1/rules.d/51-ritornello-media.rules",
]
"#;

    /// **[MUTATION]**: read `components.files` instead of the component
    /// asked for — red on the version.
    #[test]
    fn the_installer_s_rendering_is_read() {
        assert_eq!(version_in(RENDERED, "files-mount").as_deref(), Some("0.2.0-beta.2"));
        assert_eq!(version_in(RENDERED, "files").as_deref(), Some("0.2.0-beta.3"));
    }

    /// The shape `ritornello-install` writes since it records every
    /// component it places, so that a re-run can leave alone what is up to
    /// date: every plugin, most with no privileged file, and each language
    /// pack under its pack id. Rendered by the installer's own
    /// `Registry::render` for core, radio, files (with its companion) and
    /// French. The extra records change nothing for the one question asked
    /// here.
    const RENDERED_EVERY_COMPONENT: &str = r#"format = 1

[components.core]
version = "0.3.0"
privileged = ["/etc/systemd/system/ritornello.service"]

[components.files]
version = "0.3.1"
privileged = []

[components.files-mount]
version = "0.3.0"
privileged = [
    "/etc/systemd/system/ritornello-media-mount.service",
    "/etc/polkit-1/rules.d/51-ritornello-media.rules",
    "/usr/local/lib/ritornello/ritornello-media-mount",
]

[components.radio]
version = "0.3.2"
privileged = []

[components.ritornello-lang-fr]
version = "0.3.0"
privileged = []
"#;

    #[test]
    fn a_registry_recording_every_component_and_pack_is_still_read() {
        assert_eq!(version_in(RENDERED_EVERY_COMPONENT, "files-mount").as_deref(), Some("0.3.0"));
        assert_eq!(version_in(RENDERED_EVERY_COMPONENT, "radio").as_deref(), Some("0.3.2"));
        assert_eq!(version_in(RENDERED_EVERY_COMPONENT, "ritornello-lang-fr").as_deref(), Some("0.3.0"));
    }

    /// A registry that does not record the companion — one written before
    /// the companion existed — reads as unknown, never as a match.
    #[test]
    fn a_registry_silent_about_the_companion_reads_as_unknown() {
        let text = "format = 1\n[components.files]\nversion = \"0.2.0\"\nprivileged = [\"/x\"]\n";
        assert_eq!(version_in(text, "files-mount"), None);
    }

    /// **[MUTATION]**: drop the format check in `version_in` — this test
    /// fails.
    #[test]
    fn another_format_or_garbage_reads_as_nothing() {
        let format_2 = RENDERED.replace("format = 1", "format = 2");
        assert_eq!(version_in(&format_2, "files-mount"), None);
        assert_eq!(version_in("not toml at all [", "files-mount"), None);
    }

    #[test]
    fn an_absent_registry_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(companion_version(dir.path(), "files-mount"), None);
        std::fs::create_dir_all(path(dir.path()).parent().unwrap()).unwrap();
        std::fs::write(path(dir.path()), RENDERED).unwrap();
        assert_eq!(
            companion_version(dir.path(), "files-mount").as_deref(),
            Some("0.2.0-beta.2"),
            "the path is the one the installer writes"
        );
    }
}
