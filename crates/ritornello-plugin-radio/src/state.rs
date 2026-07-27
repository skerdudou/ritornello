use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginState {
    pub preset: u8,
    /// Dernier pays choisi dans la page d'admin : code ISO, ou chaîne vide pour
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
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Modifie l'état persisté **sans écraser ce qu'on ne touche pas**.
///
/// Les deux moitiés du plugin écrivent dans le même fichier : la Source pour la
/// présélection, l'Admin pour le pays. Un `save` construit de toutes pièces par
/// l'une effacerait donc le champ de l'autre — c'est arrivé par construction
/// lors de l'ajout du pays, d'où cette lecture-modification-écriture.
///
/// Reste une fenêtre de course : deux écritures simultanées peuvent se perdre
/// l'une l'autre. Conséquence maximale, une préférence oubliée jusqu'au prochain
/// changement ; un verrou ne se justifierait pas pour cela.
pub fn update(path: &Path, modifie: impl FnOnce(&mut PluginState)) -> Result<()> {
    let mut etat = load(path);
    modifie(&mut etat);
    save(path, &etat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defauts_dun_fichier_absent() {
        let dir = tempfile::tempdir().unwrap();
        let etat = load(&dir.path().join("absent.json"));
        assert_eq!(etat.preset, 1);
        assert_eq!(etat.country, "", "aucun pays impose par defaut");
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &PluginState { preset: 5, country: "BE".into() }).unwrap();
        let etat = load(&path);
        assert_eq!(etat.preset, 5);
        assert_eq!(etat.country, "BE");
    }

    #[test]
    fn un_fichier_dune_version_anterieure_se_relit() {
        // Sans `#[serde(default)]` sur `country`, une mise a jour de l'appareil
        // ferait echouer la lecture et repartir sur la preselection 1.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"preset":7}"#).unwrap();
        let etat = load(&path);
        assert_eq!(etat.preset, 7);
        assert_eq!(etat.country, "");
    }

    #[test]
    fn update_ne_detruit_pas_le_champ_quon_ne_touche_pas() {
        // Les deux moities du plugin ecrivent dans ce fichier : c'est
        // exactement le defaut que `update` existe pour eviter.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        update(&path, |s| s.country = "DE".into()).unwrap();
        update(&path, |s| s.preset = 4).unwrap();
        let etat = load(&path);
        assert_eq!(etat.preset, 4);
        assert_eq!(etat.country, "DE", "le pays doit survivre a une ecriture de preselection");
    }
}
