use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginState {
    pub preset: u8,
    /// Last country chosen in the admin page: ISO code, or the empty string
    /// for "all countries".
    ///
    /// Persisted on the plugin side and not in the browser: the choice follows
    /// the device, not the workstation connecting to it. `#[serde(default)]`
    /// makes a file written by an earlier version load without error.
    #[serde(default)]
    pub country: String,
}

impl Default for PluginState {
    fn default() -> Self {
        Self { preset: 1, country: String::new() }
    }
}

pub fn load(path: &Path) -> PluginState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, state: &PluginState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Temporary name unique to this process **and** to this call: both halves
    // of the plugin write the same file, and a shared `.tmp` let two
    // simultaneous writes steal the file from under each other (`rename`
    // failing with ENOENT), on top of the lost preference described on
    // `update`.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("json.tmp.{}.{unique}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Modifies the persisted state **without overwriting what is left untouched**.
///
/// Both halves of the plugin write to the same file: the Source for the
/// preset, the Admin for the country. A `save` built from scratch by one would
/// therefore erase the other's field — it happened by construction when the
/// country was added, hence this read-modify-write.
///
/// A race window remains: two simultaneous read-modify cycles can lose each
/// other's changes. Worst consequence, a preference forgotten until the next
/// change; a lock would not be justified for that.
pub fn update(path: &Path, modify: impl FnOnce(&mut PluginState)) -> Result<()> {
    let mut state = load(path);
    modify(&mut state);
    save(path, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_of_an_absent_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = load(&dir.path().join("absent.json"));
        assert_eq!(state.preset, 1);
        assert_eq!(state.country, "", "no country is imposed by default");
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &PluginState { preset: 5, country: "BE".into() }).unwrap();
        let state = load(&path);
        assert_eq!(state.preset, 5);
        assert_eq!(state.country, "BE");
    }

    #[test]
    fn a_file_from_an_earlier_version_loads_again() {
        // Without `#[serde(default)]` on `country`, a device update would make
        // the read fail and start over on preset 1.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"preset":7}"#).unwrap();
        let state = load(&path);
        assert_eq!(state.preset, 7);
        assert_eq!(state.country, "");
    }

    #[test]
    fn update_does_not_destroy_the_field_left_untouched() {
        // Both halves of the plugin write to this file: this is exactly the
        // defect `update` exists to avoid.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        update(&path, |s| s.country = "DE".into()).unwrap();
        update(&path, |s| s.preset = 4).unwrap();
        let state = load(&path);
        assert_eq!(state.preset, 4);
        assert_eq!(state.country, "DE", "the country must survive a preset write");
    }
}
