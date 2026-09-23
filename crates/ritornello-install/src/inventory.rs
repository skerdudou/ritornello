//! The release's inventory: what each shipped component installs, read the
//! way `scripts/install-inventory.py` writes it (format 1). A format this
//! installer does not understand is refused by name rather than read
//! partway — a future format's fields would otherwise deserialize through
//! whatever they happen to share with this one, and the installer would
//! place files it never actually described.

use anyhow::Context;
use serde::Deserialize;

/// One file a component's release archive carries.
///
/// Fields beyond `dest` are read only by the placing and removal code later
/// tasks add.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// The member's path inside the archive, without a leading `./`.
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub archive_path: String,
    /// Where it lands on the device: always `"/" + archive_path`.
    pub dest: String,
    /// `"0755"` or `"0644"`, kept as a string — never parsed as an octal
    /// number this crate would have to get right a second time.
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub mode: String,
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub owner: String,
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub privileged: bool,
}

/// One file a component writes only when its target is absent on the
/// device — a fresh install, or the first install of that component.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "wired by task 15")]
pub struct InitialConfig {
    pub archive_path: String,
    pub target: String,
}

/// A shipped component: the core, or one plugin.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub name: String,
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub version: String,
    /// `"<base>-<version>-{arch}.tar.gz"`. `{arch}` is a literal
    /// placeholder, substituted by `archive_for`, never a template engine's
    /// own syntax.
    pub archive: String,
    pub files: Vec<FileEntry>,
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub initial_config: Vec<InitialConfig>,
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub enable: Vec<String>,
    #[cfg_attr(test, expect(dead_code, reason = "wired by task 15"))]
    pub mount_root: Option<String>,
    /// The `plugins.toml` `[[plugin]]` block, or absent for the core, which
    /// is not a plugin and never has one.
    pub block: Option<String>,
}

impl Component {
    /// `self.archive` with the literal `{arch}` placeholder replaced by the
    /// device's own architecture label.
    pub fn archive_for(&self, arch: &str) -> String {
        self.archive.replace("{arch}", arch)
    }
}

/// One language pack the release ships.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "wired by task 15")]
pub struct Pack {
    pub language: String,
    pub version: String,
    pub archive: String,
}

/// The whole of `inventory.json`, format 1.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub format: u32,
    pub product: String,
    pub reference_order: Vec<String>,
    pub core: Component,
    pub plugins: Vec<Component>,
    pub packs: Vec<Pack>,
}

impl Inventory {
    /// Parses `inventory.json`, refusing anything but format 1 by name.
    pub fn parse(text: &str) -> anyhow::Result<Inventory> {
        let inventory: Inventory =
            serde_json::from_str(text).context("inventory.json does not parse")?;
        anyhow::ensure!(
            inventory.format == 1,
            "inventory.json declares format {}, this installer only understands format 1",
            inventory.format
        );
        Ok(inventory)
    }

    /// The plugin named `name`, if the release ships one.
    pub fn plugin(&self, name: &str) -> Option<&Component> {
        self.plugins.iter().find(|p| p.name == name)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Runs the real generator, exactly as `packaging_manifest.rs` does for
    /// the same script (`run_install_inventory` there): the inventory this
    /// installer will actually receive is produced by python, not
    /// hand-written here.
    fn run_install_inventory() -> String {
        let out = std::process::Command::new("python3")
            .arg("scripts/install-inventory.py")
            .current_dir(repo_root())
            .output()
            .expect("python3 is available: package-release.sh already needs it, here and in CI");
        assert!(
            out.status.success(),
            "install-inventory.py failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("inventory.json is UTF-8")
    }

    /// Parses the real inventory once; other tests (here and in
    /// `names.rs`) reuse the value rather than spawning python3 again.
    pub(crate) fn real_inventory() -> Inventory {
        Inventory::parse(&run_install_inventory()).expect("the real inventory.json parses")
    }

    #[test]
    fn the_real_inventory_parses() {
        let inv = real_inventory();
        assert_eq!(inv.format, 1);
        assert!(!inv.product.is_empty());
        assert!(!inv.reference_order.is_empty());
        assert_eq!(inv.reference_order.len(), inv.plugins.len());
        assert!(!inv.packs.is_empty());
        for name in &inv.reference_order {
            assert!(
                inv.plugin(name).is_some(),
                "reference_order names {name}, which Inventory::plugin cannot find"
            );
        }
        assert!(
            inv.core.block.is_none(),
            "the core is not a plugin and must carry no plugins.toml block"
        );
        for p in &inv.plugins {
            assert!(
                p.block.is_some(),
                "{}: a plugin's inventory entry must carry its plugins.toml block",
                p.name
            );
        }
    }

    #[test]
    fn archive_for_replaces_the_literal_arch_placeholder() {
        let inv = real_inventory();
        let resolved = inv.core.archive_for("armv7");
        assert!(!resolved.contains("{arch}"), "{resolved}");
        assert!(resolved.contains("armv7"), "{resolved}");
        assert!(resolved.starts_with("ritornello-core-"), "{resolved}");
    }

    /// **[MUTATION]**: relax `Inventory::parse`'s check to `format >= 1` —
    /// this test fails, since the mutated format `2` would then be accepted.
    #[test]
    fn a_format_other_than_one_is_refused_by_name() {
        let real = run_install_inventory();
        let mutated = real.replacen("\"format\": 1", "\"format\": 2", 1);
        assert_ne!(mutated, real, "expected the literal \"format\": 1 in the real inventory.json");
        let err = Inventory::parse(&mutated).unwrap_err();
        assert!(err.to_string().contains('2'), "{err}");
    }

    /// **[MUTATION]**: drop `#[serde(deny_unknown_fields)]` from `Inventory`
    /// — this test fails, since the added field would then be silently
    /// dropped instead of refused.
    #[test]
    fn an_unknown_top_level_field_is_refused_rather_than_silently_dropped() {
        let real = run_install_inventory();
        let mutated = real.replacen("\"format\": 1,", "\"format\": 1,\n  \"a_future_field\": true,", 1);
        assert_ne!(mutated, real);
        assert!(Inventory::parse(&mutated).is_err());
    }
}
