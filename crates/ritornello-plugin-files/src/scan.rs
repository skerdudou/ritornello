//! Marche récursive d'un répertoire : filtre d'extensions, garde contre les
//! boucles de liens symboliques, plafond.

use ritornello_i18n::Catalog;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Plafond d'une liste. Protège trois choses à la fois : la charge utile JSON
/// servie à la page, l'écriture du m3u, et la liste de lecture de mpv.
pub const MAX_TRACKS: usize = 2000;

/// Plafond de **visite** d'une recherche, distinct de `MAX_TRACKS`.
///
/// `MAX_TRACKS` borne ce que la liste de lecture peut contenir ; le confondre
/// avec le coût d'un parcours faisait refuser toute recherche lancée dans un
/// dossier de plus de 2000 pistes, avec le message de l'ajout — mesuré à la
/// racine d'un NAS. Une recherche ne remplit rien : elle n'a besoin que d'une
/// borne qui l'empêche de tourner sans fin, et le dépassement se rapporte
/// comme « tronqué ».
///
/// Compte désormais **chaque entrée inspectée** — dossier ou fichier, audio ou
/// non — et non plus seulement les fichiers audio rencontrés : un dossier
/// plein de fichiers non-audio n'était borné par rien avant ce changement.
/// Relevé en conséquence : c'est [`DELAI_RECHERCHE`] qui protège désormais un
/// partage lent (le coût dominant y est le `read_dir` par dossier, pas le
/// compte d'entrées) ; ce plafond reste un filet pour le cas local, rapide par
/// entrée, où seul un nombre démesuré d'entrées doit être refusé.
pub const MAX_VISITES: usize = 500_000;

/// Délai maximal accordé à une recherche.
///
/// Franchement sous les 5 s du protocole d'admin, qui est **sériel** : passé
/// ce délai, les `get_data` du sondage de la page s'empilent derrière la
/// recherche en cours et expirent tous — c'est le mode de panne de l'incident
/// du 2026-08-17, où la page disparaissait. La marge restante couvre la
/// résolution des chemins et la sérialisation qui suivent la marche.
pub const DELAI_RECHERCHE: Duration = Duration::from_secs(3);

/// Nombre d'entrées inspectées entre deux mesures de l'échéance.
///
/// `Instant::elapsed` n'est pas gratuit : le mesurer à chaque entrée
/// ajouterait un appel système par fichier, sur le chemin le plus chaud de la
/// marche.
const PAS_ECHEANCE: usize = 64;

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

/// Extensions des fichiers de liste de lecture.
///
/// Écartées des extensions audio : un m3u ne s'ajoute pas à la liste, il la
/// **remplace**. Les confondre ferait ajouter un fichier texte que mpv
/// tenterait de jouer.
const EXTENSIONS_LISTE: &[&str] = &["m3u", "m3u8"];

pub fn is_playlist(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS_LISTE.iter().any(|k| k.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Contenu d'un seul niveau, chaque catégorie triée.
///
/// Une structure nommée plutôt qu'un triplet : trois `Vec<String>` anonymes
/// s'inversent au premier refactor, et l'erreur se voit seulement à l'écran.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Contenu {
    pub dossiers: Vec<String>,
    pub audio: Vec<String>,
    /// Fichiers de liste de lecture, qui se **chargent** au lieu de s'ajouter.
    pub listes: Vec<String>,
}

/// Contenu d'un seul niveau : sous-répertoires, fichiers audio et listes de
/// lecture. C'est ce que consomme l'arbre **paresseux** de la page, qui ne
/// demande jamais toute l'arborescence d'un coup.
pub fn list_dir(dir: &Path) -> Result<Contenu, ScanError> {
    let lecture =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut out = Contenu::default();
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
            out.dossiers.push(nom);
        } else if meta.is_file() && is_audio(&chemin) {
            out.audio.push(nom);
        } else if meta.is_file() && is_playlist(&chemin) {
            out.listes.push(nom);
        }
    }
    out.dossiers.sort();
    out.audio.sort();
    out.listes.sort();
    Ok(out)
}

/// Pourquoi une recherche s'est arrêtée.
///
/// Deux causes, deux conseils à donner : trop de correspondances invite à
/// préciser le motif, un parcours interrompu invite à descendre dans un
/// sous-dossier. Les confondre faisait afficher « Aucun résultat » — donc « ce
/// fichier n'existe pas » — à quelqu'un dont la recherche avait simplement
/// renoncé avant d'arriver jusqu'à lui.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinDeRecherche {
    /// Tout le dossier a été parcouru.
    Complete,
    /// Le plafond de résultats est atteint : il y en avait davantage.
    TropDeResultats,
    /// Le parcours a été interrompu avant d'avoir tout vu.
    Interrompue,
}

/// Cherche récursivement les fichiers audio dont le nom contient `motif`
/// (comparaison insensible à la casse).
///
/// Deux bornes, et deux raisons distinctes : `cap` limite ce qu'on **rapporte**
/// à la page, `plafond_visites` ce qu'on accepte de **parcourir**. L'une comme
/// l'autre rend une [`FinDeRecherche`] distincte, jamais un refus : une liste
/// partielle annoncée comme telle est utile, un refus ne l'est pas.
///
/// Le filtre s'applique **pendant** la marche : collecter d'abord tout le
/// dossier pour ne garder ensuite qu'une poignée de noms était ce qui faisait
/// buter la recherche sur le plafond de la liste de lecture.
pub fn search(
    dir: &Path,
    motif: &str,
    cap: usize,
    plafond_visites: usize,
    delai: Duration,
) -> Result<(Vec<PathBuf>, FinDeRecherche), ScanError> {
    let motif = motif.to_lowercase();
    if motif.is_empty() {
        return Ok((Vec::new(), FinDeRecherche::Complete));
    }
    let mut out = Vec::new();
    let mut visites = 0usize;
    let mut vus = HashSet::new();
    let debut = Instant::now();
    // `cap + 1` : on en cherche un de plus que ce qu'on rend, pour distinguer
    // « exactement cap résultats » de « il y en avait davantage ». Sans cela une
    // liste complète de cap éléments serait annoncée comme tronquée.
    let arrete = marche_cherchant(
        dir,
        &motif,
        cap + 1,
        plafond_visites,
        debut,
        delai,
        &mut out,
        &mut visites,
        &mut vus,
    )?;
    out.truncate(cap);
    Ok((out, arrete.unwrap_or(FinDeRecherche::Complete)))
}

/// Marche filtrante. Rend la cause d'un arrêt anticipé, `None` si la marche a
/// couvert tout le dossier.
// Neuf paramètres : les trois derniers sont l'état de la récursion, les six
// premiers ses bornes. Les regrouper dans une struct n'apporterait qu'un nom
// de plus à lire — accepté tel quel.
#[allow(clippy::too_many_arguments)]
fn marche_cherchant(
    dir: &Path,
    motif: &str,
    cap: usize,
    plafond_visites: usize,
    debut: Instant,
    delai: Duration,
    out: &mut Vec<PathBuf>,
    visites: &mut usize,
    vus: &mut HashSet<PathBuf>,
) -> Result<Option<FinDeRecherche>, ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    // Même garde que `marche` : un lien pointant vers un ancêtre ferait tourner
    // la marche en produisant des chemins de plus en plus longs.
    if !vus.insert(canon) {
        return Ok(None);
    }
    let lecture =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut sous_dossiers = Vec::new();
    for entree in lecture {
        let Ok(entree) = entree else { continue };
        let chemin = entree.path();
        // Chaque entrée compte, dossier ou fichier, audio ou non : un dossier
        // plein de fichiers non-audio n'était borné par rien tant que seuls
        // les fichiers audio étaient comptés.
        *visites += 1;
        if *visites > plafond_visites {
            return Ok(Some(FinDeRecherche::Interrompue));
        }
        // Mesurée toutes les `PAS_ECHEANCE` entrées, pas à chaque entrée :
        // `Instant::elapsed` n'est pas gratuit.
        if visites.is_multiple_of(PAS_ECHEANCE) && debut.elapsed() >= delai {
            return Ok(Some(FinDeRecherche::Interrompue));
        }
        // `metadata` et non `symlink_metadata`, comme dans `marche` : un lien
        // vers un dossier réel doit être suivi.
        let Ok(meta) = std::fs::metadata(&chemin) else { continue };
        if meta.is_dir() {
            sous_dossiers.push(chemin);
            continue;
        }
        if !(meta.is_file() && is_audio(&chemin)) {
            continue;
        }
        let correspond = chemin
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_lowercase().contains(motif));
        if correspond {
            out.push(chemin);
            if out.len() >= cap {
                return Ok(Some(FinDeRecherche::TropDeResultats));
            }
        }
    }
    sous_dossiers.sort();
    for d in sous_dossiers {
        if let Some(raison) =
            marche_cherchant(&d, motif, cap, plafond_visites, debut, delai, out, visites, vus)?
        {
            return Ok(Some(raison));
        }
    }
    Ok(None)
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
    fn un_niveau_separe_dossiers_pistes_et_listes_de_lecture() {
        // Les listes voyagent à part parce qu'elles portent une action
        // différente : elles **remplacent** la liste en cours au lieu de s'y
        // ajouter. Les ranger avec les pistes ferait ajouter un fichier texte
        // que mpv tenterait de jouer.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Album")).unwrap();
        for nom in ["a.mp3", "tout.m3u", "autre.M3U8", "pochette.jpg", ".cache.m3u"] {
            fichier(dir.path(), nom);
        }
        let c = list_dir(dir.path()).unwrap();
        assert_eq!(c.dossiers, vec!["Album"]);
        assert_eq!(c.audio, vec!["a.mp3"]);
        // Casse indifférente, comme pour l'audio ; l'entrée cachée reste écartée.
        assert_eq!(c.listes, vec!["autre.M3U8", "tout.m3u"]);
    }

    #[test]
    fn un_m3u_nest_pas_un_fichier_audio() {
        // Garde-fou de la séparation ci-dessus, du côté des prédicats : un
        // balayage récursif ne doit pas ramasser les listes comme des pistes.
        assert!(is_playlist(Path::new("x/tout.m3u")));
        assert!(is_playlist(Path::new("x/tout.M3U8")));
        assert!(!is_audio(Path::new("x/tout.m3u")));
        assert!(!is_playlist(Path::new("x/piste.mp3")));
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
    fn une_recherche_au_dela_du_plafond_tronque_au_lieu_de_refuser() {
        // Symptôme mesuré sur un vrai NAS : chercher à la racine renvoyait « this
        // folder holds more than 2000 tracks: narrow it down, or add its
        // subfolders one by one » — le message de l'AJOUT — pour une recherche
        // qui n'ajoute rien à la liste. La cause : `search` réutilisait
        // `MAX_TRACKS`, le plafond de la liste de lecture, comme plafond de
        // marche. Une recherche trop large se tronque et le dit ; elle ne se
        // refuse pas.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        let (trouves, fin) = search(dir.path(), "mp3", 200, 3, DELAI_RECHERCHE).expect("aucun refus attendu");
        assert_ne!(fin, FinDeRecherche::Complete, "un plafond atteint doit se dire");
        assert!(!trouves.is_empty(), "des resultats partiels valent mieux que rien");
    }

    #[test]
    fn une_recherche_interrompue_par_le_plafond_de_visites_le_dit_comme_telle() {
        // Défaut trouvé en revue : la marche rendait `Ok(true)` que le plafond
        // atteint soit celui des VISITES ou celui des RÉSULTATS, et la page
        // affichait alors « Aucun résultat » — donc « ce fichier n'existe pas »
        // — pour une recherche qui avait simplement renoncé avant d'arriver
        // jusqu'à lui. Ici le plafond de visites est atteint bien avant celui
        // des résultats (200) : la cause doit se distinguer.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        let (_, fin) = search(dir.path(), "mp3", 200, 3, DELAI_RECHERCHE).unwrap();
        assert_eq!(fin, FinDeRecherche::Interrompue);
    }

    #[test]
    fn une_recherche_qui_depasse_le_plafond_de_resultats_le_dit_comme_telle() {
        // L'autre cause d'arrêt : ici le plafond de visites est large, seul
        // celui des résultats est en cause.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("miles{i}.mp3"));
        }
        let (trouves, fin) = search(dir.path(), "miles", 3, MAX_VISITES, DELAI_RECHERCHE).unwrap();
        assert_eq!(fin, FinDeRecherche::TropDeResultats);
        assert_eq!(trouves.len(), 3);
    }

    #[test]
    fn une_recherche_qui_a_tout_parcouru_est_complete() {
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "A/miles.flac");
        let (_, fin) = search(dir.path(), "miles", 200, MAX_VISITES, DELAI_RECHERCHE).unwrap();
        assert_eq!(fin, FinDeRecherche::Complete);
    }

    #[test]
    fn une_recherche_avec_exactement_cap_resultats_est_complete() {
        // Régime non couvert avant la revue, et pourtant toute la raison du
        // `cap + 1` : sans lui, une liste complète de `cap` éléments serait
        // annoncée comme tronquée alors qu'elle est exhaustive.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..3 {
            fichier(dir.path(), &format!("miles{i}.mp3"));
        }
        let (trouves, fin) = search(dir.path(), "miles", 3, MAX_VISITES, DELAI_RECHERCHE).unwrap();
        assert_eq!(trouves.len(), 3);
        assert_eq!(fin, FinDeRecherche::Complete);
    }

    #[test]
    fn une_recherche_avec_plus_de_cap_resultats_et_un_plafond_de_visites_large_est_tronquee() {
        // L'autre régime non couvert : le plafond de visites n'entre pas en
        // ligne de compte, seul celui des résultats doit se déclencher.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            fichier(dir.path(), &format!("miles{i}.mp3"));
        }
        let (trouves, fin) = search(dir.path(), "miles", 3, 1_000_000, DELAI_RECHERCHE).unwrap();
        assert_eq!(trouves.len(), 3);
        assert_eq!(fin, FinDeRecherche::TropDeResultats);
    }

    #[test]
    fn le_delai_de_recherche_est_franchement_sous_le_plafond_du_protocole() {
        // Le protocole admin abandonne une requête après 5 s ; il faut de la
        // marge pour la résolution des chemins et la sérialisation qui suivent
        // la marche, sans quoi le délai lui-même dépasserait le plafond du
        // cœur.
        assert!(DELAI_RECHERCHE < std::time::Duration::from_secs(5));
    }

    #[test]
    fn une_recherche_qui_depasse_son_delai_est_interrompue_sans_attendre() {
        // Le compte de visites ne protège pas un partage lent : le coût y est
        // dominant par `read_dir`, pas par entrée. Un délai nul permet
        // d'observer l'interruption sans faire dépendre le test d'une horloge.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(PAS_ECHEANCE * 2) {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        let (_, fin) = search(dir.path(), "mp3", 200, MAX_VISITES, Duration::ZERO).unwrap();
        assert_eq!(fin, FinDeRecherche::Interrompue);
    }

    #[test]
    fn un_dossier_plein_de_fichiers_non_audio_est_borne_par_le_plafond_de_visites() {
        // Défaut corrigé : `visites` ne comptait que les fichiers AUDIO, donc un
        // dossier plein de fichiers non-audio n'était borné par rien — la
        // marche pouvait inspecter un nombre de fichiers arbitraire sans jamais
        // s'arrêter sur ce plafond.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.txt"));
        }
        let (trouves, fin) = search(dir.path(), "txt", 200, 3, DELAI_RECHERCHE).unwrap();
        assert_eq!(fin, FinDeRecherche::Interrompue);
        assert!(trouves.is_empty(), "aucun fichier non-audio ne doit etre rapporte");
    }

    // Les deux plafonds ne mesurent pas la même chose : `MAX_TRACKS` borne ce
    // qu'on peut AJOUTER, `MAX_VISITES` ce qu'on peut PARCOURIR en cherchant.
    // Les confondre est exactement le défaut corrigé ici. Vérifié à la
    // compilation : un test sur deux constantes ne peut pas échouer à
    // l'exécution, clippy le refuse à raison.
    const _: () = assert!(MAX_VISITES > MAX_TRACKS);

    #[test]
    fn une_recherche_rend_les_correspondances_et_seulement_elles() {
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "A/miles.flac");
        fichier(dir.path(), "A/autre.mp3");
        fichier(dir.path(), "B/sous/MILES live.mp3");
        let (trouves, fin) = search(dir.path(), "miles", 200, MAX_VISITES, DELAI_RECHERCHE).unwrap();
        let mut relatifs: Vec<String> = trouves
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        relatifs.sort();
        // Insensible à la casse, et sur le nom de fichier seul.
        assert_eq!(relatifs, vec!["A/miles.flac", "B/sous/MILES live.mp3"]);
        assert_eq!(fin, FinDeRecherche::Complete, "trois fichiers ne remplissent aucun plafond");
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
