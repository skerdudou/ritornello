//! Lecture et écriture de playlists m3u.
//!
//! Deux objets distincts passent par ici, et les confondre serait une erreur :
//! la **liste utilisateur** (éditée, enregistrée, rechargeable, à chemins
//! relatifs quand c'est possible) et la **liste donnée à mpv** (générée, à
//! chemins absolus, jamais montrée).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub title: Option<String>,
    pub duration_s: Option<u32>,
}

impl Entry {
    /// Nom affichable : le titre `#EXTINF` s'il existe, sinon le name du fichier
    /// sans extension.
    ///
    /// C'est ce que la Source déclare en `preset_name`, de sorte que l'écran ne
    /// soit **jamais muet** même sans aucune métadonnée : les tags ne font
    /// qu'enrichir par-dessus.
    pub fn display_name(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// Entrées qu'aucune règle n'a su résoudre. **Rapportées**, jamais
    /// supprimées en silence : une liste qui rétrécit sans rien dire est un
    /// défaut qu'on met des mois à attribuer.
    pub unresolved: Vec<String>,
}

/// Résout une entrée brute.
///
/// Trois règles, in_dir cet order. Un m3u écrit par le NAS porte souvent des
/// chemins qui n'ont de sens que chez lui (`Z:\Musique\…`, `/volume1/music/…`,
/// un path UNC) : la troisième règle est là pour les rattraper plutôt que de
/// jeter l'entrée.
fn resolve(brut: &str, m3u_dir: &Path, root: &Path) -> Option<PathBuf> {
    let brut = brut.trim();
    if brut.is_empty() {
        return None;
    }
    let normalise = brut.replace('\\', "/");

    // 1. relative au répertoire du m3u — le cas normal, et celui qu'on écrit.
    let rel = m3u_dir.join(&normalise);
    if rel.is_file() {
        return Some(rel);
    }

    // 2. absolue telle quelle, si elle désigne quelque chose ici.
    let abs = Path::new(&normalise);
    if abs.is_absolute() && abs.is_file() {
        return Some(abs.to_path_buf());
    }

    // 3. path d'un autre système : on retire un préfixe de player (`Z:`),
    //    puis on essaie les suffixes successifs sous la racine, du plus long au
    //    plus court — `Musique/Album/02.mp3`, puis `Album/02.mp3`, puis
    //    `02.mp3`. Le premier qui existe gagne.
    let sans_lecteur = match normalise.find(':') {
        Some(i) if i <= 2 => &normalise[i + 1..],
        _ => normalise.as_str(),
    };
    let segments: Vec<&str> = sans_lecteur.split('/').filter(|s| !s.is_empty()).collect();
    for depart in 0..segments.len() {
        let candidat = root.join(segments[depart..].join("/"));
        if candidat.is_file() {
            return Some(candidat);
        }
    }
    None
}

/// Analyse un m3u. `m3u_dir` est le répertoire du fichier lu, `root` la racine
/// sous laquelle rattraper les chemins étrangers.
pub fn parse(text: &str, m3u_dir: &Path, root: &Path) -> Parsed {
    let mut out = Parsed::default();
    let mut en_attente: Option<(Option<u32>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(reste) = line.strip_prefix("#EXTINF:") {
            en_attente = Some(match reste.split_once(',') {
                Some((d, t)) => (
                    // `-1` est la convention « durée inconnue » : elle ne doit
                    // pas devenir une durée.
                    d.trim().parse::<i64>().ok().filter(|n| *n > 0).map(|n| n as u32),
                    (!t.trim().is_empty()).then(|| t.trim().to_string()),
                ),
                None => (None, None),
            });
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (duration, titre) = en_attente.take().unwrap_or((None, None));
        match resolve(line, m3u_dir, root) {
            Some(path) => out.entries.push(Entry { path, title: titre, duration_s: duration }),
            None => out.unresolved.push(line.to_string()),
        }
    }
    out
}

/// Rend un m3u.
///
/// Avec une `base`, les chemins sont **relatifs** à elle : c'est ce qui rend la
/// liste relisible par un autre player et survivante à un changement de point
/// de montage. Sans base, ils sont absolus — la forme de la liste destinée à
/// mpv, qui ne doit dépendre d'aucun répertoire courant.
pub fn render(entries: &[Entry], base: Option<&Path>) -> String {
    let mut s = String::from("#EXTM3U\n");
    for e in entries {
        let duration = e.duration_s.map(|d| d.to_string()).unwrap_or_else(|| "-1".into());
        s.push_str(&format!("#EXTINF:{duration},{}\n", e.display_name()));
        let path = base
            .and_then(|b| e.path.strip_prefix(b).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| e.path.to_string_lossy().into_owned());
        s.push_str(&path);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fichier(dir: &Path, rel: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
        p
    }

    #[test]
    fn un_m3u_relatif_se_resout_contre_le_repertoire_du_fichier() {
        let dir = tempfile::tempdir().unwrap();
        let cible = fichier(dir.path(), "Album/01.mp3");
        let texte = "#EXTM3U\n#EXTINF:245,Miles Davis - So What\nAlbum/01.mp3\n";
        let p = parse(texte, dir.path(), dir.path());
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].path, cible);
        assert_eq!(p.entries[0].title.as_deref(), Some("Miles Davis - So What"));
        assert_eq!(p.entries[0].duration_s, Some(245));
        assert!(p.unresolved.is_empty());
    }

    #[test]
    fn un_chemin_windows_ecrit_par_le_nas_se_rattrape_sous_la_racine() {
        // Un m3u produit par le NAS porte des chemins qui n'ont de sens que
        // chez lui. On retire le préfixe de player et on essaie les suffixes
        // successifs sous la racine, plutôt que de jeter l'entrée.
        let dir = tempfile::tempdir().unwrap();
        let cible = fichier(dir.path(), "Musique/Album/02.mp3");
        let p = parse("#EXTM3U\nZ:\\Musique\\Album\\02.mp3\n", dir.path(), dir.path());
        assert_eq!(p.entries.len(), 1, "non resolu : {:?}", p.unresolved);
        assert_eq!(p.entries[0].path, cible);
    }

    #[test]
    fn un_chemin_absolu_etranger_se_rattrape_par_son_suffixe() {
        // Le cas d'un Synology : /volume1/music/... n'existe pas ici, mais
        // « Album/03.mp3 » est bien sous la racine.
        let dir = tempfile::tempdir().unwrap();
        let cible = fichier(dir.path(), "Album/03.mp3");
        let p = parse("#EXTM3U\n/volume1/music/Album/03.mp3\n", dir.path(), dir.path());
        assert_eq!(p.entries.len(), 1, "non resolu : {:?}", p.unresolved);
        assert_eq!(p.entries[0].path, cible);
    }

    #[test]
    fn une_entree_introuvable_est_rapportee_et_non_jetee() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse("#EXTM3U\n/volume1/music/absent.mp3\n", dir.path(), dir.path());
        assert!(p.entries.is_empty());
        assert_eq!(p.unresolved, vec!["/volume1/music/absent.mp3".to_string()]);
    }

    #[test]
    fn les_commentaires_les_lignes_vides_et_lextinf_orphelin_sont_traites() {
        let dir = tempfile::tempdir().unwrap();
        let p = parse("#EXTM3U\n\n# un commentaire\n#EXTINF:12,Orphelin\n\n", dir.path(), dir.path());
        assert!(p.entries.is_empty());
        assert!(p.unresolved.is_empty());
    }

    #[test]
    fn une_duree_inconnue_ne_devient_pas_une_duree() {
        // `-1` est la convention m3u pour « je ne sais pas » : la prendre pour
        // une durée afficherait « -1 s » quelque part.
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "a.mp3");
        let p = parse("#EXTM3U\n#EXTINF:-1,Sans duration\na.mp3\n", dir.path(), dir.path());
        assert_eq!(p.entries[0].duration_s, None);
        assert_eq!(p.entries[0].title.as_deref(), Some("Sans duration"));
    }

    #[test]
    fn le_rendu_est_relatif_quand_une_base_est_donnee() {
        let base = Path::new("/mnt/ritornello/nas");
        let entries = vec![Entry {
            path: base.join("Album/01.mp3"),
            title: Some("So What".into()),
            duration_s: Some(245),
        }];
        assert_eq!(render(&entries, Some(base)), "#EXTM3U\n#EXTINF:245,So What\nAlbum/01.mp3\n");
    }

    #[test]
    fn le_rendu_est_absolu_sans_base_et_nomme_par_defaut_le_fichier() {
        let entries = vec![Entry {
            path: "/mnt/ritornello/nas/Album/01.mp3".into(),
            title: None,
            duration_s: None,
        }];
        assert_eq!(
            render(&entries, None),
            "#EXTM3U\n#EXTINF:-1,01\n/mnt/ritornello/nas/Album/01.mp3\n"
        );
    }

    #[test]
    fn ecrire_puis_relire_conserve_titres_et_durees() {
        // Le vrai aller-retour : ce qu'on enregistre doit revenir identique.
        let dir = tempfile::tempdir().unwrap();
        let a = fichier(dir.path(), "Album/01.mp3");
        let b = fichier(dir.path(), "Album/02.mp3");
        let entries = vec![
            Entry { path: a, title: Some("So What".into()), duration_s: Some(545) },
            Entry { path: b, title: Some("Blue in Green".into()), duration_s: None },
        ];
        let texte = render(&entries, Some(dir.path()));
        let relu = parse(&texte, dir.path(), dir.path());
        assert_eq!(relu.entries, entries);
        assert!(relu.unresolved.is_empty());
    }
}
