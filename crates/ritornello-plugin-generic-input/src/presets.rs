use crate::bindings::Binding;
use ritornello_i18n::Catalog;
use serde::Deserialize;
use std::path::Path;

/// A preset is a simple list of bindings, with no device name.
#[derive(Debug, Clone, Default, Deserialize)]
struct Preset {
    #[serde(default)]
    bindings: Vec<Binding>,
}

/// Preset not found, unreadable or invalid — a single user-facing error
/// case: "this preset does not exist". The detail goes to the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPreset(pub String);

impl UnknownPreset {
    pub fn message(&self, catalog: &Catalog) -> String {
        catalog.get("unknown_preset").replace("{preset}", &self.0)
    }
}

impl std::fmt::Display for UnknownPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown preset: {}", self.0)
    }
}

impl std::error::Error for UnknownPreset {}

/// A preset name is a simple identifier: it comes from the browser and is
/// used to build a path, so no separator and no dot (no `../`).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Pure parse of a directory listing: keeps only `*.toml` files with a valid
/// name, without the extension, sorted and deduplicated. Separated from disk
/// access to stay testable (like the core's `audio_output::parse_device_list`).
pub fn parse_preset_names(entries: &[String]) -> Vec<String> {
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.strip_suffix(".toml"))
        .filter(|n| valid_name(n))
        .map(|n| n.to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Names of the available presets. Missing or unreadable directory → empty list.
pub fn list(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else {
        tracing::warn!("preset directory {} unreadable: no preset", root.display());
        return Vec::new();
    };
    let entries: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    parse_preset_names(&entries)
}

/// Pure parse of a preset's TOML content (no disk access): this is the only
/// text → bindings conversion point, used both by `load` (shipped preset)
/// and by the import from an uploaded file (`admin::Op::ImportPreset`), so
/// that only one parser exists.
pub fn parse_preset(content: &str) -> Result<Vec<Binding>, String> {
    let preset: Preset = toml::from_str(content).map_err(|e| e.to_string())?;
    Ok(preset.bindings)
}

/// Loads the bindings of a preset. Invalid name, missing file or unreadable
/// TOML → `UnknownPreset` (with a `warn` detailing the real cause).
pub fn load(root: &Path, name: &str) -> Result<Vec<Binding>, UnknownPreset> {
    if !valid_name(name) {
        tracing::warn!("preset name rejected: {name}");
        return Err(UnknownPreset(name.to_string()));
    }
    let path = root.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        tracing::warn!("preset {} unreadable: {e}", path.display());
        UnknownPreset(name.to_string())
    })?;
    parse_preset(&text).map_err(|e| {
        tracing::warn!("preset {} invalid: {e}", path.display());
        UnknownPreset(name.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::Command;

    /// Root of the presets shipped in the repo (`deploy/input-presets`).
    fn shipped_presets() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/input-presets")
    }

    #[test]
    fn parse_preset_names_keeps_only_valid_toml_files() {
        let entries = vec![
            "mce.toml".to_string(),
            "keyboard.toml".to_string(),
            "README.md".to_string(),
            "..toml".to_string(),
            "../evasion.toml".to_string(),
            "mce.toml".to_string(),
        ];
        assert_eq!(parse_preset_names(&entries), vec!["keyboard", "mce"]);
    }

    #[test]
    fn list_discovers_the_presets_of_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mce.toml"), "").unwrap();
        std::fs::write(dir.path().join("keyboard.toml"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        assert_eq!(list(dir.path()), vec!["keyboard", "mce"]);
    }

    #[test]
    fn list_missing_directory_gives_an_empty_list() {
        assert!(list(Path::new("/nonexistent-presets-xyz")).is_empty());
    }

    #[test]
    fn parse_preset_valid_gives_the_expected_bindings() {
        let toml =
            "[[bindings]]\ncode = 115\ncmd = \"VolumeUp\"\n\n[[bindings]]\ncode = 2\ncmd = \"Select\"\narg = 1\n";
        let b = parse_preset(toml).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].code, 115);
        assert_eq!(b[0].command(), Some(Command::VolumeUp));
        assert_eq!(b[1].command(), Some(Command::Select(1)));
    }

    #[test]
    fn parse_preset_invalid_toml_gives_an_error() {
        assert!(parse_preset("this is not = toml [").is_err());
    }

    #[test]
    fn parse_preset_reads_the_same_content_as_the_shipped_file() {
        let root = shipped_presets();
        let text = std::fs::read_to_string(root.join("mce.toml")).unwrap();
        let via_text = parse_preset(&text).unwrap();
        let via_file = load(&root, "mce").unwrap();
        assert_eq!(via_text, via_file, "text parser diverges from file loading");
        assert!(!via_text.is_empty());
    }

    #[test]
    fn the_mce_preset_covers_the_ten_presets_and_plus_ten() {
        // The shipped table comes from a measurement on the hardware. It
        // carried neither the 0 key nor the "+10": two remote keys did
        // nothing even though the core already knew how to handle them.
        let b = load(&shipped_presets(), "mce").unwrap();
        for n in 0..=9u8 {
            assert!(
                b.iter().any(|x| x.command() == Some(Command::Select(n))),
                "no key selects preset {n}"
            );
        }
        assert!(b.iter().any(|x| x.command() == Some(Command::Plus10)), "no +10 key");
    }

    #[test]
    fn the_mce_preset_carries_the_transport_codes_measured_on_the_hardware() {
        // The previous table was transcribed from an old keymap, never
        // checked against a real device: it bound Stop to 166 and Eject to
        // 161, which this receiver does not emit. These codes are measured.
        let b = load(&shipped_presets(), "mce").unwrap();
        let cmd = |code: u16| b.iter().find(|x| x.code == code).and_then(Binding::command);
        assert_eq!(cmd(128), Some(Command::Stop));
        assert_eq!(cmd(174), Some(Command::Eject));
        assert_eq!(cmd(142), Some(Command::Power));
        assert_eq!(cmd(407), Some(Command::Next));
        assert_eq!(cmd(412), Some(Command::Prev));
        assert_eq!(cmd(105), Some(Command::SeekBackward));
        assert_eq!(cmd(106), Some(Command::SeekForward));
    }

    #[test]
    fn load_reads_the_bindings_of_a_preset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test.toml"),
            "[[bindings]]\ncode = 115\ncmd = \"VolumeUp\"\n\n[[bindings]]\ncode = 2\ncmd = \"Select\"\narg = 1\n",
        )
        .unwrap();
        let b = load(dir.path(), "test").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].code, 115);
        assert_eq!(b[0].command(), Some(Command::VolumeUp));
        assert_eq!(b[1].command(), Some(Command::Select(1)));
    }

    #[test]
    fn load_unknown_preset_returns_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), "absent"), Err(UnknownPreset("absent".into())));
    }

    #[test]
    fn load_rejects_a_hijacked_name() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), "../../etc/passwd").is_err());
        assert!(load(dir.path(), "").is_err());
    }

    #[test]
    fn unknown_preset_message_uses_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "unknown_preset = \"preset inconnu : {preset}\"\n",
        )
        .unwrap();
        let cat = Catalog::load("generic-input", "fr", dir.path(), crate::GENERIC_INPUT_EN);
        assert_eq!(UnknownPreset("zzz".into()).message(&cat), "preset inconnu : zzz");
    }

    #[test]
    fn the_shipped_presets_load_and_are_non_empty() {
        let root = shipped_presets();
        assert_eq!(list(&root), vec!["keyboard", "mce"]);

        let mce = load(&root, "mce").unwrap();
        assert!(!mce.is_empty());
        assert_eq!(mce.iter().find(|b| b.code == 115).unwrap().command(), Some(Command::VolumeUp));
        assert_eq!(mce.iter().find(|b| b.code == 513).unwrap().command(), Some(Command::Select(1)));
        // 142 (KEY_SLEEP), not 356: the table was measured on the device, and
        // this receiver does not emit the codes the old transcription
        // attributed to it.
        assert_eq!(mce.iter().find(|b| b.code == 142).unwrap().command(), Some(Command::Power));

        let kbd = load(&root, "keyboard").unwrap();
        assert!(!kbd.is_empty());
        assert_eq!(kbd.iter().find(|b| b.code == 57).unwrap().command(), Some(Command::PlayPause));
        assert_eq!(kbd.iter().find(|b| b.code == 103).unwrap().command(), Some(Command::VolumeUp));
    }
}
