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
    walk_with(dir, cap, &|_, _| {})
}

/// Même marche, avec un **crochet de progression** appelé à chaque répertoire
/// visité : le nombre de pistes trouvées jusque-là, et le répertoire en cours.
///
/// Il existe parce qu'une marche sur un partage SMB endormi peut durer
/// longtemps, et que le protocole admin ne pousse rien : la page interroge, et
/// il lui faut quelque chose à montrer entre-temps. Sans lui, l'utilisateur
/// verrait un écran figé sans savoir si quelque chose avance.
pub fn walk_with(
    dir: &Path,
    cap: usize,
    progres: &dyn Fn(usize, &Path),
) -> Result<Vec<PathBuf>, ScanError> {
    let mut out = Vec::new();
    let mut vus: HashSet<PathBuf> = HashSet::new();
    marche(dir, cap, &mut out, &mut vus, progres)?;
    out.sort();
    Ok(out)
}

/// Contenu d'un seul niveau : les sous-répertoires et les fichiers audio, tous
/// deux triés. C'est ce que consomme l'arbre **paresseux** de la page, qui ne
/// demande jamais toute l'arborescence d'un coup.
pub fn list_dir(dir: &Path) -> Result<(Vec<String>, Vec<String>), ScanError> {
    let lecture =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let (mut dossiers, mut fichiers) = (Vec::new(), Vec::new());
    for entree in lecture.flatten() {
        let chemin = entree.path();
        let Some(nom) = chemin.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        // Les entrées cachées ne sont pas montrées : une bibliothèque en est
        // pleine (`.DS_Store`, `@eaDir` d'un Synology) et elles n'ont rien à
        // faire dans un arbre de navigation musicale.
        if nom.starts_with('.') {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&chemin) else { continue };
        if meta.is_dir() {
            dossiers.push(nom);
        } else if meta.is_file() && is_audio(&chemin) {
            fichiers.push(nom);
        }
    }
    dossiers.sort();
    fichiers.sort();
    Ok((dossiers, fichiers))
}

/// Cherche récursivement les fichiers audio dont le nom contient `motif`
/// (comparaison insensible à la casse), plafonnés à `cap` résultats.
///
/// Le plafond est **silencieux côté résultat mais rendu à l'appelant** : une
/// recherche large sur une grosse bibliothèque doit rendre la main, et la page
/// doit pouvoir dire « affinez » plutôt que d'afficher une liste tronquée sans
/// le signaler.
pub fn search(dir: &Path, motif: &str, cap: usize) -> Result<(Vec<PathBuf>, bool), ScanError> {
    let motif = motif.to_lowercase();
    if motif.is_empty() {
        return Ok((Vec::new(), false));
    }
    let tous = walk(dir, MAX_TRACKS)?;
    let trouves: Vec<PathBuf> = tous
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_lowercase().contains(&motif))
        })
        .collect();
    let tronque = trouves.len() > cap;
    Ok((trouves.into_iter().take(cap).collect(), tronque))
}

fn marche(
    dir: &Path,
    cap: usize,
    out: &mut Vec<PathBuf>,
    vus: &mut HashSet<PathBuf>,
    progres: &dyn Fn(usize, &Path),
) -> Result<(), ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    if !vus.insert(canon) {
        return Ok(());
    }
    progres(out.len(), dir);
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
        marche(&d, cap, out, vus, progres)?;
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
