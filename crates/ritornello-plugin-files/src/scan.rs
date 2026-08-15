//! Marche récursive d'un répertoire : filtre d'extensions, garde contre les
//! boucles de liens symboliques, plafond.

use ritornello_i18n::Catalog;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Plafond d'une liste. Protège trois choses à la fois : la charge utile JSON
/// servie à la page, l'écriture du m3u, et la liste de lecture de mpv.
pub const MAX_TRACKS: usize = 2000;

const EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "aiff", "ape", "wv", "mpc",
];

#[derive(Debug)]
pub enum ScanError {
    TooMany { cap: usize },
    Io { path: String },
}

pub fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.iter().any(|k| k.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Parcourt `dir` récursivement et rend les fichiers audio, **triés**.
///
/// Le tri rend l'ajout reproductible : sans lui, deux ajouts du même dossier
/// donneraient des numéros de présélection différents d'un jour à l'autre,
/// l'ordre de `read_dir` n'étant garanti par aucun système de fichiers.
///
/// La garde anti-boucle mémorise les répertoires **canonisés** déjà visités :
/// un lien symbolique pointant vers un ancêtre ferait sinon tourner la marche
/// jusqu'au plafond, avec un symptôme qui ressemble à une bibliothèque énorme
/// plutôt qu'à un défaut.
pub fn walk(dir: &Path, cap: usize) -> Result<Vec<PathBuf>, ScanError> {
    let mut out = Vec::new();
    let mut vus: HashSet<PathBuf> = HashSet::new();
    marche(dir, cap, &mut out, &mut vus)?;
    out.sort();
    Ok(out)
}

fn marche(
    dir: &Path,
    cap: usize,
    out: &mut Vec<PathBuf>,
    vus: &mut HashSet<PathBuf>,
) -> Result<(), ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    if !vus.insert(canon) {
        return Ok(());
    }
    let lecture =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut sous_dossiers = Vec::new();
    for entree in lecture {
        let Ok(entree) = entree else { continue };
        let chemin = entree.path();
        // `metadata` et non `symlink_metadata` : un lien vers un dossier réel
        // doit être suivi. C'est la boucle qu'on refuse, pas le lien. Un lien
        // cassé ou un répertoire interdit fait passer au suivant plutôt que
        // d'échouer : une bibliothèque est rarement parfaite, et refuser tout
        // l'ajout pour un fichier bancal serait disproportionné.
        let Ok(meta) = std::fs::metadata(&chemin) else { continue };
        if meta.is_dir() {
            sous_dossiers.push(chemin);
        } else if meta.is_file() && is_audio(&chemin) {
            if out.len() >= cap {
                return Err(ScanError::TooMany { cap });
            }
            out.push(chemin);
        }
    }
    sous_dossiers.sort();
    for d in sous_dossiers {
        marche(&d, cap, out, vus)?;
    }
    Ok(())
}

impl ScanError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ScanError::TooMany { cap } => {
                catalog.get("too_many_tracks").replace("{cap}", &cap.to_string())
            }
            ScanError::Io { path } => catalog.get("scan_io_error").replace("{path}", path),
        }
    }
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::TooMany { cap } => write!(f, "more than {cap} tracks"),
            ScanError::Io { path } => write!(f, "cannot read {path}"),
        }
    }
}

impl std::error::Error for ScanError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn fichier(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn seuls_les_fichiers_audio_sont_retenus_quelle_que_soit_la_casse() {
        let dir = tempfile::tempdir().unwrap();
        for nom in ["a.mp3", "b.FLAC", "c.Opus", "pochette.jpg", "notes.txt", "sans-extension"] {
            fichier(dir.path(), nom);
        }
        let mut noms: Vec<String> = walk(dir.path(), MAX_TRACKS)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        noms.sort();
        assert_eq!(noms, vec!["a.mp3", "b.FLAC", "c.Opus"]);
    }

    #[test]
    fn la_marche_est_recursive_et_ordonnee() {
        // L'ordre doit être stable : deux ajouts du même dossier produisent la
        // même liste, sinon les numéros de présélection changeraient d'un jour
        // à l'autre.
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "A/02.mp3");
        fichier(dir.path(), "A/01.mp3");
        fichier(dir.path(), "B/sous/03.mp3");
        let relatifs: Vec<String> = walk(dir.path(), MAX_TRACKS)
            .unwrap()
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(relatifs, vec!["A/01.mp3", "A/02.mp3", "B/sous/03.mp3"]);
    }

    #[test]
    fn le_plafond_est_refuse_et_non_tronque_en_silence() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        assert!(matches!(walk(dir.path(), 3), Err(ScanError::TooMany { cap: 3 })));
    }

    #[cfg(unix)]
    #[test]
    fn une_boucle_de_liens_symboliques_ne_fait_pas_tourner_la_marche() {
        // Sans garde, un lien pointant vers un ancêtre fait tourner la marche
        // jusqu'au plafond en produisant des chemins de plus en plus longs. Le
        // symptôme ressemble à une bibliothèque énorme, pas à un défaut.
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "sous/a.mp3");
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sous/boucle")).unwrap();
        let trouves = walk(dir.path(), MAX_TRACKS).unwrap();
        assert_eq!(trouves.len(), 1, "la boucle a ete suivie : {trouves:?}");
    }

    #[test]
    fn un_repertoire_inexistant_donne_une_erreur_nommee() {
        let dir = tempfile::tempdir().unwrap();
        let err = walk(&dir.path().join("absent"), MAX_TRACKS).unwrap_err();
        assert!(matches!(err, ScanError::Io { .. }));
    }

    #[test]
    fn chaque_refus_resout_contre_le_catalogue_embarque() {
        let catalog = Catalog::load("files", "en", Path::new("/inexistant"), crate::FILES_EN);
        for m in [
            ScanError::TooMany { cap: 2000 }.message(&catalog),
            ScanError::Io { path: "/mnt/ritornello/nas".into() }.message(&catalog),
        ] {
            assert!(m.contains(' '), "message reduit a une cle brute : {m:?}");
        }
        let borne = ScanError::TooMany { cap: 2000 }.message(&catalog);
        assert!(borne.contains("2000"), "borne non interpolee : {borne:?}");
        assert!(!borne.contains("{cap}"), "jeton laisse tel quel : {borne:?}");
    }
}
