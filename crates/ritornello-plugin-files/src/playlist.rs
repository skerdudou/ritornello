//! La liste en cours : les pistes, la piste courante, et le m3u qu'on donne à
//! mpv.

use crate::m3u::{render, Entry};
use ritornello_proto::Preset;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playlist {
    pub entries: Vec<Entry>,
    pub index: usize,
}

impl Playlist {
    /// Combien de pistes portent un chiffre de télécommande.
    ///
    /// `preset` est un `u8` de plage 1–99 : au-delà, les pistes restent
    /// atteignables par next/prev et par la liste de la page, mais aucun
    /// chiffre ne les désigne. Ce n'est pas contourné — c'est déclaré.
    pub fn preset_count(&self) -> u8 {
        self.entries.len().min(99) as u8
    }

    /// Les présélections **nommées** : un numéro et le titre de la piste.
    ///
    /// La source n'a longtemps annoncé qu'un `preset_count`, si bien que la
    /// grille de la page d'accueil ne montrait que des numéros nus là où la
    /// radio affiche « 1 · FIP ». Le name existait pourtant déjà — c'est celui
    /// que `preset_name` publie pour la piste courante, et celui que le m3u
    /// écrit en `#EXTINF`.
    ///
    /// **Dense et bornée à 99, exactement comme `preset_count`** : les deux
    /// décrivent la même chose et doivent rester d'accord. Une liste de fichiers
    /// n'a pas de trous — les numéros suivent les positions — donc l'indice est
    /// bien « la position plus un », ce qui n'est *pas* vrai d'une table de
    /// stations creuse (voir la doc du greffon MPD, § Dense positions, sparse
    /// indices).
    pub fn presets(&self) -> Vec<Preset> {
        self.entries
            .iter()
            .take(usize::from(self.preset_count()))
            .enumerate()
            .map(|(i, e)| Preset { index: (i + 1) as u8, name: e.display_name() })
            .collect()
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.index)
    }

    /// Numéro de présélection de ce qui plays (1-based), plafonné à 99 pour
    /// tenir in_dir un `u8`.
    pub fn preset(&self) -> Option<u8> {
        (self.index < self.entries.len()).then(|| (self.index + 1).min(99) as u8)
    }

    /// Positionne sur la présélection `n` (1-based). Rend `false` — **sans
    /// déplacer la playback** — quand elle n'existe pas : un échec de sélection
    /// ne doit pas interrompre ce qui plays.
    pub fn select(&mut self, n: u8) -> bool {
        if n == 0 || usize::from(n) > self.entries.len() {
            return false;
        }
        self.index = usize::from(n) - 1;
        true
    }

    /// Recale l'index sur une piste annoncée par le player. Rend `false` pour
    /// un index hors liste — mpv dit `-1` en fin de liste, et le cœur le relaie
    /// tel quel.
    pub fn set_index(&mut self, n: i64) -> bool {
        let Ok(i) = usize::try_from(n) else { return false };
        if i >= self.entries.len() {
            return false;
        }
        self.index = i;
        true
    }

    /// Écrit la liste destinée à mpv : chemins **absolus**, pour qu'elle ne
    /// dépende d'aucun répertoire courant. Écriture atomique — une coupure ne
    /// doit pas laisser un m3u tronqué que mpv lirait à moitié.
    pub fn write_for_mpv(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("m3u.tmp");
        std::fs::write(&tmp, render(&self.entries, None))?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn liste_de(n: usize) -> Playlist {
        Playlist {
            entries: (1..=n)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/musique/{i:02}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect(),
            index: 0,
        }
    }

    #[test]
    fn le_compte_de_preselections_est_plafonne_a_99() {
        assert_eq!(liste_de(150).preset_count(), 99);
        assert_eq!(liste_de(12).preset_count(), 12);
        assert_eq!(Playlist::default().preset_count(), 0);
    }

    #[test]
    fn les_preselections_nommees_suivent_les_positions_et_le_meme_plafond() {
        // Le name est celui que `preset_name` publie déjà pour la piste
        // courante : les tuiles de la grille et le player doivent dire la même
        // chose de la même piste.
        let p = liste_de(3);
        assert_eq!(
            p.presets(),
            vec![
                Preset { index: 1, name: "01".into() },
                Preset { index: 2, name: "02".into() },
                Preset { index: 3, name: "03".into() },
            ]
        );
        // Le même cap que `preset_count`, et il doit le rester : une
        // présélection annoncée que `Command::Select` ne peut pas atteindre
        // ferait une tuile qui ne plays rien.
        let longue = liste_de(150);
        assert_eq!(longue.presets().len(), usize::from(longue.preset_count()));
        assert_eq!(longue.presets().last().unwrap().index, 99);
        assert!(Playlist::default().presets().is_empty());
    }

    #[test]
    fn selectionner_hors_bornes_echoue_sans_bouger_l_index() {
        let mut p = liste_de(3);
        p.index = 1;
        assert!(!p.select(0), "le zero n'est pas une presentation");
        assert!(!p.select(4));
        assert_eq!(p.index, 1, "un failure ne doit pas deplacer la playback");
        assert!(p.select(3));
        assert_eq!(p.index, 2);
    }

    #[test]
    fn un_index_negatif_ou_hors_liste_est_ecarte() {
        // mpv dit -1 en fin de liste, et le cœur le transmet tel quel.
        let mut p = liste_de(3);
        assert!(!p.set_index(-1));
        assert!(!p.set_index(3));
        assert_eq!(p.index, 0, "l'index ne doit pas avoir bouge");
        assert!(p.set_index(2));
        assert_eq!(p.index, 2);
    }

    #[test]
    fn la_preselection_suit_l_index_et_disparait_sur_une_liste_vide() {
        let mut p = liste_de(3);
        p.index = 2;
        assert_eq!(p.preset(), Some(3));
        assert_eq!(Playlist::default().preset(), None);
    }

    #[test]
    fn le_m3u_de_mpv_porte_des_chemins_absolus() {
        // Il est écrit in_dir le répertoire d'état et lu par un autre processus :
        // un path relatif s'y résoudrait contre le répertoire courant de mpv.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plugin-files.m3u");
        liste_de(2).write_for_mpv(&f).unwrap();
        let texte = std::fs::read_to_string(&f).unwrap();
        assert!(texte.starts_with("#EXTM3U\n"));
        assert!(texte.contains("\n/musique/01.mp3\n"), "{texte}");
        assert!(texte.contains("\n/musique/02.mp3\n"), "{texte}");
        // Et rien ne traîne du fichier temporaire.
        assert!(!dir.path().join("plugin-files.m3u.tmp").exists());
    }
}
