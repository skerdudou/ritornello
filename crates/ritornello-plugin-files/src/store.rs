//! Listes enregistrées : in_dir le stockage interne, ou sur une racine.
//!
//! Le format est le m3u, au même titre que ce qu'on charge : une liste déposée
//! sur le NAS doit y être relisible par n'importe quel autre player, et donc
//! porter des chemins **relatifs** à la racine où elle est posée.
//!
//! Deux asymétries valent d'être dites, parce qu'elles sont volontaires :
//! enregistrer exige `writable = true` alors que charger ne demande rien (une
//! racine en playback seule est parfaitement légitime à la playback) ; et une
//! racine unreachable est ignorée par `list` sans jamais lever d'erreur, faute
//! de quoi un NAS endormi empêcherait de voir ses playlists internes.

use crate::m3u::{self, Entry};
use crate::roots::Roots;
use ritornello_i18n::Catalog;
use std::path::{Path, PathBuf};

/// Où vit une liste enregistrée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// Le répertoire d'état du plugin, sur l'appareil.
    Internal,
    /// Une racine déclarée, désignée par son name.
    Root(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    pub location: Location,
}

/// Erreur typée : le texte utilisateur est produit à la frontière HTTP via
/// `message(&Catalog)`. `Display` fournit une version anglaise pour les
/// logs internes, hors périmètre i18n.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    BadPlaylistName { name: String },
    ReadOnlyRoot { root: String },
    UnknownRoot { name: String },
    Io { path: String },
}

/// Un name de liste devient un **name de fichier**, écrit soit in_dir `/var/lib`,
/// soit **sur le partage réseau**. Tout ce qui pourrait traverser est refusé :
/// pas de séparateur (in_dir les deux sens, un m3u venu de Windows en portant),
/// pas de name réservé, pas de point initial qui cacherait la liste, pas
/// d'octet nul qui tronquerait une chaîne C côté noyau.
///
/// La bounded de longueur n'est pas cosmétique : bien des systèmes de fichiers
/// plafonnent un composant à 255 bytes, et le name reçoit encore un suffixe.
fn valid_playlist_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// Répertoire de destination d'une **écriture**. Le montage est `ro` par
/// défaut : refuser ici par une phrase vaut mieux que de laisser remonter une
/// erreur d'entrée-sortie du noyau, que personne ne saurait attribuer.
fn writable_dir(
    dest: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<PathBuf, StoreError> {
    match dest {
        Location::Internal => Ok(internal_dir.to_path_buf()),
        Location::Root(name) => {
            let r =
                roots.by_name(name).ok_or_else(|| StoreError::UnknownRoot { name: name.clone() })?;
            if !r.writable {
                return Err(StoreError::ReadOnlyRoot { root: name.clone() });
            }
            Ok(r.base_dir())
        }
    }
}

/// Répertoire de **playback**. Aucune vérification d'écriture : c'est tout
/// l'objet de la distinction avec `writable_dir`.
fn readable_dir(
    from: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<PathBuf, StoreError> {
    match from {
        Location::Internal => Ok(internal_dir.to_path_buf()),
        Location::Root(name) => roots
            .by_name(name)
            .map(|r| r.base_dir())
            .ok_or_else(|| StoreError::UnknownRoot { name: name.clone() }),
    }
}

/// Écriture atomique : un fichier temporaire, puis `rename`. Un enregistrement
/// interrompu ne doit jamais laisser derrière lui une liste tronquée à la
/// place de la précédente.
fn write_atomically(fichier: &Path, tmp: &Path, texte: &str) -> std::io::Result<()> {
    std::fs::write(tmp, texte)?;
    std::fs::rename(tmp, fichier)
}

pub fn save(
    entries: &[Entry],
    name: &str,
    dest: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<(), StoreError> {
    if !valid_playlist_name(name) {
        return Err(StoreError::BadPlaylistName { name: name.to_string() });
    }
    let dir = writable_dir(dest, internal_dir, roots)?;
    // Le répertoire interne, on le crée : au premier enregistrement il n'existe
    // pas encore. Celui d'une racine, **jamais** : un partage non monté a un
    // point de montage clear, et y créer l'arborescence écrirait sur le disque
    // local une liste qui disparaîtrait au montage suivant.
    if matches!(dest, Location::Internal) {
        std::fs::create_dir_all(&dir)
            .map_err(|_| StoreError::Io { path: dir.display().to_string() })?;
    }
    // Chemins relatifs quand la destination est une racine : c'est ce qui rend
    // la liste relisible ailleurs et survivante à un changement de point de
    // montage. En interne, une base n'aurait pas de sens — les pistes ne sont
    // pas sous le répertoire d'état : chemins absolus.
    let base = matches!(dest, Location::Root(_)).then(|| dir.clone());
    let texte = m3u::render(entries, base.as_deref());
    let fichier = dir.join(format!("{name}.m3u"));
    let tmp = dir.join(format!("{name}.m3u.tmp"));
    write_atomically(&fichier, &tmp, &texte).map_err(|_| {
        // Un temporaire abandonné sur le partage serait visible de all et ne
        // servirait plus à rien.
        let _ = std::fs::remove_file(&tmp);
        StoreError::Io { path: fichier.display().to_string() }
    })
}

pub fn load(
    name: &str,
    from: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<m3u::Parsed, StoreError> {
    if !valid_playlist_name(name) {
        return Err(StoreError::BadPlaylistName { name: name.to_string() });
    }
    let dir = readable_dir(from, internal_dir, roots)?;
    let fichier = dir.join(format!("{name}.m3u"));
    let texte = std::fs::read_to_string(&fichier)
        .map_err(|_| StoreError::Io { path: fichier.display().to_string() })?;
    Ok(m3u::parse(&texte, &dir, &dir))
}

/// Toutes les playlists visibles, l'interne et les racines confondues.
///
/// Une racine unreachable est **ignorée sans erreur** : un NAS endormi ne doit
/// pas empêcher de voir ses playlists internes. Chaque répertoire est rendition trié,
/// l'order de `read_dir` n'étant garanti par aucun système de fichiers — sans
/// quoi la page réordonnerait ses playlists d'un rafraîchissement à l'autre.
pub fn list(internal_dir: &Path, roots: &Roots) -> Vec<Saved> {
    let mut out = in_dir(internal_dir, Location::Internal);
    for r in &roots.root {
        out.extend(in_dir(&r.base_dir(), Location::Root(r.name.clone())));
    }
    out
}

/// Les playlists d'**un seul** répertoire.
///
/// Séparée de `list` pour que l'appelant puisse borner chaque répertoire
/// individuellement : `read_dir` sur un partage en reconnexion ne rend pas la
/// main, et la moitié Admin sert ses requêtes en série. Voir `health`.
pub fn in_dir(dir: &Path, loc: Location) -> Vec<Saved> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        let m3u = p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x.eq_ignore_ascii_case("m3u"))
            .unwrap_or(false);
        if m3u {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names.into_iter().map(|name| Saved { name, location: loc.clone() }).collect()
}

impl StoreError {
    /// Message localisé remonté à l'utilisateur (corps du refus HTTP).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            StoreError::BadPlaylistName { name } => {
                catalog.get("bad_playlist_name").replace("{name}", name)
            }
            StoreError::ReadOnlyRoot { root } => {
                catalog.get("read_only_root").replace("{name}", root)
            }
            StoreError::UnknownRoot { name } => catalog.get("unknown_root").replace("{name}", name),
            StoreError::Io { path } => catalog.get("store_io_error").replace("{path}", path),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::BadPlaylistName { name } => write!(f, "invalid playlist name: {name}"),
            StoreError::ReadOnlyRoot { root } => write!(f, "root mounted read-only: {root}"),
            StoreError::UnknownRoot { name } => write!(f, "unknown root: {name}"),
            StoreError::Io { path } => write!(f, "cannot write or read {path}"),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roots::{Root, RootKind};
    use tempfile::TempDir;

    /// Les racines de test sont bâties **in_dir un `tempdir`**, donc en
    /// `RootKind::Local` : une racine `Smb` aurait pour `base_dir()`
    /// `/mnt/ritornello/<name>`, où la suite de tests ne peut pas écrire. Le
    /// drapeau `writable` étant vérifié quel que soit le kind, la règle reste
    /// éprouvable sans le moindre montage.
    fn decor_avec(writable: bool) -> (TempDir, Roots) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("nas");
        std::fs::create_dir_all(&base).unwrap();
        let roots = Roots {
            root: vec![Root {
                name: "nas".into(),
                kind: RootKind::Local,
                path: Some(base.to_string_lossy().into_owned()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable,
            }],
        };
        (dir, roots)
    }

    fn decor() -> (TempDir, Roots) {
        decor_avec(false)
    }

    fn decor_inscriptible() -> (TempDir, Roots) {
        decor_avec(true)
    }

    fn trois_fichiers(dir: &TempDir) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for name in ["Musique/01.mp3", "Musique/02.mp3", "Musique/03.mp3"] {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"").unwrap();
            out.push(p);
        }
        out
    }

    #[test]
    fn un_nom_de_liste_qui_traverse_est_refuse() {
        // Le name devient un name de fichier, écrit soit in_dir /var/lib, soit sur
        // le partage : « ../../etc/cron.d/x » ne doit jamais atteindre le
        // disque. La contre-barre compte autant que la barre, un name saisi
        // depuis un poste Windows en portant ; le point initial cacherait la
        // liste ; l'octet nul tronquerait une chaîne C côté noyau.
        let (dir, roots) = decor();
        let trop_long = "x".repeat(65);
        let mauvais =
            ["../evasion", "a/b", "a\\b", "", ".", "..", ".cache", "x\0y", trop_long.as_str()];
        for m in mauvais {
            assert!(
                matches!(
                    save(&[], m, &Location::Internal, dir.path(), &roots),
                    Err(StoreError::BadPlaylistName { .. })
                ),
                "accepte a tort a l'enregistrement : {m:?}"
            );
            // Le chargement valide le name lui aussi : il construit le même
            // path, et le refuser d'un seul côté laisserait la traversée
            // ouverte en playback.
            assert!(
                matches!(
                    load(m, &Location::Internal, dir.path(), &roots),
                    Err(StoreError::BadPlaylistName { .. })
                ),
                "accepte a tort a la playback : {m:?}"
            );
        }
        // Et un name ordinaire, lui, passe — la règle ne doit pas être si
        // stricte qu'elle interdise d'enregistrer.
        assert!(save(&[], "Jazz du dimanche", &Location::Internal, dir.path(), &roots).is_ok());
    }

    #[test]
    fn enregistrer_sur_une_racine_en_lecture_seule_est_refuse_avec_une_phrase() {
        // Le montage est `ro` par défaut : il faut le dire clairement plutôt
        // que de laisser remonter une erreur d'entrée-sortie du noyau, qui ne
        // désignerait ni la racine ni le remède.
        let (dir, roots) = decor(); // « nas » est writable = false
        let err = save(&[], "Jazz", &Location::Root("nas".into()), dir.path(), &roots).unwrap_err();
        assert!(matches!(err, StoreError::ReadOnlyRoot { .. }), "{err:?}");
        assert!(!dir.path().join("nas/Jazz.m3u").exists(), "ecrit malgre le refus");
    }

    #[test]
    fn charger_depuis_une_racine_en_lecture_seule_reste_permis() {
        // L'asymétrie est le cœur de la règle : read ne demande aucune
        // écriture, et le cas courant est justement un partage monté `ro`.
        let (dir, roots) = decor(); // writable = false
        let base = dir.path().join("nas");
        std::fs::write(base.join("Album.m3u"), "#EXTM3U\n#EXTINF:-1,So What\nAlbum/01.mp3\n")
            .unwrap();
        std::fs::create_dir_all(base.join("Album")).unwrap();
        std::fs::write(base.join("Album/01.mp3"), b"").unwrap();
        let relu = load("Album", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        assert_eq!(relu.entries.len(), 1, "non resolu : {:?}", relu.unresolved);
        assert_eq!(relu.entries[0].path, base.join("Album/01.mp3"));
    }

    #[test]
    fn une_liste_enregistree_en_interne_se_recharge_a_l_identique() {
        let (dir, roots) = decor();
        let fichiers = trois_fichiers(&dir);
        let entries: Vec<Entry> = fichiers
            .iter()
            .map(|p| Entry { path: p.clone(), title: None, duration_s: None })
            .collect();
        save(&entries, "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        let relu = load("Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        assert_eq!(relu.entries.len(), 3);
        assert!(relu.unresolved.is_empty());
        assert_eq!(relu.entries[0].path, fichiers[0]);
    }

    #[test]
    fn une_liste_enregistree_sur_une_racine_porte_des_chemins_relatifs() {
        // C'est ce qui la rend relisible par un autre player et survivante à
        // un changement de point de montage.
        let (dir, roots) = decor_inscriptible();
        let base = roots.by_name("nas").unwrap().base_dir();
        let entries =
            vec![Entry { path: base.join("Album/01.mp3"), title: None, duration_s: None }];
        save(&entries, "Jazz", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        let texte = std::fs::read_to_string(base.join("Jazz.m3u")).unwrap();
        assert!(texte.contains("Album/01.mp3"), "{texte}");
        assert!(!texte.contains(base.to_str().unwrap()), "path absolu ecrit : {texte}");
    }

    #[test]
    fn lister_montre_l_interne_et_les_racines_ensemble() {
        let (dir, roots) = decor_inscriptible();
        save(&[], "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        save(&[], "Rock", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        let listees = list(dir.path(), &roots);
        let mut names: Vec<String> = listees.iter().map(|s| s.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["Jazz", "Rock"]);
        // Et chacune sait d'où elle vient : sans quoi recharger « Rock »
        // irait chercher in_dir le stockage interne.
        assert_eq!(
            listees.iter().find(|s| s.name == "Rock").unwrap().location,
            Location::Root("nas".into())
        );
    }

    #[test]
    fn une_racine_injoignable_n_empeche_pas_de_voir_les_listes_internes() {
        // Un NAS endormi rend son point de montage illisible. Si `list`
        // échouait pour autant, la page n'afficherait plus rien du tout.
        let (dir, mut roots) = decor_inscriptible();
        save(&[], "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
        roots.root[0].path = Some("/inexistant/nulle-part".into());
        let names: Vec<String> = list(dir.path(), &roots).into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["Jazz"]);
    }

    #[test]
    fn l_ecriture_ne_laisse_aucun_temporaire_derriere_elle() {
        // Le `.tmp` du rename atomique ne doit pas rester visible sur le
        // partage, ni se faire prendre pour une liste enregistrée.
        let (dir, roots) = decor_inscriptible();
        save(&[], "Jazz", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
        let restants: Vec<String> = std::fs::read_dir(dir.path().join("nas"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(restants, vec!["Jazz.m3u".to_string()], "{restants:?}");
    }

    #[test]
    fn une_racine_inconnue_est_nommee_plutot_que_devinee() {
        // Une racine supprimée de la configuration alors qu'une liste la
        // désigne encore : le refus doit dire laquelle, des deux côtés.
        let (dir, roots) = decor_inscriptible();
        let absente = Location::Root("absente".into());
        assert!(matches!(
            save(&[], "Jazz", &absente, dir.path(), &roots),
            Err(StoreError::UnknownRoot { .. })
        ));
        assert!(matches!(
            load("Jazz", &absente, dir.path(), &roots),
            Err(StoreError::UnknownRoot { .. })
        ));
    }

    #[test]
    fn charger_une_liste_absente_echoue_en_nommant_le_fichier() {
        // Sans le path in_dir le refus, « impossible de read » n'aide
        // personne à comprendre où le plugin est allé chercher.
        let (dir, roots) = decor();
        let err = load("Jazz", &Location::Internal, dir.path(), &roots).unwrap_err();
        match err {
            StoreError::Io { path } => assert!(path.ends_with("Jazz.m3u"), "{path}"),
            autre => panic!("attendu Io, obtenu {autre:?}"),
        }
    }

    #[test]
    fn chaque_refus_de_store_resout_contre_le_catalogue_embarque() {
        // `Catalog::get` rend la clé quand il ne la trouve pas : sans ce test,
        // une faute de frappe afficherait « read_only_root » à l'écran sans
        // que rien ne bronche. On résout donc contre le sources_catalog réellement
        // embarqué, et on refuse un message réduit à sa propre clé.
        let catalog =
            Catalog::load("files", "en", std::path::Path::new("/inexistant"), crate::FILES_EN);
        let messages = [
            StoreError::BadPlaylistName { name: "../x".into() }.message(&catalog),
            StoreError::ReadOnlyRoot { root: "nas".into() }.message(&catalog),
            StoreError::UnknownRoot { name: "absent".into() }.message(&catalog),
            StoreError::Io { path: "/x".into() }.message(&catalog),
        ];
        for m in &messages {
            assert!(m.contains(' '), "message reduit a une key brute : {m:?}");
        }
        // Et l'interpolation aboutit : pas de jeton laissé tel quel.
        let bounded = StoreError::ReadOnlyRoot { root: "nas".into() }.message(&catalog);
        assert!(bounded.contains("nas"), "le refus doit nommer la racine : {bounded:?}");
        assert!(!bounded.contains("{name}"), "jeton laisse tel quel : {bounded:?}");
    }
}
