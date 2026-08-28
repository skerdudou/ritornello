use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginState {
    pub preset: u8,
    /// Dernier pays choisi dans la page d'admin : code ISO, ou chaîne clear pour
    /// « tous les pays ».
    ///
    /// Persisté côté plugin et non côté navigateur : le choix suit l'appareil,
    /// pas le poste qui s'y connecte. `#[serde(default)]` fait qu'un fichier
    /// écrit par une version antérieure se relit sans erreur.
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
    // Nom temporaire propre à ce processus **et** à cet appel : les deux
    // moitiés du plugin écrivent le même fichier, et un `.tmp` partagé
    // permettait à deux écritures simultanées de se voler le fichier sous le
    // pied (`rename` en ENOENT), en plus de la perte de préférence décrite
    // sur `update`.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("json.tmp.{}.{unique}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Modifie l'état persisté **sans écraser ce qu'on ne touche pas**.
///
/// Les deux moitiés du plugin écrivent dans le même fichier : la Source pour la
/// présélection, l'Admin pour le pays. Un `save` construit de toutes pièces par
/// l'une effacerait donc le champ de l'autre — c'est arrivé par construction
/// lors de l'ajout du pays, d'où cette playback-modification-écriture.
///
/// Reste une fenêtre de course : deux lectures-modifications simultanées
/// peuvent se perdre l'une l'autre. Conséquence maximale, une préférence
/// oubliée jusqu'au prochain changement ; un verrou ne se justifierait pas
/// pour cela.
pub fn update(path: &Path, modifie: impl FnOnce(&mut PluginState)) -> Result<()> {
    let mut state = load(path);
    modifie(&mut state);
    save(path, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defauts_dun_fichier_absent() {
        let dir = tempfile::tempdir().unwrap();
        let state = load(&dir.path().join("absent.json"));
        assert_eq!(state.preset, 1);
        assert_eq!(state.country, "", "aucun pays impose par defaut");
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
    fn un_fichier_dune_version_anterieure_se_relit() {
        // Sans `#[serde(default)]` sur `country`, une mise a jour de l'appareil
        // ferait echouer la playback et repartir sur la preselection 1.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"preset":7}"#).unwrap();
        let state = load(&path);
        assert_eq!(state.preset, 7);
        assert_eq!(state.country, "");
    }

    #[test]
    fn update_ne_detruit_pas_le_champ_quon_ne_touche_pas() {
        // Les deux halves du plugin ecrivent dans ce fichier : c'est
        // exactement le defaut que `update` existe pour eviter.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        update(&path, |s| s.country = "DE".into()).unwrap();
        update(&path, |s| s.preset = 4).unwrap();
        let state = load(&path);
        assert_eq!(state.preset, 4);
        assert_eq!(state.country, "DE", "le pays doit survivre a une ecriture de preselection");
    }
}
