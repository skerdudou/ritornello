//! The release's inventory: what each shipped component installs, read the
//! way `scripts/install-inventory.py` writes it (format 1). A format this
//! installer does not understand is refused by name rather than read
//! partway — a future format's fields would otherwise deserialize through
//! whatever they happen to share with this one, and the installer would
//! place files it never actually described.

use anyhow::Context;
use serde::Deserialize;

/// M4: a field whose *key* must be present even though its value may be
/// `null`.
///
/// A plain `Option<T>` field cannot express this on its own: serde-derive's
/// generated code, on a missing key, calls a placeholder deserializer whose
/// `deserialize_option` answers `visit_none()` — which is exactly what
/// `Option<T>`'s own `Deserialize` impl asks for — so a missing key and an
/// explicit `null` end up indistinguishable, and a future generator that
/// stopped writing the key at all would be read as "no value" rather than
/// refused. (A newtype wrapping `Option<T>` does not escape this either:
/// its `Deserialize` impl still has to call `deserialize_option`
/// eventually to read the value, and that alone is what the placeholder
/// keys off — not the field's declared type name.)
///
/// The escape is narrower than a new type: *any* `#[serde(deserialize_with
/// = "...")]` on the field, on its own, skips that placeholder entirely —
/// serde-derive's own missing-field code checks for the attribute's
/// presence, not what the function does, and reports "missing field"
/// directly when it is missing. The function itself does nothing unusual
/// for a key that *is* present: it deserializes `Option<T>` exactly as the
/// derive would have, so an explicit `null` still reads as `None`.
fn required_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

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
    #[serde(deserialize_with = "required_some")]
    pub mount_root: Option<String>,
    /// The `plugins.toml` `[[plugin]]` block, or absent for the core, which
    /// is not a plugin and never has one. `install-inventory.py` always
    /// writes this key, `null` or not (M4) — see
    /// `a_missing_block_key_is_refused_rather_than_defaulted_to_none`.
    #[serde(deserialize_with = "required_some")]
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

    /// M3: only the top-level `deny_unknown_fields` was ever exercised.
    /// Dropping it from `Component`, `FileEntry`, `InitialConfig` or `Pack`
    /// individually reddened nothing, since the mutated field always
    /// landed inside one of those nested objects, never at the top level.
    /// One test per nested type, each anchored on a substring that is
    /// unique in the real inventory (checked below) so the injected field
    /// lands in exactly the object it names.
    ///
    /// **[MUTATION]**: drop `#[serde(deny_unknown_fields)]` from `Component`
    /// — this test fails.
    #[test]
    fn an_unknown_component_field_is_refused_rather_than_silently_dropped() {
        let real = run_install_inventory();
        let anchor = "\"name\": \"core\",";
        assert_eq!(real.matches(anchor).count(), 1, "expected exactly one Component named \"core\"");
        let mutated = real.replacen(anchor, "\"name\": \"core\", \"a_future_field\": true,", 1);
        assert_ne!(mutated, real);
        assert!(Inventory::parse(&mutated).is_err());
    }

    /// **[MUTATION]**: drop `#[serde(deny_unknown_fields)]` from `FileEntry`
    /// — this test fails.
    #[test]
    fn an_unknown_file_entry_field_is_refused_rather_than_silently_dropped() {
        let real = run_install_inventory();
        let anchor = "\"archive_path\": \"usr/local/bin/ritornello-core\",";
        assert_eq!(real.matches(anchor).count(), 1, "expected exactly one FileEntry for the core binary");
        let mutated = real.replacen(anchor, &format!("{anchor} \"a_future_field\": true,"), 1);
        assert_ne!(mutated, real);
        assert!(Inventory::parse(&mutated).is_err());
    }

    /// **[MUTATION]**: drop `#[serde(deny_unknown_fields)]` from
    /// `InitialConfig` — this test fails.
    #[test]
    fn an_unknown_initial_config_field_is_refused_rather_than_silently_dropped() {
        let real = run_install_inventory();
        let anchor = "\"archive_path\": \"initial-config/stations.example.toml\",";
        assert_eq!(real.matches(anchor).count(), 1, "expected exactly one radio InitialConfig entry");
        let mutated = real.replacen(anchor, &format!("{anchor} \"a_future_field\": true,"), 1);
        assert_ne!(mutated, real);
        assert!(Inventory::parse(&mutated).is_err());
    }

    /// **[MUTATION]**: drop `#[serde(deny_unknown_fields)]` from `Pack` —
    /// this test fails.
    #[test]
    fn an_unknown_pack_field_is_refused_rather_than_silently_dropped() {
        let real = run_install_inventory();
        let anchor = "\"language\": \"fr\",";
        assert_eq!(real.matches(anchor).count(), 1, "expected exactly one pack for \"fr\"");
        let mutated = real.replacen(anchor, &format!("{anchor} \"a_future_field\": true,"), 1);
        assert_ne!(mutated, real);
        assert!(Inventory::parse(&mutated).is_err());
    }

    /// M4, isolated from the real inventory's own shape: `required_some`
    /// requires the key present, but still reads an explicit `null` as
    /// `None` rather than refusing it.
    ///
    /// **[MUTATION]**: drop `#[serde(deserialize_with = "required_some")]`
    /// from `Wrapper::x` below — the last assertion fails, since a plain
    /// `Option<i32>` field accepts a missing key.
    #[test]
    fn required_some_needs_the_key_present_but_still_reads_an_explicit_null() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "required_some")]
            x: Option<i32>,
        }
        assert_eq!(serde_json::from_str::<Wrapper>(r#"{"x": null}"#).unwrap().x, None);
        assert_eq!(serde_json::from_str::<Wrapper>(r#"{"x": 3}"#).unwrap().x, Some(3));
        assert!(serde_json::from_str::<Wrapper>("{}").is_err());
    }

    /// M4: a component whose `mount_root` key is missing entirely must be
    /// refused, not read as `None` — the same ambiguity a bare `Option`
    /// would create between "no value" and "the generator stopped writing
    /// this key". Anchored on core's own object, the only one where
    /// `mount_root` and `block` are both `null` (so this exact combined
    /// string is unique), and `replacen(..., 1)` only ever touches the
    /// first match regardless.
    ///
    /// **[MUTATION]**: change `Component::mount_root`'s type back to
    /// `Option<String>` — this test fails, since the missing key would
    /// then default to `None`.
    #[test]
    fn a_missing_mount_root_key_is_refused_rather_than_defaulted_to_none() {
        let real = run_install_inventory();
        let anchor = "\"mount_root\": null,\n    \"block\": null\n  },";
        assert!(real.contains(anchor), "core's own mount_root/block pair moved or changed shape");
        let mutated = real.replacen(anchor, "\"block\": null\n  },", 1);
        assert_ne!(mutated, real);
        assert!(Inventory::parse(&mutated).is_err());
    }

    /// M4, the same failure mode for `block`.
    ///
    /// **[MUTATION]**: change `Component::block`'s type back to
    /// `Option<String>` — this test fails.
    #[test]
    fn a_missing_block_key_is_refused_rather_than_defaulted_to_none() {
        let real = run_install_inventory();
        let anchor = "\"mount_root\": null,\n    \"block\": null\n  },";
        assert!(real.contains(anchor), "core's own mount_root/block pair moved or changed shape");
        let mutated = real.replacen(anchor, "\"mount_root\": null\n  },", 1);
        assert_ne!(mutated, real);
        assert!(Inventory::parse(&mutated).is_err());
    }
}
