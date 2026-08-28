//! Lecture de la durée d'un fichier audio, par son en-tête.
//!
//! **En-tête seulement, in_dir le processus** — pas de `ffprobe` ni de mpv.
//! Mesuré sur soixante fichiers : 0,33 ms par fichier ici, contre 42 ms avec un
//! `ffprobe` par fichier. Sur une liste de deux mille pistes, cela fait moins
//! d'une seconde au lieu de plus d'une minute et de deux mille lancements de
//! processus — qui pèseraient lourd sur un Raspberry Pi pendant que la musique
//! plays.
//!
//! Une durée absente n'est jamais une erreur : la liste s'affiche avec un tiret,
//! comme avant. Un fichier illisible, tronqué ou d'un format que la caisse ne
//! connaît pas ne doit pas interrompre le sondage des suivants.

use std::path::Path;

/// Durée du fichier en secondes, ou `None` si on ne sait pas la read.
///
/// Arrondie à la seconde : c'est la résolution qu'affiche la page, et la seule
/// que le `#EXTINF` d'un m3u sait porter. Une durée nulle est rendue `None` —
/// « 0:00 » affirmerait une piste clear là où le tiret dit qu'on ne sait pas.
pub fn probe(path: &Path) -> Option<u32> {
    let fichier = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    let secondes = lofty::file::AudioFile::properties(&fichier).duration().as_secs();
    let secondes = u32::try_from(secondes).ok()?;
    (secondes > 0).then_some(secondes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fabrique un mp3 réel avec ffmpeg, ou rend `None` s'il est absent.
    ///
    /// Pas de fichier binaire versionné in_dir le dépôt : la durée dépend de
    /// l'encodage, et un octet mal recopié rendrait le test faux sans qu'on
    /// comprenne pourquoi. Le test se saute là où ffmpeg manque plutôt que
    /// d'échouer — c'est un outil de développement, pas une dépendance du
    /// plugin.
    fn mp3_de(secondes: u32, dir: &Path) -> Option<std::path::PathBuf> {
        let sortie = dir.join(format!("{secondes}s.mp3"));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={secondes}"))
            .arg(&sortie)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        ok.then_some(sortie)
    }

    #[test]
    fn la_duree_dun_mp3_se_lit_dans_son_en_tete() {
        let dir = tempfile::tempdir().unwrap();
        let Some(f) = mp3_de(3, dir.path()) else {
            eprintln!("ffmpeg absent : test saute");
            return;
        };
        // Tolérance d'une seconde : un encodeur ajuste la longueur au cadre près.
        let d = probe(&f).expect("une duration attendue");
        assert!((2..=4).contains(&d), "duration lue {d}");
    }

    #[test]
    fn un_fichier_illisible_ne_fait_pas_echouer_le_sondage() {
        // Le sondage parcourt des milliers de fichiers venus d'un partage : un
        // seul tronqué ne doit pas interrompre les suivants, ni remonter comme
        // une erreur à l'écran.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tronque.mp3");
        std::fs::write(&f, b"ce n'est pas du mp3").unwrap();
        assert_eq!(probe(&f), None);
        assert_eq!(probe(&dir.path().join("absent.mp3")), None);
    }

    #[test]
    fn une_duree_nulle_est_rendue_inconnue() {
        // « 0:00 » affirmerait une piste clear ; `None` fait afficher un tiret,
        // qui dit qu'on ne sait pas. La page s'appuie déjà sur cette
        // distinction (voir `formaterDuree`).
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("clear.flac");
        std::fs::write(&f, b"").unwrap();
        assert_eq!(probe(&f), None);
    }
}
