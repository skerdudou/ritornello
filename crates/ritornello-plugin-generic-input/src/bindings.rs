use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_proto::Command;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// A key bound to a command. The `cmd`/`arg` pair is exactly the serialized
/// representation of `Command` (`#[serde(tag = "cmd", content = "arg")]`)
/// flattened into the binding: no list of commands is duplicated, and the
/// same object flows unchanged as JSON to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub code: u16,
    #[serde(flatten)]
    pub command: Command,
}

impl Binding {
    /// Convenience constructor for tests (everywhere else, a `Binding`
    /// arrives via TOML/JSON deserialization, never built by hand).
    #[cfg(test)]
    pub fn new(code: u16, command: &Command) -> Self {
        Binding { code, command: command.clone() }
    }

    /// Command carried by this binding. `Option` because the fallback form
    /// documented in the spec (`cmd: String` + `arg: Option<u8>`) can carry
    /// an unknown command; under the nominal flattened form, it is always
    /// `Some`. The rest of the crate always goes through this accessor,
    /// which confines any fallback to this file.
    pub fn command(&self) -> Option<Command> {
        Some(self.command.clone())
    }
}

/// The bindings of a device, identified by its **name** (stable across
/// reboots): every evdev node carrying this name is affected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    #[serde(default)]
    pub devices: Vec<Device>,
}

/// Typed validation error: the user-facing text is produced at the boundary
/// via `message(&Catalog)` (the radio plugin's model). `Display` provides an
/// English version for internal logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateCode { device: String, code: u16 },
    SelectOutOfRange { device: String, arg: u8 },
    UnknownCommand { device: String, code: u16 },
}

impl ValidationError {
    /// Localized message surfaced to the user (body of the admin-side 422).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ValidationError::DuplicateCode { device, code } => catalog
                .get("duplicate_code")
                .replace("{code}", &code.to_string())
                .replace("{device}", device),
            ValidationError::SelectOutOfRange { device, arg } => catalog
                .get("select_out_of_range")
                .replace("{n}", &arg.to_string())
                .replace("{device}", device),
            ValidationError::UnknownCommand { device, code } => catalog
                .get("unknown_command")
                .replace("{code}", &code.to_string())
                .replace("{device}", device),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::DuplicateCode { device, code } => {
                write!(f, "code {code} bound twice on {device}")
            }
            ValidationError::SelectOutOfRange { device, arg } => {
                write!(f, "preset {arg} out of range 0-9 on {device}")
            }
            ValidationError::UnknownCommand { device, code } => {
                write!(f, "unknown command bound to code {code} on {device}")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

impl Bindings {
    /// Loads the table. Best-effort: a missing file or invalid TOML give an
    /// empty table with a `warn` — never a panic, the plugin starts and the
    /// user fixes it from the admin page.
    pub fn load(path: &Path) -> Bindings {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "bindings {} unreadable ({e}): starting with no binding, use the admin page",
                    path.display()
                );
                return Bindings::default();
            }
        };
        match toml::from_str::<Bindings>(&text) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "bindings {} invalid ({e}): starting with no binding, use the admin page",
                    path.display()
                );
                Bindings::default()
            }
        }
    }

    /// Atomic write: temporary file then `rename`, never a truncated file if
    /// power is lost at the wrong moment.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        for dev in &self.devices {
            let mut seen = HashSet::new();
            for b in &dev.bindings {
                if !seen.insert(b.code) {
                    return Err(ValidationError::DuplicateCode {
                        device: dev.name.clone(),
                        code: b.code,
                    });
                }
                match b.command() {
                    None => {
                        return Err(ValidationError::UnknownCommand {
                            device: dev.name.clone(),
                            code: b.code,
                        })
                    }
                    Some(Command::Select(n)) if !(0..=9).contains(&n) => {
                        return Err(ValidationError::SelectOutOfRange {
                            device: dev.name.clone(),
                            arg: n,
                        })
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// Resolution at event time: (device name, code) → command. `None` =
    /// unbound key, silently ignored.
    pub fn resolve(&self, device_name: &str, code: u16) -> Option<Command> {
        self.devices
            .iter()
            .find(|d| d.name == device_name)
            .and_then(|d| d.bindings.iter().find(|b| b.code == code))
            .and_then(|b| b.command())
    }

    /// Replaces the entire set of bindings of a device (creating the entry
    /// if it doesn't exist). Used by `load_preset`.
    pub fn replace_device(&mut self, device: &str, bindings: Vec<Binding>) {
        match self.devices.iter_mut().find(|d| d.name == device) {
            Some(d) => d.bindings = bindings,
            None => self.devices.push(Device { name: device.to_string(), bindings }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bindings {
        Bindings {
            devices: vec![
                Device {
                    name: "eHome Infrared Transceiver".into(),
                    bindings: vec![
                        Binding::new(115, &Command::VolumeUp),
                        Binding::new(2, &Command::Select(1)),
                    ],
                },
                Device {
                    name: "USB Keyboard".into(),
                    bindings: vec![Binding::new(57, &Command::PlayPause)],
                },
            ],
        }
    }

    /// FIRST test of this effort: `#[serde(flatten)]` on an adjacently-tagged
    /// enum is proven in JSON on this project, not in TOML. If it fails,
    /// apply without discussion the fallback documented in the spec (fields
    /// `cmd: String` + `arg: Option<u8>` and conversions to `Command`),
    /// which keeps exactly the same file and JSON shape.
    #[test]
    fn binding_roundtrip_toml() {
        let with_arg = Binding::new(2, &Command::Select(1));
        let t = toml::to_string_pretty(&with_arg).unwrap();
        assert!(t.contains("code = 2"), "TOML produced: {t}");
        assert!(t.contains("cmd = \"Select\""), "TOML produced: {t}");
        assert!(t.contains("arg = 1"), "TOML produced: {t}");
        assert_eq!(toml::from_str::<Binding>(&t).unwrap(), with_arg);

        let without_arg = Binding::new(115, &Command::VolumeUp);
        let t2 = toml::to_string_pretty(&without_arg).unwrap();
        assert!(!t2.contains("arg"), "TOML produced: {t2}");
        assert_eq!(toml::from_str::<Binding>(&t2).unwrap(), without_arg);
    }

    #[test]
    fn binding_json_carries_cmd_and_arg_flattened() {
        assert_eq!(
            serde_json::to_value(Binding::new(2, &Command::Select(1))).unwrap(),
            serde_json::json!({ "code": 2, "cmd": "Select", "arg": 1 })
        );
        assert_eq!(
            serde_json::to_value(Binding::new(166, &Command::Stop)).unwrap(),
            serde_json::json!({ "code": 166, "cmd": "Stop" })
        );
        let b: Binding =
            serde_json::from_value(serde_json::json!({ "code": 9, "cmd": "Select", "arg": 8 }))
                .unwrap();
        assert_eq!(b.command(), Some(Command::Select(8)));
    }

    #[test]
    fn file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        sample().save(&path).unwrap();
        assert_eq!(Bindings::load(&path), sample());
    }

    #[test]
    fn missing_file_gives_an_empty_table() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Bindings::load(&dir.path().join("absent.toml")), Bindings::default());
    }

    #[test]
    fn invalid_toml_gives_an_empty_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        std::fs::write(&path, "this is not = toml [").unwrap();
        assert_eq!(Bindings::load(&path), Bindings::default());
    }

    #[test]
    fn save_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        sample().save(&path).unwrap();
        assert!(path.exists());
        assert!(!dir.path().join("input-bindings.toml.tmp").exists());
    }

    #[test]
    fn validate_rejects_a_code_bound_twice_on_the_same_device() {
        let mut b = sample();
        b.devices[0].bindings.push(Binding::new(115, &Command::Mute));
        assert_eq!(
            b.validate(),
            Err(ValidationError::DuplicateCode {
                device: "eHome Infrared Transceiver".into(),
                code: 115
            })
        );
    }

    #[test]
    fn validate_accepts_the_same_code_on_two_different_devices() {
        let mut b = sample();
        b.devices[1].bindings.push(Binding::new(115, &Command::VolumeUp));
        assert!(b.validate().is_ok());
    }

    #[test]
    fn validate_accepts_select_0_the_remotes_0_key() {
        let mut b = sample();
        b.devices[1].bindings.push(Binding::new(11, &Command::Select(0)));
        assert!(b.validate().is_ok());
    }

    #[test]
    fn validate_rejects_a_select_out_of_bounds() {
        let mut b2 = sample();
        b2.devices[1].bindings.push(Binding::new(11, &Command::Select(10)));
        assert_eq!(
            b2.validate(),
            Err(ValidationError::SelectOutOfRange { device: "USB Keyboard".into(), arg: 10 })
        );
    }

    #[test]
    fn plus10_binds_and_roundtrips_in_toml() {
        let b = Binding::new(11, &Command::Plus10);
        let t = toml::to_string_pretty(&b).unwrap();
        assert!(t.contains("cmd = \"Plus10\""), "TOML produced: {t}");
        assert!(!t.contains("arg"), "TOML produced: {t}");
        assert_eq!(toml::from_str::<Binding>(&t).unwrap(), b);
        let mut table = Bindings::default();
        table.devices.push(Device { name: "X".into(), bindings: vec![b] });
        assert!(table.validate().is_ok());
    }

    #[test]
    fn save_rejects_an_invalid_table_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        let mut b = sample();
        b.devices[0].bindings.push(Binding::new(115, &Command::Mute));
        assert!(b.save(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn validation_message_uses_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "duplicate_code = \"code {code} lie deux fois sur {device}\"\n",
        )
        .unwrap();
        let cat =
            Catalog::load("generic-input", "fr", dir.path(), crate::GENERIC_INPUT_EN);
        let err = ValidationError::DuplicateCode { device: "X".into(), code: 42 };
        assert_eq!(err.message(&cat), "code 42 lie deux fois sur X");
    }

    #[test]
    fn resolve_finds_the_right_devices_command() {
        let b = sample();
        assert_eq!(b.resolve("eHome Infrared Transceiver", 115), Some(Command::VolumeUp));
        assert_eq!(b.resolve("eHome Infrared Transceiver", 2), Some(Command::Select(1)));
        assert_eq!(b.resolve("USB Keyboard", 57), Some(Command::PlayPause));
        // code not bound on this device
        assert_eq!(b.resolve("USB Keyboard", 115), None);
        // unknown device
        assert_eq!(b.resolve("Mouse", 115), None);
    }

    #[test]
    fn replace_device_replaces_or_creates_the_entry() {
        let mut b = sample();
        b.replace_device("USB Keyboard", vec![Binding::new(50, &Command::Mute)]);
        assert_eq!(b.devices[1].bindings, vec![Binding::new(50, &Command::Mute)]);
        b.replace_device("New", vec![Binding::new(1, &Command::Power)]);
        assert_eq!(b.devices.len(), 3);
        assert_eq!(b.devices[2].name, "New");
    }
}
