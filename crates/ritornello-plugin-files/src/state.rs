//! État persisté : la liste courante et la piste en cours.
//!
//! Même pattern que `crates/ritornello-plugin-radio/src/state.rs`, y compris
//! l'`update` qui préserve les champs qu'il ne touche pas — la moitié Admin
//! écrira in_dir ce même fichier, et un `save` reconstruit par la moitié Source
//! l'effacerait.

use ritornello_plugin_files::m3u::Entry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub playlist: Vec<StoredEntry>,
    #[serde(default)]
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEntry {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
}

impl From<&StoredEntry> for Entry {
    fn from(s: &StoredEntry) -> Self {
        Entry { path: s.path.clone(), title: s.title.clone(), duration_s: s.duration_s }
    }
}

impl From<&Entry> for StoredEntry {
    fn from(e: &Entry) -> Self {
        StoredEntry { path: e.path.clone(), title: e.title.clone(), duration_s: e.duration_s }
    }
}

/// Un fichier absent ou illisible rend l'état clear, **sans paniquer** : une
/// première installation, ou un `/var/lib` effacé, doit laisser le plugin
/// démarrer et non refuser de se run.
pub fn load(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Écriture atomique : `.tmp` puis `rename`, pour qu'une coupure ne laisse
/// jamais un fichier tronqué que le démarrage suivant jetterait en silence.
pub fn save(path: &Path, state: &State) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Relit, modifie, réécrit — et préserve donc ce que l'appelant ne touche pas.
pub fn update(path: &Path, f: impl FnOnce(&mut State)) -> anyhow::Result<()> {
    let mut state = load(path);
    f(&mut state);
    save(path, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entree_de_test() -> StoredEntry {
        StoredEntry {
            path: "/mnt/ritornello/nas/Album/01.mp3".into(),
            title: Some("So What".into()),
            duration_s: Some(245),
        }
    }

    #[test]
    fn un_etat_absent_ou_illisible_donne_un_etat_vide_sans_paniquer() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("absent.json")).index, 0);
        let abime = dir.path().join("abime.json");
        std::fs::write(&abime, b"{ ceci n'est pas du json").unwrap();
        assert!(load(&abime).playlist.is_empty());
    }

    #[test]
    fn la_liste_et_l_index_survivent_a_un_aller_retour() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-files.json");
        save(&f, &State { playlist: vec![entree_de_test()], index: 0 }).unwrap();
        let relu = load(&f);
        assert_eq!(relu.index, 0);
        assert_eq!(relu.playlist, vec![entree_de_test()]);
    }

    #[test]
    fn update_ne_perd_pas_les_champs_qu_il_ne_touche_pas() {
        // La moitié Admin écrit la liste in_dir ce même fichier ; un `save`
        // reconstruit par la moitié Source l'effacerait.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-files.json");
        save(&f, &State { playlist: vec![entree_de_test()], index: 0 }).unwrap();
        update(&f, |s| s.index = 1).unwrap();
        let relu = load(&f);
        assert_eq!(relu.index, 1);
        assert_eq!(relu.playlist.len(), 1, "la liste a ete effacee par l'update");
    }

    #[test]
    fn la_conversion_avec_lentree_m3u_fait_laller_retour() {
        let e = Entry {
            path: "/musique/01.mp3".into(),
            title: Some("So What".into()),
            duration_s: Some(245),
        };
        let stocke = StoredEntry::from(&e);
        assert_eq!(Entry::from(&stocke), e);
    }
}
