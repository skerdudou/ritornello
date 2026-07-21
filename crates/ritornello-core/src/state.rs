use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub active_source: String,
    pub volume: u8,
    #[serde(default)]
    pub audio_device: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self { active_source: "radio".into(), volume: 60, audio_device: None, locale: None }
    }
}

pub fn load(path: &Path) -> PersistedState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, state: &PersistedState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaut_si_fichier_absent_ou_corrompu() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.json");
        assert_eq!(load(&missing), PersistedState::default());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{pas du json").unwrap();
        assert_eq!(load(&bad), PersistedState::default());
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState { active_source: "cd".into(), volume: 35, audio_device: Some("bluealsa:DEV=XX".into()), locale: None };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn defaut_est_radio_vol60_sans_sortie_choisie() {
        let d = PersistedState::default();
        assert_eq!(d.active_source, "radio");
        assert_eq!(d.volume, 60);
        assert_eq!(d.audio_device, None);
    }

    #[test]
    fn locale_absente_par_defaut_et_roundtrip() {
        assert_eq!(PersistedState::default().locale, None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState {
            active_source: "radio".into(),
            volume: 50,
            audio_device: None,
            locale: Some("fr".into()),
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }
}
