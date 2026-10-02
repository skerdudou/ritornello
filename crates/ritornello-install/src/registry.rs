//! The device registry: what `ritornello-install` itself remembers having
//! placed on the device, kept beside the inventory so an update or an
//! uninstall knows what to remove without re-deriving it from a release
//! archive it may no longer have on hand.

use std::collections::BTreeMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// One component the installer has placed on the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub version: String,
    /// The privileged files this component placed. Recorded so an
    /// uninstall knows exactly what a root step must remove, without
    /// trusting a stale or absent inventory to still describe a component
    /// it may have moved past.
    pub privileged: Vec<String>,
    /// Per privileged dest, the lowercase hex sha256 of the exact bytes this
    /// installer placed there: the archive member `put` copied. The core
    /// reads it, and only reads it, to prove that an update of a privileged
    /// plugin leaves every root-owned file byte-identical — the one case in
    /// which the web UI may update such a plugin (see the core's
    /// `update::privileged_update_allowed`). A dest with no hash here proves
    /// nothing, so the core refuses; that is what a registry written before
    /// this field existed parses to (`default`: an empty map).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sha256: BTreeMap<String, String>,
}

/// `/var/lib/ritornello-install/installed.toml`: everything the installer
/// has placed on this device, format 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub format: u32,
    pub components: BTreeMap<String, Recorded>,
}

impl Registry {
    /// Parses `installed.toml`, refusing anything but format 1 by name —
    /// the same rule as `Inventory::parse`, and for the same reason: a
    /// later format's fields must never be read partway as this one's.
    pub fn parse(text: &str) -> anyhow::Result<Registry> {
        let registry: Registry =
            toml::from_str(text).context("installed.toml does not parse")?;
        anyhow::ensure!(
            registry.format == 1,
            "installed.toml declares format {}, this installer only understands format 1",
            registry.format
        );
        Ok(registry)
    }

    /// Renders the registry back to TOML, in the shape `parse` reads.
    pub fn render(&self) -> String {
        toml::to_string_pretty(self).expect("Registry serialises: every field is plain TOML data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Registry {
        let mut components = BTreeMap::new();
        components.insert(
            "radio".to_string(),
            Recorded { version: "0.2.0".to_string(), privileged: vec![], sha256: BTreeMap::new() },
        );
        components.insert(
            "files".to_string(),
            Recorded {
                version: "0.2.1".to_string(),
                privileged: vec![
                    "/etc/systemd/system/ritornello-media-mount.service".to_string(),
                    "/etc/polkit-1/rules.d/51-ritornello-media.rules".to_string(),
                ],
                sha256: [
                    ("/etc/systemd/system/ritornello-media-mount.service".to_string(), "a".repeat(64)),
                    ("/etc/polkit-1/rules.d/51-ritornello-media.rules".to_string(), "b".repeat(64)),
                ]
                .into(),
            },
        );
        Registry { format: 1, components }
    }

    /// The hashes survive the trip too: a render that dropped them would
    /// leave the core nothing to compare, and every update of `files`
    /// refused from the UI without a word of why.
    #[test]
    fn a_registry_round_trips_through_render_and_parse() {
        let original = sample();
        let rendered = original.render();
        let parsed = Registry::parse(&rendered).expect("a freshly rendered registry parses");
        assert_eq!(parsed, original);
        assert_eq!(parsed.components["files"].sha256.len(), 2, "{rendered}");
    }

    /// A registry written before the hashes existed still parses, with an
    /// empty map: the core then reads it as "cannot prove unchanged" and
    /// refuses, exactly as before this field.
    ///
    /// **[MUTATION]**: drop `default` from the `sha256` field — this test
    /// fails, since the field would then be required.
    #[test]
    fn a_registry_written_before_the_hashes_still_parses() {
        let text = "format = 1\n\n[components.files]\nversion = \"0.2.0\"\n\
                    privileged = [\"/etc/systemd/system/ritornello-media-mount.service\"]\n";
        let parsed = Registry::parse(text).expect("an older registry parses");
        assert!(parsed.components["files"].sha256.is_empty());
        assert_eq!(parsed.components["files"].privileged.len(), 1);
    }

    /// **[MUTATION]**: drop the `format == 1` check from `Registry::parse`
    /// — this test fails, since format 2 would then be accepted silently.
    #[test]
    fn a_registry_of_a_later_format_is_refused() {
        let mut later = sample();
        later.format = 2;
        let rendered = later.render();
        let err = Registry::parse(&rendered).unwrap_err();
        assert!(err.to_string().contains('2'), "{err}");
    }

    /// **[MUTATION]**: drop `#[serde(deny_unknown_fields)]` from `Registry`
    /// — this test fails, since the unknown field would then be silently
    /// dropped instead of refused.
    #[test]
    fn an_unknown_field_is_refused_rather_than_silently_dropped() {
        let text = "format = 1\nunknown_field = true\n\n[components]\n";
        assert!(Registry::parse(text).is_err());
    }
}
