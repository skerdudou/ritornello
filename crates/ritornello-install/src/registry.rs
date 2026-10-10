//! The device registry: what `ritornello-install` itself remembers having
//! placed on the device, kept beside the inventory so an update or an
//! uninstall knows what to remove without re-deriving it from a release
//! archive it may no longer have on hand.
//!
//! Every component it places is recorded — the core, each plugin of ours,
//! each companion, each language pack under its pack id — with the version
//! placed, even when it places nothing privileged. That version is what
//! lets a later run leave alone what is already up to date, and it may be
//! trusted for that because the file is root's (root:root 0644): nothing
//! the unprivileged account writes can ever make a run skip a component
//! (see `plan::is_current`).

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
    /// Which content each privileged file placed is, by destination: the
    /// inventory's `identity`, recorded as placed. The core compares it with
    /// what a release offers before updating itself from the page, so a
    /// file placed by anything but this installer is never vouched for here.
    /// A registry written before identities existed has none: read as empty,
    /// it makes the next run place the component again (`plan::is_current`),
    /// which records them. Omitted from the file when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub identity: BTreeMap<String, String>,
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
            Recorded { version: "0.2.0".to_string(), privileged: vec![], identity: BTreeMap::new() },
        );
        components.insert(
            "files-mount".to_string(),
            Recorded {
                version: "0.2.1".to_string(),
                privileged: vec![
                    "/etc/systemd/system/ritornello-media-mount.service".to_string(),
                    "/etc/polkit-1/rules.d/51-ritornello-media.rules".to_string(),
                ],
                identity: [
                    ("/etc/systemd/system/ritornello-media-mount.service".to_string(), "sha256:ab".to_string()),
                    ("/etc/polkit-1/rules.d/51-ritornello-media.rules".to_string(), "sha256:cd".to_string()),
                ]
                .into(),
            },
        );
        Registry { format: 1, components }
    }

    /// The exact text this installer writes for a core it placed with
    /// identities. The core's reader is tested on this very text
    /// (`install_registry::tests::RENDERED_WITH_IDENTITIES`); if this
    /// rendering changes, that copy must follow.
    #[test]
    fn the_rendering_with_identities_is_the_text_the_core_is_tested_on() {
        let core = Recorded {
            version: "0.3.0".into(),
            privileged: vec![
                "/etc/systemd/system/ritornello.service".into(),
                "/usr/local/lib/ritornello/ritornello-update".into(),
            ],
            identity: [
                ("/etc/systemd/system/ritornello.service".to_string(), "sha256:6e6b".to_string()),
                ("/usr/local/lib/ritornello/ritornello-update".to_string(), "version:1.0.0".to_string()),
            ]
            .into(),
        };
        let radio = Recorded { version: "0.3.2".into(), privileged: vec![], identity: BTreeMap::new() };
        let registry =
            Registry { format: 1, components: [("core".to_string(), core), ("radio".to_string(), radio)].into() };
        let want = r#"format = 1

[components.core]
version = "0.3.0"
privileged = [
    "/etc/systemd/system/ritornello.service",
    "/usr/local/lib/ritornello/ritornello-update",
]

[components.core.identity]
"/etc/systemd/system/ritornello.service" = "sha256:6e6b"
"/usr/local/lib/ritornello/ritornello-update" = "version:1.0.0"

[components.radio]
version = "0.3.2"
privileged = []
"#;
        assert_eq!(registry.render(), want);
        assert_eq!(Registry::parse(want).unwrap(), registry);
    }

    #[test]
    fn a_registry_round_trips_through_render_and_parse() {
        let original = sample();
        let rendered = original.render();
        let parsed = Registry::parse(&rendered).expect("a freshly rendered registry parses");
        assert_eq!(parsed, original);
    }

    /// The exact text this installer writes for a device with the core,
    /// radio, files (and its companion) and French: every component
    /// recorded, plugins and packs with no privileged file. The core's
    /// reader is tested on this very text
    /// (`install_registry::tests::RENDERED_EVERY_COMPONENT`); if this
    /// rendering changes, that copy must follow.
    #[test]
    fn the_rendering_of_every_component_is_the_text_the_core_is_tested_on() {
        let rec = |v: &str, p: &[&str]| Recorded {
            version: v.into(),
            privileged: p.iter().map(|s| s.to_string()).collect(),
            identity: BTreeMap::new(),
        };
        let registry = Registry {
            format: 1,
            components: [
                ("core", rec("0.3.0", &["/etc/systemd/system/ritornello.service"])),
                ("files", rec("0.3.1", &[])),
                (
                    "files-mount",
                    rec(
                        "0.3.0",
                        &[
                            "/etc/systemd/system/ritornello-media-mount.service",
                            "/etc/polkit-1/rules.d/51-ritornello-media.rules",
                            "/usr/local/lib/ritornello/ritornello-media-mount",
                        ],
                    ),
                ),
                ("radio", rec("0.3.2", &[])),
                ("ritornello-lang-fr", rec("0.3.0", &[])),
            ]
            .into_iter()
            .map(|(n, r)| (n.to_string(), r))
            .collect(),
        };
        let want = "format = 1

[components.core]
version = \"0.3.0\"
privileged = [\"/etc/systemd/system/ritornello.service\"]

[components.files]
version = \"0.3.1\"
privileged = []

[components.files-mount]
version = \"0.3.0\"
privileged = [
    \"/etc/systemd/system/ritornello-media-mount.service\",
    \"/etc/polkit-1/rules.d/51-ritornello-media.rules\",
    \"/usr/local/lib/ritornello/ritornello-media-mount\",
]

[components.radio]
version = \"0.3.2\"
privileged = []

[components.ritornello-lang-fr]
version = \"0.3.0\"
privileged = []
";
        assert_eq!(registry.render(), want.replace("\r\n", "\n"));
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
