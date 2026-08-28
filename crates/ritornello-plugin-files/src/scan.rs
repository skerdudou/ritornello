//! Marche récursive d'un répertoire : filtre d'extensions, garde contre les
//! boucles de liens symboliques, cap.

use ritornello_i18n::Catalog;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Plafond d'une liste. Protège trois choses à la fois : la charge utile JSON
/// servie à la page, l'écriture du m3u, et la liste de playback de mpv.
pub const MAX_TRACKS: usize = 2000;

/// Plafond de **visite** d'une recherche, distinct de `MAX_TRACKS`.
///
/// `MAX_TRACKS` bounded ce que la liste de playback peut contenir ; le confondre
/// avec le coût d'un parcours faisait refuser toute recherche lancée in_dir un
/// dossier de plus de 2000 pistes, avec le message de l'ajout — mesuré à la
/// racine d'un NAS. Une recherche ne remplit rien : elle n'a besoin que d'une
/// bounded qui l'empêche de tourner sans fin, et le dépassement se rapporte
/// comme « tronqué ».
///
/// Compte désormais **chaque entrée inspectée** — dossier ou fichier, audio ou
/// non — et non plus seulement les fichiers audio rencontrés : un dossier
/// plein de fichiers non-audio n'était borné par rien avant ce changement.
/// Relevé en conséquence : c'est [`SEARCH_TIMEOUT`] qui protège désormais un
/// partage lent (le coût dominant y est le `read_dir` par dossier, pas le
/// compte d'entrées) ; ce cap reste un filet pour le cas local, rapide par
/// entrée, où seul un nombre démesuré d'entrées doit être refusé.
pub const MAX_VISITS: usize = 500_000;

/// Délai maximal accordé à une recherche.
///
/// Franchement sous les 5 s du protocol d'admin, qui est **sériel** : passé
/// ce délai, les `get_data` du sondage de la page s'empilent derrière la
/// recherche en cours et expirent all — c'est le mode de panne de l'incident
/// du 2026-08-17, où la page disparaissait. La marge restante couvre la
/// résolution des chemins et la sérialisation qui suivent la walk_dir.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(3);

/// Nombre d'entrées inspectées entre deux mesures de l'échéance.
///
/// `Instant::elapsed` n'est pas gratuit : le mesurer à chaque entrée
/// ajouterait un appel système par fichier, sur le path le plus chaud de la
/// walk_dir.
const DEADLINE_STEP: usize = 64;

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
/// l'order de `read_dir` n'étant garanti par aucun système de fichiers.
///
/// La garde anti-boucle mémorise les répertoires **canonisés** déjà visités :
/// un lien symbolique pointant vers un ancêtre ferait sinon tourner la walk_dir
/// jusqu'au cap, avec un symptôme qui ressemble à une bibliothèque énorme
/// plutôt qu'à un défaut.
pub fn walk(dir: &Path, cap: usize) -> Result<Vec<PathBuf>, ScanError> {
    walk_with(dir, cap, &|_, _| {})
}

/// Même walk_dir, avec un **crochet de progress** appelé à chaque répertoire
/// visité : le nombre de pistes trouvées jusque-là, et le répertoire en cours.
///
/// Il existe parce qu'une walk_dir sur un partage SMB endormi peut durer
/// longtemps, et que le protocol admin ne push_cover rien : la page interroge, et
/// il lui faut quelque chose à montrer entre-temps. Sans lui, l'utilisateur
/// verrait un écran figé sans savoir si quelque chose avance.
pub fn walk_with(
    dir: &Path,
    cap: usize,
    progress: &dyn Fn(usize, &Path),
) -> Result<Vec<PathBuf>, ScanError> {
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    walk_dir(dir, cap, &mut out, &mut seen, progress)?;
    out.sort();
    Ok(out)
}

/// Extensions des fichiers de liste de playback.
///
/// Écartées des extensions audio : un m3u ne s'add pas à la liste, il la
/// **remplace**. Les confondre ferait add un fichier texte que mpv
/// tenterait de play.
const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8"];

pub fn is_playlist(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| PLAYLIST_EXTENSIONS.iter().any(|k| k.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Contents d'un seul niveau, chaque catégorie triée.
///
/// Une structure nommée plutôt qu'un triplet : trois `Vec<String>` anonymes
/// s'inversent au premier refactor, et l'erreur se voit seulement à l'écran.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Contents {
    pub dirs: Vec<String>,
    pub audio: Vec<String>,
    /// Fichiers de liste de playback, qui se **chargent** au lieu de s'add.
    pub playlists: Vec<String>,
}

/// Contents d'un seul niveau : sous-répertoires, fichiers audio et playlists de
/// playback. C'est ce que consomme l'arbre **paresseux** de la page, qui ne
/// demande jamais toute l'arborescence d'un coup.
pub fn list_dir(dir: &Path) -> Result<Contents, ScanError> {
    let playback =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut out = Contents::default();
    for entree in playback.flatten() {
        let path = entree.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        // Les entrées cachées ne sont pas montrées : une bibliothèque en est
        // pleine (`.DS_Store`, `@eaDir` d'un Synology) et elles n'ont rien à
        // faire in_dir un arbre de navigation musicale.
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            out.dirs.push(name);
        } else if meta.is_file() && is_audio(&path) {
            out.audio.push(name);
        } else if meta.is_file() && is_playlist(&path) {
            out.playlists.push(name);
        }
    }
    out.dirs.sort();
    out.audio.sort();
    out.playlists.sort();
    Ok(out)
}

/// Pourquoi une recherche s'est arrêtée.
///
/// Deux causes, deux conseils à donner : trop de correspondances invite à
/// préciser le pattern, un parcours interrompu invite à descendre in_dir un
/// sous-dossier. Les confondre faisait afficher « Aucun résultat » — donc « ce
/// fichier n'existe pas » — à quelqu'un dont la recherche avait simplement
/// renoncé avant d'arriver jusqu'à lui.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchEnd {
    /// Tout le dossier a été parcouru.
    Complete,
    /// Le cap de résultats est atteint : il y en avait davantage.
    TooManyResults,
    /// Le parcours a été interrompu avant d'avoir tout vu.
    Interrupted,
}

/// Cherche récursivement les fichiers audio dont le name contains `pattern`
/// (comparaison insensible à la casse).
///
/// Deux bornes, et deux raisons distinctes : `cap` limite ce qu'on **rapporte**
/// à la page, `visit_cap` ce qu'on accepte de **browse**. L'une comme
/// l'autre rend une [`SearchEnd`] distincte, jamais un refus : une liste
/// partielle annoncée comme telle est utile, un refus ne l'est pas.
///
/// Le filtre s'applique **pendant** la walk_dir : collecter d'abord tout le
/// dossier pour ne garder ensuite qu'une poignée de names était ce qui faisait
/// buter la recherche sur le cap de la liste de playback.
pub fn search(
    dir: &Path,
    pattern: &str,
    cap: usize,
    visit_cap: usize,
    timeout: Duration,
) -> Result<(Vec<PathBuf>, SearchEnd), ScanError> {
    let pattern = pattern.to_lowercase();
    if pattern.is_empty() {
        return Ok((Vec::new(), SearchEnd::Complete));
    }
    let mut out = Vec::new();
    let mut visits = 0usize;
    let mut seen = HashSet::new();
    let start = Instant::now();
    // `cap + 1` : on en search un de plus que ce qu'on rend, pour distinguer
    // « exactement cap résultats » de « il y en avait davantage ». Sans cela une
    // liste complète de cap éléments serait annoncée comme tronquée.
    let arrete = walk_searching(
        dir,
        &pattern,
        cap + 1,
        visit_cap,
        start,
        timeout,
        &mut out,
        &mut visits,
        &mut seen,
    )?;
    out.truncate(cap);
    Ok((out, arrete.unwrap_or(SearchEnd::Complete)))
}

/// Marche filtrante. Rend la cause d'un arrêt anticipé, `None` si la walk_dir a
/// couvert tout le dossier.
// Neuf paramètres : les trois derniers sont l'état de la récursion, les six
// premiers ses bornes. Les regrouper in_dir une struct n'apporterait qu'un name
// de plus à read — accepté tel quel.
#[allow(clippy::too_many_arguments)]
fn walk_searching(
    dir: &Path,
    pattern: &str,
    cap: usize,
    visit_cap: usize,
    start: Instant,
    timeout: Duration,
    out: &mut Vec<PathBuf>,
    visits: &mut usize,
    seen: &mut HashSet<PathBuf>,
) -> Result<Option<SearchEnd>, ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    // Même garde que `walk_dir` : un lien pointant vers un ancêtre ferait tourner
    // la walk_dir en produisant des chemins de plus en plus longs.
    if !seen.insert(canon) {
        return Ok(None);
    }
    let playback =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut sous_dossiers = Vec::new();
    for entree in playback {
        let Ok(entree) = entree else { continue };
        let path = entree.path();
        // Chaque entrée compte, dossier ou fichier, audio ou non : un dossier
        // plein de fichiers non-audio n'était borné par rien tant que seuls
        // les fichiers audio étaient comptés.
        *visits += 1;
        if *visits > visit_cap {
            return Ok(Some(SearchEnd::Interrupted));
        }
        // Mesurée toutes les `DEADLINE_STEP` entrées, pas à chaque entrée :
        // `Instant::elapsed` n'est pas gratuit.
        if visits.is_multiple_of(DEADLINE_STEP) && start.elapsed() >= timeout {
            return Ok(Some(SearchEnd::Interrupted));
        }
        // `metadata` et non `symlink_metadata`, comme in_dir `walk_dir` : un lien
        // vers un dossier réel doit être suivi.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            sous_dossiers.push(path);
            continue;
        }
        if !(meta.is_file() && is_audio(&path)) {
            continue;
        }
        let correspond = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_lowercase().contains(pattern));
        if correspond {
            out.push(path);
            if out.len() >= cap {
                return Ok(Some(SearchEnd::TooManyResults));
            }
        }
    }
    sous_dossiers.sort();
    for d in sous_dossiers {
        if let Some(raison) =
            walk_searching(&d, pattern, cap, visit_cap, start, timeout, out, visits, seen)?
        {
            return Ok(Some(raison));
        }
    }
    Ok(None)
}

fn walk_dir(
    dir: &Path,
    cap: usize,
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    progress: &dyn Fn(usize, &Path),
) -> Result<(), ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    if !seen.insert(canon) {
        return Ok(());
    }
    progress(out.len(), dir);
    let playback =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut sous_dossiers = Vec::new();
    for entree in playback {
        let Ok(entree) = entree else { continue };
        let path = entree.path();
        // `metadata` et non `symlink_metadata` : un lien vers un dossier réel
        // doit être suivi. C'est la boucle qu'on refuse, pas le lien. Un lien
        // cassé ou un répertoire interdit fait passer au suivant plutôt que
        // d'échouer : une bibliothèque est rarement parfaite, et refuser tout
        // l'ajout pour un fichier bancal serait disproportionné.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            sous_dossiers.push(path);
        } else if meta.is_file() && is_audio(&path) {
            if out.len() >= cap {
                return Err(ScanError::TooMany { cap });
            }
            out.push(path);
        }
    }
    sous_dossiers.sort();
    for d in sous_dossiers {
        walk_dir(&d, cap, out, seen, progress)?;
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
        for name in ["a.mp3", "b.FLAC", "c.Opus", "cover.jpg", "notes.txt", "sans-extension"] {
            fichier(dir.path(), name);
        }
        let mut names: Vec<String> = walk(dir.path(), MAX_TRACKS)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.mp3", "b.FLAC", "c.Opus"]);
    }

    #[test]
    fn un_niveau_separe_dossiers_pistes_et_listes_de_lecture() {
        // Les playlists voyagent à part parce qu'elles portent une action
        // différente : elles **remplacent** la liste en cours au lieu de s'y
        // add. Les ranger avec les pistes ferait add un fichier texte
        // que mpv tenterait de play.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Album")).unwrap();
        for name in ["a.mp3", "tout.m3u", "autre.M3U8", "cover.jpg", ".cache.m3u"] {
            fichier(dir.path(), name);
        }
        let c = list_dir(dir.path()).unwrap();
        assert_eq!(c.dirs, vec!["Album"]);
        assert_eq!(c.audio, vec!["a.mp3"]);
        // Casse indifférente, comme pour l'audio ; l'entrée cachée reste écartée.
        assert_eq!(c.playlists, vec!["autre.M3U8", "tout.m3u"]);
    }

    #[test]
    fn un_m3u_nest_pas_un_fichier_audio() {
        // Garde-fou de la séparation ci-dessus, du côté des prédicats : un
        // balayage récursif ne doit pas ramasser les playlists comme des pistes.
        assert!(is_playlist(Path::new("x/tout.m3u")));
        assert!(is_playlist(Path::new("x/tout.M3U8")));
        assert!(!is_audio(Path::new("x/tout.m3u")));
        assert!(!is_playlist(Path::new("x/piste.mp3")));
    }

    #[test]
    fn la_marche_est_recursive_et_ordonnee() {
        // L'order doit être stable : deux ajouts du même dossier produisent la
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
        // Sans garde, un lien pointant vers un ancêtre fait tourner la walk_dir
        // jusqu'au cap en produisant des chemins de plus en plus longs. Le
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
        // qui n'add rien à la liste. La cause : `search` réutilisait
        // `MAX_TRACKS`, le cap de la liste de playback, comme cap de
        // walk_dir. Une recherche trop large se tronque et le dit ; elle ne se
        // refuse pas.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        let (trouves, fin) = search(dir.path(), "mp3", 200, 3, SEARCH_TIMEOUT).expect("aucun refus attendu");
        assert_ne!(fin, SearchEnd::Complete, "un cap atteint doit se dire");
        assert!(!trouves.is_empty(), "des resultats partiels valent mieux que rien");
    }

    #[test]
    fn une_recherche_interrompue_par_le_plafond_de_visites_le_dit_comme_telle() {
        // Défaut trouvé en revue : la walk_dir rendait `Ok(true)` que le cap
        // atteint soit celui des VISITES ou celui des RÉSULTATS, et la page
        // affichait alors « Aucun résultat » — donc « ce fichier n'existe pas »
        // — pour une recherche qui avait simplement renoncé avant d'arriver
        // jusqu'à lui. Ici le cap de visits est atteint bien avant celui
        // des résultats (200) : la cause doit se distinguer.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        let (_, fin) = search(dir.path(), "mp3", 200, 3, SEARCH_TIMEOUT).unwrap();
        assert_eq!(fin, SearchEnd::Interrupted);
    }

    #[test]
    fn une_recherche_qui_depasse_le_plafond_de_resultats_le_dit_comme_telle() {
        // L'autre cause d'arrêt : ici le cap de visits est large, seul
        // celui des résultats est en cause.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("miles{i}.mp3"));
        }
        let (trouves, fin) = search(dir.path(), "miles", 3, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        assert_eq!(fin, SearchEnd::TooManyResults);
        assert_eq!(trouves.len(), 3);
    }

    #[test]
    fn une_recherche_qui_a_tout_parcouru_est_complete() {
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "A/miles.flac");
        let (_, fin) = search(dir.path(), "miles", 200, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        assert_eq!(fin, SearchEnd::Complete);
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
        let (trouves, fin) = search(dir.path(), "miles", 3, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        assert_eq!(trouves.len(), 3);
        assert_eq!(fin, SearchEnd::Complete);
    }

    #[test]
    fn une_recherche_avec_plus_de_cap_resultats_et_un_plafond_de_visites_large_est_tronquee() {
        // L'autre régime non couvert : le cap de visits n'entre pas en
        // line de compte, seul celui des résultats doit se déclencher.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            fichier(dir.path(), &format!("miles{i}.mp3"));
        }
        let (trouves, fin) = search(dir.path(), "miles", 3, 1_000_000, SEARCH_TIMEOUT).unwrap();
        assert_eq!(trouves.len(), 3);
        assert_eq!(fin, SearchEnd::TooManyResults);
    }

    #[test]
    fn le_delai_de_recherche_est_franchement_sous_le_plafond_du_protocole() {
        // Le protocol admin abandonne une requête après 5 s ; il faut de la
        // marge pour la résolution des chemins et la sérialisation qui suivent
        // la walk_dir, sans quoi le délai lui-même dépasserait le cap du
        // cœur.
        assert!(SEARCH_TIMEOUT < std::time::Duration::from_secs(5));
    }

    #[test]
    fn une_recherche_qui_depasse_son_delai_est_interrompue_sans_attendre() {
        // Le compte de visits ne protège pas un partage lent : le coût y est
        // dominant par `read_dir`, pas par entrée. Un délai nul permet
        // d'observer l'interruption sans faire dépendre le test d'une horloge.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(DEADLINE_STEP * 2) {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        let (_, fin) = search(dir.path(), "mp3", 200, MAX_VISITS, Duration::ZERO).unwrap();
        assert_eq!(fin, SearchEnd::Interrupted);
    }

    #[test]
    fn un_dossier_plein_de_fichiers_non_audio_est_borne_par_le_plafond_de_visites() {
        // Défaut corrigé : `visits` ne comptait que les fichiers AUDIO, donc un
        // dossier plein de fichiers non-audio n'était borné par rien — la
        // walk_dir pouvait inspecter un nombre de fichiers arbitraire sans jamais
        // s'arrêter sur ce cap.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.txt"));
        }
        let (trouves, fin) = search(dir.path(), "txt", 200, 3, SEARCH_TIMEOUT).unwrap();
        assert_eq!(fin, SearchEnd::Interrupted);
        assert!(trouves.is_empty(), "aucun fichier non-audio ne doit etre rapporte");
    }

    // Les deux plafonds ne mesurent pas la même chose : `MAX_TRACKS` bounded ce
    // qu'on peut AJOUTER, `MAX_VISITS` ce qu'on peut PARCOURIR en cherchant.
    // Les confondre est exactement le défaut corrigé ici. Vérifié à la
    // compilation : un test sur deux constantes ne peut pas échouer à
    // l'exécution, clippy le refuse à raison.
    const _: () = assert!(MAX_VISITS > MAX_TRACKS);

    #[test]
    fn une_recherche_rend_les_correspondances_et_seulement_elles() {
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "A/miles.flac");
        fichier(dir.path(), "A/autre.mp3");
        fichier(dir.path(), "B/sous/MILES live.mp3");
        let (trouves, fin) = search(dir.path(), "miles", 200, MAX_VISITS, SEARCH_TIMEOUT).unwrap();
        let mut relatifs: Vec<String> = trouves
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        relatifs.sort();
        // Insensible à la casse, et sur le name de fichier seul.
        assert_eq!(relatifs, vec!["A/miles.flac", "B/sous/MILES live.mp3"]);
        assert_eq!(fin, SearchEnd::Complete, "trois fichiers ne remplissent aucun cap");
    }

    #[test]
    fn chaque_refus_resout_contre_le_catalogue_embarque() {
        let catalog = Catalog::load("files", "en", Path::new("/inexistant"), crate::FILES_EN);
        for m in [
            ScanError::TooMany { cap: 2000 }.message(&catalog),
            ScanError::Io { path: "/mnt/ritornello/nas".into() }.message(&catalog),
        ] {
            assert!(m.contains(' '), "message reduit a une key brute : {m:?}");
        }
        let bounded = ScanError::TooMany { cap: 2000 }.message(&catalog);
        assert!(bounded.contains("2000"), "bounded non interpolee : {bounded:?}");
        assert!(!bounded.contains("{cap}"), "jeton laisse tel quel : {bounded:?}");
    }
}
