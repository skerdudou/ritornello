//! Les racines : les répertoires où le plugin a le droit de regarder.
//!
//! Un disque USB, un dossier de l'appareil et un partage SMB sont la même chose
//! pour tout le reste du plugin ; le montage n'est qu'un détail du kind `Smb`.
//! C'est ce qui rend le parcours des fichiers locaux quasi gratuit.
//!
//! La validation de ce module est **lue par un binaire racine**. Elle est donc
//! stricte, et refuse par principe tout ce dont elle ne sait pas prouver
//! l'innocuité.

use ritornello_i18n::Catalog;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Racine des points de montage. Constante, **jamais lue depuis la
/// configuration** : un point de montage libre serait un path à valider, et
/// c'est root qui l'emploierait.
pub const MOUNT_ROOT: &str = "/mnt/ritornello";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Root {
    pub name: String,
    pub kind: RootKind,
    /// Genre `Local` uniquement : path absolu du répertoire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub share: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub domain: String,
    /// Retire `ro` des options de montage. Faux par défaut : enregistrer une
    /// liste sur le partage est un choix explicite, pas un état de fait.
    #[serde(default)]
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    Local,
    Smb,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Roots {
    #[serde(default)]
    pub root: Vec<Root>,
}

/// Erreur de validation typée : le texte utilisateur est produit à la frontière
/// via `message(&Catalog)`. `Display` fournit une version anglaise pour les
/// logs internes, hors périmètre i18n.
#[derive(Debug, Clone, PartialEq)]
pub enum RootError {
    BadName { name: String },
    BadHost { host: String },
    BadShare { share: String },
    BadSubpath { subpath: String },
    DuplicateName { name: String },
    RelativeLocalPath { path: String },
}

/// Grammaire d'un name de racine : il devient un **composant de path** et un
/// **name de fichier d'identifiants**. Tout ce qui sort de cet alphabet ouvrirait
/// une traversée de répertoire du côté privilégié.
fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 {
        return false;
    }
    let mut chars = name.chars();
    let premier = chars.next().unwrap_or(' ');
    if !premier.is_ascii_lowercase() && !premier.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Un champ qui atterrit in_dir une line d'options `mount.cifs`.
///
/// **La virgule est l'injection à craindre** : les options de `mount.cifs` sont
/// séparées par des virgules, si bien qu'un hôte « nas,uid=0 » ajouterait une
/// option à la line exécutée par root. L'espace casse l'analyse, `..` remonte
/// l'arborescence, et l'octet nul tronque une chaîne C.
pub fn field_on(valeur: &str) -> bool {
    !valeur.is_empty()
        && !valeur.contains(',')
        && !valeur.chars().any(char::is_whitespace)
        && !valeur.contains("..")
        && !valeur.contains('\0')
}

/// Grammaire d'un **sous-path** parcouru sous un point de montage.
///
/// Distincte de `field_on`, et c'est délibéré. `field_on` refuse la virgule
/// et l'espace parce que ses valeurs atterrissent in_dir la line d'options de
/// `mount.cifs`, qui les sépare par des virgules. **Un sous-path n'y entre
/// jamais** : `mount_command` ne pose que l'hôte, le partage et
/// `mount_point()`, lequel ignore le sous-path.
///
/// Leur appliquer la même règle rendait « Ma Musique » indéclarable pour une
/// raison qui ne la concerne pas. Le défaut se voyait peu tant qu'on saisissait
/// le sous-path à la main ; il deviendrait constant avec un assistant qui
/// propose de choisir n'importe quel dossier d'un NAS.
fn subpath_on(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('/')
        && !s.contains('\0')
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

impl Roots {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let roots: Roots = toml::from_str(&text)?;
        roots.validate()?;
        Ok(roots)
    }

    pub fn validate(&self) -> Result<(), RootError> {
        let mut seen: Vec<&str> = Vec::new();
        for r in &self.root {
            if !valid_name(&r.name) {
                return Err(RootError::BadName { name: r.name.clone() });
            }
            if seen.contains(&r.name.as_str()) {
                return Err(RootError::DuplicateName { name: r.name.clone() });
            }
            seen.push(&r.name);
            match r.kind {
                RootKind::Local => {
                    let p = r.path.clone().unwrap_or_default();
                    if !Path::new(&p).is_absolute() {
                        return Err(RootError::RelativeLocalPath { path: p });
                    }
                }
                RootKind::Smb => {
                    if !field_on(&r.host) {
                        return Err(RootError::BadHost { host: r.host.clone() });
                    }
                    if !field_on(&r.share) {
                        return Err(RootError::BadShare { share: r.share.clone() });
                    }
                    if let Some(s) = &r.subpath {
                        if !subpath_on(s) {
                            return Err(RootError::BadSubpath { subpath: s.clone() });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn by_name(&self, name: &str) -> Option<&Root> {
        self.root.iter().find(|r| r.name == name)
    }
}

impl Root {
    /// Répertoire réellement parcouru. Pour un partage, le point de montage
    /// **imposé**, éventuellement suivi du sous-path déclaré.
    pub fn base_dir(&self) -> PathBuf {
        match self.kind {
            RootKind::Local => PathBuf::from(self.path.clone().unwrap_or_default()),
            RootKind::Smb => {
                let mut p = PathBuf::from(MOUNT_ROOT).join(&self.name);
                if let Some(s) = &self.subpath {
                    p = p.join(s);
                }
                p
            }
        }
    }

    /// Point de montage, **sans** le sous-path : c'est le partage entier qui
    /// est monté, le sous-path n'étant qu'un endroit où regarder dedans.
    pub fn mount_point(&self) -> PathBuf {
        PathBuf::from(MOUNT_ROOT).join(&self.name)
    }

    /// File d'identifiants consommé par `mount.cifs`.
    pub fn credentials_path(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.cred", self.name))
    }
}

impl RootError {
    /// Message localisé remonté à l'utilisateur (corps du refus côté admin).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            RootError::BadName { name } => catalog.get("bad_root_name").replace("{name}", name),
            RootError::BadHost { host } => catalog.get("bad_host").replace("{host}", host),
            RootError::BadShare { share } => catalog.get("bad_share").replace("{share}", share),
            RootError::BadSubpath { subpath } => {
                catalog.get("bad_subpath").replace("{path}", subpath)
            }
            RootError::DuplicateName { name } => {
                catalog.get("duplicate_root").replace("{name}", name)
            }
            RootError::RelativeLocalPath { path } => {
                catalog.get("relative_local_path").replace("{path}", path)
            }
        }
    }
}

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RootError::BadName { name } => write!(f, "invalid root name: {name}"),
            RootError::BadHost { host } => write!(f, "invalid host: {host}"),
            RootError::BadShare { share } => write!(f, "invalid share: {share}"),
            RootError::BadSubpath { subpath } => write!(f, "invalid subpath: {subpath}"),
            RootError::DuplicateName { name } => write!(f, "duplicate root name: {name}"),
            RootError::RelativeLocalPath { path } => write!(f, "local path not absolute: {path}"),
        }
    }
}

impl std::error::Error for RootError {}

/// Replie un libellé quelconque en un name de racine conforme à `valid_name`.
///
/// L'utilisateur ne saisit plus ce name : les assistants le dérivent du name du
/// partage ou du dernier segment du path choisi. Comme il devient **un
/// composant du path de montage et un name de fichier d'identifiants**, la
/// dérivation doit produire du valide par construction — un refus après
/// dérivation serait un défaut que rien in_dir l'IHM ne permettrait de corriger.
///
/// `pris` porte les names déjà employés : sans dédoublonnage, une deuxième
/// source écraserait le fichier d'identifiants de la première et se disputerait
/// son point de montage.
pub fn derive_name(indice: &str, pris: &[&str]) -> String {
    let base = fold(indice);
    if !pris.contains(&base.as_str()) {
        return base;
    }
    for n in 2..1000 {
        let suffixe = format!("-{n}");
        // Tronquer **avant** de concaténer : add le suffixe à un name déjà
        // long produirait un name refusé, donc une source impossible à déclarer
        // une deuxième fois.
        let tete: String = base.chars().take(32 - suffixe.len()).collect();
        let candidat = format!("{}{suffixe}", tete.trim_end_matches('-'));
        if !pris.contains(&candidat.as_str()) {
            return candidat;
        }
    }
    base
}

/// Le repliage lui-même : minuscules ASCII, tiret pour tout le reste.
///
/// Le premier caractère est alphanumérique **par construction** — on ne push_cover
/// jamais de tiret sur une chaîne clear — ce qui satisfait la première règle de
/// `valid_name` sans avoir à la vérifier après coup.
fn fold(indice: &str) -> String {
    let mut out = String::new();
    let mut tiret = false;
    for c in indice.chars() {
        let c = without_accents(c);
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            tiret = false;
        } else if !out.is_empty() && !tiret {
            out.push('-');
            tiret = true;
        }
    }
    let tronque: String = out.chars().take(32).collect();
    let net = tronque.trim_end_matches('-').to_string();
    // Un indice entièrement non-ASCII ne laisse rien : mieux vaut un name
    // générique qu'une source impossible à déclarer.
    if net.is_empty() {
        "source".to_string()
    } else {
        net
    }
}

/// Replie les accents latins courants.
///
/// Une table plutôt qu'une caisse de normalisation Unicode : quinze lines
/// couvrent le français, l'espagnol et l'allemand, et tout le reste tombe de
/// toute façon sur le tiret. « Été » qui deviendrait « t » serait un name exact
/// mais illisible in_dir les logs et sous `/mnt/ritornello`.
fn without_accents(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'A',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
        'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => 'O',
        'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
        'Ç' => 'C',
        'Ñ' => 'N',
        'Ý' => 'Y',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn racine_smb() -> Root {
        Root {
            name: "nas".into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: "musique".into(),
            subpath: Some("Albums".into()),
            user: "steven".into(),
            domain: String::new(),
            writable: false,
        }
    }

    fn racine_locale() -> Root {
        Root {
            name: "usb".into(),
            kind: RootKind::Local,
            path: Some("/media/usb".into()),
            host: String::new(),
            share: String::new(),
            subpath: None,
            user: String::new(),
            domain: String::new(),
            writable: false,
        }
    }

    fn roots_avec(root: Root) -> Roots {
        Roots { root: vec![root] }
    }

    #[test]
    fn un_nom_de_racine_hors_grammaire_est_refuse() {
        // Le name devient un composant de path (/mnt/ritornello/<name>) ET un
        // name de fichier d'identifiants. Tout ce qui n'est pas [a-z0-9-]
        // ouvrirait une traversée de répertoire du côté privilégié.
        for mauvais in ["../evasion", "Nas", "nas/musique", "", "nas musique", "-nas", "nas.."] {
            let r = roots_avec(Root { name: mauvais.into(), ..racine_smb() });
            assert!(
                matches!(r.validate(), Err(RootError::BadName { .. })),
                "accepte a tort : {mauvais:?}"
            );
        }
    }

    #[test]
    fn une_virgule_dans_l_hote_ou_le_partage_est_refusee() {
        // LA faille à ne pas manquer : les options de mount.cifs sont séparées
        // par des virgules. Un hôte « nas,uid=0 » injecterait une option in_dir
        // la line de montage exécutée par root.
        let r = roots_avec(Root { host: "nas,uid=0".into(), ..racine_smb() });
        assert!(matches!(r.validate(), Err(RootError::BadHost { .. })));
        let r = roots_avec(Root { share: "musique,rw".into(), ..racine_smb() });
        assert!(matches!(r.validate(), Err(RootError::BadShare { .. })));
    }

    #[test]
    fn un_sous_chemin_qui_remonte_ou_qui_est_absolu_est_refuse() {
        let r = roots_avec(Root { subpath: Some("../../etc".into()), ..racine_smb() });
        assert!(matches!(r.validate(), Err(RootError::BadSubpath { .. })));
        let r = roots_avec(Root { subpath: Some("/etc".into()), ..racine_smb() });
        assert!(matches!(r.validate(), Err(RootError::BadSubpath { .. })));
    }

    #[test]
    fn un_sous_chemin_a_espaces_est_accepte() {
        // Le défaut corrigé. `field_on` refuse l'espace parce que ses valeurs
        // atterrissent in_dir la line d'options de mount.cifs, séparée par des
        // virgules. Un sous-path n'y entre JAMAIS : `mount_command` ne pose que
        // l'hôte, le partage et `mount_point()`, qui l'ignore. Lui appliquer la
        // même règle rendait « Ma Musique » indéclarable pour une raison qui ne le
        // concerne pas — et l'assistant propose désormais n'importe quel dossier.
        let r = roots_avec(Root { subpath: Some("Ma Musique/Jazz, live".into()), ..racine_smb() });
        assert!(r.validate().is_ok(), "{:?}", r.validate());
    }

    #[test]
    fn un_sous_chemin_qui_remonte_reste_refuse() {
        for mauvais in ["../../etc", "/etc", "a/../../b", "a//b", "a/./b", "a\0b", ""] {
            let r = roots_avec(Root { subpath: Some(mauvais.into()), ..racine_smb() });
            assert!(
                matches!(r.validate(), Err(RootError::BadSubpath { .. })),
                "accepte a tort : {mauvais:?}"
            );
        }
    }

    #[test]
    fn un_nom_derive_est_toujours_accepte_par_la_grammaire() {
        // L'invariant qui compte : ce name devient un composant du path de
        // montage ET un name de fichier d'identifiants. La dérivation doit produire
        // du valide par construction, jamais par chance — l'utilisateur ne voit
        // plus ce name et n'aurait aucun moyen de corriger un refus.
        let hostiles = [
            "../etc", "Ma Musique", "Éric's Jazz!", "///", "", "$$$", "3615",
            "CamelCase", "a b c d e f g h i j k l m n o p q r s t u v w x y z 0 1 2 3",
            "日本語", "-début-tiret-", "fin-tiret---",
        ];
        for h in hostiles {
            let n = derive_name(h, &[]);
            assert!(valid_name(&n), "indice {h:?} a donne un name refuse : {n:?}");
        }
    }

    #[test]
    fn deux_indices_identiques_donnent_deux_noms_distincts() {
        // Sans dédoublonnage, la deuxième source écraserait le fichier
        // d'identifiants de la première et se disputerait son point de montage.
        let a = derive_name("Musique", &[]);
        let b = derive_name("Musique", &[a.as_str()]);
        let c = derive_name("Musique", &[a.as_str(), b.as_str()]);
        assert_eq!(a, "musique");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert!(valid_name(&b) && valid_name(&c));
    }

    #[test]
    fn un_indice_tres_long_reste_dedoublonnable() {
        // Le suffixe doit rentrer in_dir les 32 caractères : le concaténer sans
        // tronquer d'abord produirait un name refusé, donc une source impossible à
        // déclarer une deuxième fois.
        let long = "a".repeat(60);
        let a = derive_name(&long, &[]);
        let b = derive_name(&long, &[a.as_str()]);
        assert!(valid_name(&a) && valid_name(&b), "{a:?} / {b:?}");
        assert_ne!(a, b);
    }

    #[test]
    fn les_accents_se_replient_au_lieu_de_disparaitre() {
        // « Été » qui deviendrait « t » serait un name exact mais illisible in_dir les
        // logs et in_dir /mnt/ritornello.
        assert_eq!(derive_name("Été à Nîmes", &[]), "ete-a-nimes");
    }

    #[test]
    fn deux_racines_de_meme_nom_sont_refusees() {
        // Elles se disputeraient le même point de montage et le même fichier
        // d'identifiants.
        let r = Roots { root: vec![racine_smb(), racine_smb()] };
        assert!(matches!(r.validate(), Err(RootError::DuplicateName { .. })));
    }

    #[test]
    fn une_racine_locale_veut_un_chemin_absolu() {
        let r = roots_avec(Root { path: Some("media/usb".into()), ..racine_locale() });
        assert!(matches!(r.validate(), Err(RootError::RelativeLocalPath { .. })));
        assert!(roots_avec(racine_locale()).validate().is_ok());
    }

    #[test]
    fn une_racine_valide_passe_et_ses_repertoires_sont_imposes() {
        let r = roots_avec(racine_smb());
        assert!(r.validate().is_ok());
        // Le point de montage n'est JAMAIS lu depuis la configuration, et le
        // sous-path n'y entre pas : c'est le partage entier qui est monté.
        assert_eq!(r.root[0].mount_point(), PathBuf::from("/mnt/ritornello/nas"));
        assert_eq!(r.root[0].base_dir(), PathBuf::from("/mnt/ritornello/nas/Albums"));
    }

    #[test]
    fn chaque_refus_resout_contre_le_catalogue_embarque() {
        // `Catalog::get` rend la clé quand il ne la trouve pas : sans ce test,
        // une faute de frappe afficherait « bad_share » à l'écran sans que rien
        // ne bronche. On résout donc contre le sources_catalog réellement embarqué,
        // et on refuse un message réduit à sa propre clé.
        let catalog =
            Catalog::load("files", "en", Path::new("/inexistant"), crate::FILES_EN);
        let messages = [
            RootError::BadName { name: "x/y".into() }.message(&catalog),
            RootError::BadHost { host: "a,b".into() }.message(&catalog),
            RootError::BadShare { share: "a,b".into() }.message(&catalog),
            RootError::BadSubpath { subpath: "..".into() }.message(&catalog),
            RootError::DuplicateName { name: "nas".into() }.message(&catalog),
            RootError::RelativeLocalPath { path: "media/usb".into() }.message(&catalog),
        ];
        for m in &messages {
            assert!(m.contains(' '), "message reduit a une key brute : {m:?}");
        }
        // Et l'interpolation aboutit : pas de jeton laissé tel quel.
        let bounded = RootError::BadHost { host: "nas,uid=0".into() }.message(&catalog);
        assert!(bounded.contains("nas,uid=0"), "le refus doit nommer ce qui cloche : {bounded:?}");
        assert!(!bounded.contains("{host}"), "jeton laisse tel quel : {bounded:?}");
    }

    #[test]
    fn une_table_se_relit_depuis_le_toml() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("media-roots.toml");
        std::fs::write(
            &f,
            r#"
[[root]]
name = "nas"
kind = "smb"
host = "192.168.1.20"
share = "musique"
subpath = "Albums"
user = "steven"

[[root]]
name = "usb"
kind = "local"
path = "/media/usb"
"#,
        )
        .unwrap();
        let roots = Roots::load(&f).unwrap();
        assert_eq!(roots.root.len(), 2);
        assert_eq!(roots.by_name("nas").unwrap().kind, RootKind::Smb);
        assert_eq!(roots.by_name("usb").unwrap().base_dir(), PathBuf::from("/media/usb"));
        // Le défaut de `writable` compte : un partage n'est pas inscriptible
        // sans qu'on l'ait demandé.
        assert!(!roots.by_name("nas").unwrap().writable);
    }
}
