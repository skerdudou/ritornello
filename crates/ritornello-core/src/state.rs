use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// What the device does with the active source when the process starts.
/// Read once, at launch, by `Core::demarrage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPower {
    /// Wake the active source: the device plays again on its own.
    #[default]
    On,
    /// Configure mpv but leave the source asleep, standby on the display.
    Standby,
    /// Whatever the device was doing when it last wrote its state
    /// (`PersistedState::standby`) — on after a crash mid-listening,
    /// standby after a power cut that followed a deliberate standby.
    Previous,
}

/// Behavior settings, edited on the config page (`PUT /api/settings`).
/// Container-level `serde(default)`: a partial block in a hand-edited
/// state.json fills in with defaults instead of failing to load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Hold-to-repeat: delay before the first repeated volume step.
    pub volume_repeat_initial_ms: u32,
    /// Hold-to-repeat: delay between subsequent volume steps.
    pub volume_repeat_interval_ms: u32,
    /// On, standby, or "as it was" at launch — see `StartupPower`.
    pub startup_power: StartupPower,
    /// How long the volume/mute overlay and sources' transient messages
    /// (e.g. "empty preset") stay on screen before the permanent view
    /// reappears. Deliberately a separate field from `tens_window_ms`,
    /// not shared: this overlay hides the "now playing" view and may want
    /// to shrink one day, while the tens-offset window below must stay
    /// comfortable regardless — coupling them would forbid tuning either
    /// on its own.
    pub overlay_ms: u32,
    /// How long the remote's pending `+10`/`+20`/... offset stays armed,
    /// shown as the `+NN` overlay: the time left to press the second
    /// digit. Independent from `overlay_ms` for the same reason in
    /// reverse — see that field's comment. The core stores each overlay's
    /// own deadline (`overlay: Option<(Overlay, Instant)>`), so
    /// `show_tens_overlay` reading this field and `expire_overlay`
    /// staying oblivious to which duration produced the deadline keeps
    /// the offset and its overlay disarming together **on expiry**,
    /// whatever the two values are. That alone would not be enough: the
    /// overlay slot can also be taken over before its deadline — by the
    /// abandon guard in `appliquer_commande`, or by a source's transient
    /// message in `handle_source_update` — and both of those explicitly
    /// clear the offset too, so it never survives behind a display that
    /// no longer shows it.
    pub tens_window_ms: u32,
    /// Pas des touches « avancer » / « reculer », en secondes.
    ///
    /// Réglable là où le pas de volume est figé, parce que la bonne valeur
    /// dépend de ce qu'on écoute : dix secondes pour rattraper une phrase,
    /// une minute pour traverser un mouvement.
    pub seek_step_s: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume_repeat_initial_ms: 800,
            volume_repeat_interval_ms: 200,
            startup_power: StartupPower::On,
            overlay_ms: 5000,
            tens_window_ms: 5000,
            seek_step_s: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub active_source: String,
    pub volume: u8,
    /// Whether the device was in standby when this state was last written.
    /// Only `StartupPower::Previous` reads it; every path that toggles
    /// standby writes it (see `Core::persist` callers), so it describes the
    /// last observed reality rather than an intention.
    #[serde(default)]
    pub standby: bool,
    #[serde(default)]
    pub audio_device: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    /// Preset de thème choisi (nom opaque pour le cœur : la liste des presets
    /// vit dans la SPA). Absent = `theme::DEFAULT_THEME`.
    #[serde(default)]
    pub theme: Option<String>,
    /// `"light"` ou `"dark"`. Absent = `theme::DEFAULT_MODE`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Behavior settings (hold-to-repeat timings, startup power state).
    #[serde(default)]
    pub settings: Settings,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self { active_source: "radio".into(), volume: 60, standby: false, audio_device: None, locale: None, theme: None, mode: None, settings: Settings::default() }
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
        let st = PersistedState {
            active_source: "cd".into(),
            volume: 35,
            standby: false,
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            theme: None,
            mode: None,
            settings: Settings::default(),
        };
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
            standby: false,
            audio_device: None,
            locale: Some("fr".into()),
            theme: None,
            mode: None,
            settings: Settings::default(),
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn theme_et_mode_absents_par_defaut_et_roundtrip() {
        assert_eq!(PersistedState::default().theme, None);
        assert_eq!(PersistedState::default().mode, None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState {
            active_source: "radio".into(),
            volume: 50,
            standby: false,
            audio_device: None,
            locale: None,
            theme: Some("cyberpunk".into()),
            mode: Some("dark".into()),
            settings: Settings::default(),
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn un_state_json_anterieur_reste_lisible() {
        // Compatibilite ascendante : un fichier ecrit avant cette version n'a
        // ni `theme` ni `mode` ; il doit se charger sans erreur.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"audio_device":null,"locale":"fr"}"#,
        )
        .unwrap();
        let st = load(&path);
        assert_eq!(st.volume, 42);
        assert_eq!(st.locale.as_deref(), Some("fr"));
        assert_eq!(st.theme, None);
        assert_eq!(st.mode, None);
    }

    #[test]
    fn settings_par_defaut() {
        let s = Settings::default();
        assert_eq!(s.volume_repeat_initial_ms, 800);
        assert_eq!(s.volume_repeat_interval_ms, 200);
        assert_eq!(s.startup_power, StartupPower::On);
        assert_eq!(s.overlay_ms, 5000);
        assert_eq!(s.tens_window_ms, 5000);
        assert_eq!(s.seek_step_s, 10);
        assert_eq!(PersistedState::default().settings, Settings::default());
    }

    #[test]
    fn un_state_json_sans_settings_reste_lisible() {
        // Backward compatibility: a state.json written before this version has
        // no `settings` block; it must load with the defaults.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"audio_device":null,"locale":"fr"}"#,
        )
        .unwrap();
        let st = load(&path);
        assert_eq!(st.settings, Settings::default());
        assert_eq!(st.volume, 42);
    }

    #[test]
    fn settings_roundtrip_et_bloc_partiel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        // Non-default values throughout, overlay_ms/tens_window_ms included:
        // a fixture carrying 5000 would no longer distinguish "the default
        // applied" from "the written value survived" — exactly the defect a
        // review flagged on the volume fixture above.
        let st = PersistedState {
            settings: Settings {
                volume_repeat_initial_ms: 900,
                volume_repeat_interval_ms: 250,
                startup_power: StartupPower::Previous,
                overlay_ms: 6000,
                tens_window_ms: 7000,
                seek_step_s: 45,
            },
            ..Default::default()
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
        // A hand-edited partial block falls back to defaults for what's missing.
        std::fs::write(&path, r#"{"active_source":"radio","volume":42,"settings":{"startup_power":"standby"}}"#).unwrap();
        let st = load(&path);
        assert_eq!(st.settings.startup_power, StartupPower::Standby);
        assert_eq!(st.settings.volume_repeat_initial_ms, 800);
        assert_eq!(st.settings.overlay_ms, 5000);
        assert_eq!(st.settings.tens_window_ms, 5000);
        assert_eq!(st.settings.seek_step_s, 10);
    }

    #[test]
    fn la_veille_persistee_vaut_faux_sans_la_cle_et_survit_au_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"active_source":"radio","volume":42}"#).unwrap();
        assert!(!load(&path).standby, "sans la cle, on repart eveille");

        let st = PersistedState { standby: true, ..Default::default() };
        save(&path, &st).unwrap();
        assert!(load(&path).standby);
    }
}
