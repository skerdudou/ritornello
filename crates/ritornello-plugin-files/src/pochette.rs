//! La pochette posée à côté des fichiers : `folder.jpg` et ses cousins.
//!
//! C'est le greffon qui fait ce travail, et non le cœur : c'est lui qui a monté
//! le partage et qui connaît la racine de la source déclarée. Et un
//! `folder.jpg` n'a rien à extraire — le chemin suffit, donc rien ne transite
//! en octets sur le canal.

use ritornello_proto::CoverRef;
use std::path::{Path, PathBuf};

/// Par ordre de préférence. `cover` d'abord : c'est le nom le plus explicite.
const PREFERENCES: [&str; 5] = ["cover", "folder", "front", "albumart", "album"];

/// Extensions reconnues.
const EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// Sous-répertoires d'artwork visités, sur **un seul** niveau.
const SOUS_REPERTOIRES: [&str; 4] = ["artwork", "scans", "covers", "art"];

/// Ce qui n'est pas la face avant.
///
/// Ne s'applique **qu'à la règle de l'image unique**, la seule qui devine : les
/// listes de préférence ne retiennent qu'un nom qu'elles connaissent, donc un
/// répertoire portant `front.jpg` et `back.jpg` est réglé par la préférence.
const EXCLUS: [&str; 8] =
    ["back", "verso", "inlay", "cd", "disc", "disque", "booklet", "matrix"];

/// Cherche la pochette du fichier joué. `None` = rien de sûr, on se tait.
pub fn cherche(fichier: &Path) -> Option<CoverRef> {
    let repertoire = fichier.parent()?;
    if let Some(p) = par_preference(repertoire) {
        return Some(chemin(p));
    }
    for sous in SOUS_REPERTOIRES {
        let Some(candidat) = sous_repertoire(repertoire, sous) else { continue };
        if let Some(p) = par_preference(&candidat) {
            return Some(chemin(p));
        }
    }
    image_unique(repertoire).map(chemin)
}

fn chemin(p: PathBuf) -> CoverRef {
    CoverRef::Path { path: p.to_string_lossy().into_owned() }
}

/// Le sous-répertoire d'artwork, quel que soit sa casse.
fn sous_repertoire(repertoire: &Path, nom: &str) -> Option<PathBuf> {
    std::fs::read_dir(repertoire)
        .ok()?
        .flatten()
        .find(|e| {
            e.file_name().to_string_lossy().eq_ignore_ascii_case(nom)
                && e.file_type().is_ok_and(|t| t.is_dir())
        })
        .map(|e| e.path())
}

/// Le premier nom de la liste de préférence présent dans le répertoire.
fn par_preference(repertoire: &Path) -> Option<PathBuf> {
    let images = images_de(repertoire);
    PREFERENCES.iter().find_map(|prefere| {
        images
            .iter()
            .find(|p| {
                p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(prefere))
            })
            .cloned()
    })
}

/// L'unique image du répertoire, si elle est unique **et** si son nom ne dit
/// pas qu'elle est autre chose que la face avant.
fn image_unique(repertoire: &Path) -> Option<PathBuf> {
    let images = images_de(repertoire);
    let [seule] = images.as_slice() else { return None };
    let tige = seule.file_stem()?.to_string_lossy().to_ascii_lowercase();
    EXCLUS.iter().all(|exclu| !tige.contains(exclu)).then(|| seule.clone())
}

fn images_de(repertoire: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(repertoire)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| {
                    EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
                })
        })
        .collect();
    // `read_dir` ne garantit aucun ordre : trier rend le choix reproductible.
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fabrique un repertoire avec les fichiers nommes, et rend son chemin.
    fn arbre(noms: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for nom in noms {
            let chemin = dir.path().join(nom);
            std::fs::create_dir_all(chemin.parent().unwrap()).unwrap();
            std::fs::write(&chemin, b"x").unwrap();
        }
        dir
    }

    fn trouve(dir: &tempfile::TempDir) -> Option<String> {
        match cherche(&dir.path().join("01 - piste.flac")) {
            Some(ritornello_proto::CoverRef::Path { path }) => {
                Some(std::path::Path::new(&path).file_name().unwrap().to_string_lossy().into_owned())
            }
            _ => None,
        }
    }

    #[test]
    fn l_ordre_de_preference_gagne_sur_l_ordre_alphabetique() {
        let dir = arbre(&["01 - piste.flac", "albumart.png", "cover.jpg", "front.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn la_casse_ne_compte_pas() {
        let dir = arbre(&["01 - piste.flac", "Folder.JPG"]);
        assert_eq!(trouve(&dir).as_deref(), Some("Folder.JPG"));
    }

    #[test]
    fn une_image_unique_sans_nom_reconnaissable_est_prise() {
        let dir = arbre(&["01 - piste.flac", "scan001.png"]);
        assert_eq!(trouve(&dir).as_deref(), Some("scan001.png"));
    }

    #[test]
    fn une_image_unique_nommee_comme_un_dos_est_ecartee() {
        // Sans cette exclusion, on afficherait le dos du boitier. Et se taire
        // laisse le relai generique prendre la main.
        for dos in ["back.jpg", "Scan_verso.png", "inlay.jpg", "booklet.png", "cd.jpg"] {
            let dir = arbre(&["01 - piste.flac", dos]);
            assert_eq!(trouve(&dir), None, "{dos} ne devrait pas etre retenu");
        }
    }

    #[test]
    fn deux_images_sans_nom_reconnaissable_ne_tranchent_rien() {
        let dir = arbre(&["01 - piste.flac", "scan001.png", "scan002.png"]);
        assert_eq!(trouve(&dir), None);
    }

    #[test]
    fn l_exclusion_ne_s_applique_pas_a_la_liste_de_preference() {
        // `cd` est un motif d'exclusion, mais un fichier nomme `cover.jpg` est
        // retenu sans discussion : l'exclusion ne concerne que la regle qui
        // devine.
        let dir = arbre(&["01 - piste.flac", "cover.jpg", "back.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn un_sous_repertoire_d_artwork_est_visite_sur_un_seul_niveau() {
        let dir = arbre(&["01 - piste.flac", "Artwork/front.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("front.jpg"));
        // Deux niveaux : on ne parcourt pas un NAS pour trouver une image.
        let profond = arbre(&["01 - piste.flac", "Artwork/haute-def/front.jpg"]);
        assert_eq!(trouve(&profond), None);
    }

    #[test]
    fn le_repertoire_passe_devant_le_sous_repertoire() {
        let dir = arbre(&["01 - piste.flac", "folder.jpg", "Artwork/cover.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("folder.jpg"));
    }
}
