//! Les volumes montés de l'appareil : ce qu'un assistant peut proposer de
//! parcourir, et ce qu'il doit refuser.
//!
//! Tout est pur et prend le texte de `/proc/mounts` plutôt que de le lire :
//! c'est ce qui permet d'éprouver la garde de parcours sans monter quoi que ce
//! soit, ce qu'un test ne pourrait pas faire sans privilège.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Systèmes de fichiers qui ne portent pas de fichiers de l'utilisateur.
///
/// **Liste noire, et non liste blanche — décision revue en cours de route.**
///
/// La première version énumérait au contraire les systèmes de fichiers
/// acceptés. Le raisonnement était qu'une liste noire oublierait le prochain
/// pseudo-système de fichiers du noyau. Il était faux, parce qu'il pesait le
/// mauvais risque : l'asymétrie des conséquences va dans l'autre sens.
///
/// - Une liste blanche incomplète rend **un vrai disque inutilisable**, sans
///   aucun contournement offert à l'utilisateur. C'est arrivé : `/mnt/c` sous
///   WSL est un `9p`, et un disque USB en NTFS monté par ntfs-3g apparaît en
///   `fuseblk` — deux types qu'on n'avait pas prévus, deux blocages nets.
/// - Une liste noire incomplète laisse passer **une entrée parasite** dans une
///   liste de choix. Le désagrément est visible, réversible et mineur.
///
/// Ce que la liste noire doit encore garantir tient : `proc` y figure, donc la
/// garde refuse toujours `/proc/self` et son arborescence récursive.
///
/// `overlay` n'y est **pas** : sur un système conteneurisé, c'est la racine
/// elle-même. L'exclure rendrait tout invisible, ce qui est exactement l'erreur
/// qu'on vient de corriger. Ses quelques entrées parasites sous WSL sont le
/// moindre mal.
const FS_PSEUDO: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "nsfs",
    "proc",
    "pstore",
    "ramfs",
    "rootfs",
    "rpc_pipefs",
    "securityfs",
    "selinuxfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// Vrai si ce type de montage peut porter la musique de quelqu'un.
fn fs_utile(fstype: &str) -> bool {
    !FS_PSEUDO.contains(&fstype)
}

const PROC_MOUNTS: &str = "/proc/mounts";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Volume {
    pub path: PathBuf,
    pub fstype: String,
}

/// Déséchappe un champ de `/proc/mounts` : l'espace y est écrit `\040` et la
/// tabulation `\011`.
fn desechappe(s: &str) -> String {
    s.replace("\\040", " ").replace("\\011", "\t")
}

/// Tous les montages, **pseudo-systèmes de fichiers compris**.
///
/// La garde de parcours en a besoin entiers : c'est en connaissant le montage
/// de `/proc` qu'on peut refuser `/proc/self`.
fn tous(proc_mounts: &str) -> Vec<Volume> {
    proc_mounts
        .lines()
        .filter_map(|l| {
            let mut c = l.split_whitespace();
            let _source = c.next()?;
            let point = c.next()?;
            let fstype = c.next()?;
            Some(Volume { path: PathBuf::from(desechappe(point)), fstype: fstype.to_string() })
        })
        .collect()
}

/// Volumes proposables à l'utilisateur, triés.
pub fn volumes(proc_mounts: &str) -> Vec<Volume> {
    let mut retenus: Vec<Volume> = Vec::new();
    for v in tous(proc_mounts) {
        if !fs_utile(&v.fstype) {
            continue;
        }
        // Un même point monté deux fois n'apparaît qu'une fois, et c'est le
        // dernier montage qui compte — comme pour le noyau.
        match retenus.iter_mut().find(|r| r.path == v.path) {
            Some(place) => *place = v,
            None => retenus.push(v),
        }
    }
    retenus.sort_by(|a, b| a.path.cmp(&b.path));
    retenus
}

/// Le montage **propriétaire** d'un chemin : le point de montage le plus long
/// qui le préfixe.
///
/// C'est la seule formulation correcte. Un test « le chemin commence par un
/// volume » accepterait `/proc/self/root`, puisque `/proc` commence par `/`,
/// qui est bien un volume.
///
/// À égalité de longueur, `max_by_key` rend le **dernier** élément, ce qui est
/// exactement la sémantique du surmontage : le dernier monté est celui qu'on
/// voit.
pub fn proprietaire(proc_mounts: &str, chemin: &Path) -> Option<Volume> {
    tous(proc_mounts)
        .into_iter()
        .filter(|v| chemin.starts_with(&v.path))
        .max_by_key(|v| v.path.as_os_str().len())
}

/// Vrai si `chemin` peut être parcouru : son montage propriétaire porte un vrai
/// système de fichiers.
pub fn parcourable(proc_mounts: &str, chemin: &Path) -> bool {
    proprietaire(proc_mounts, chemin)
        .map(|v| fs_utile(&v.fstype))
        .unwrap_or(false)
}

/// Contenu de `/proc/mounts`.
///
/// Le chemin est surchargeable par `RITORNELLO_FILES_PROC_MOUNTS` : c'est ce
/// qui permet au parcours de bout en bout de décrire des volumes sans en
/// monter, sur une machine où le test n'a aucun privilège.
pub fn lire_proc_mounts() -> String {
    let chemin =
        std::env::var("RITORNELLO_FILES_PROC_MOUNTS").unwrap_or_else(|_| PROC_MOUNTS.to_string());
    std::fs::read_to_string(chemin).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un /proc/mounts réaliste de Raspberry Pi : la racine, la partition de
    /// démarrage, une clé USB, et les pseudo-systèmes de fichiers qui doivent
    /// rester invisibles.
    const MOUNTS: &str = "\
proc /proc proc rw,relatime 0 0
sysfs /sys sysfs rw,relatime 0 0
/dev/mmcblk0p2 / ext4 rw,relatime 0 0
devtmpfs /dev devtmpfs rw 0 0
tmpfs /run tmpfs rw,nosuid 0 0
/dev/mmcblk0p1 /boot/firmware vfat rw,relatime 0 0
/dev/sda1 /media/ma\\040cle exfat rw,relatime 0 0
//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0
";

    #[test]
    fn les_pseudo_systemes_de_fichiers_ne_sont_pas_proposes() {
        // Liste noire, et non liste blanche : voir FS_PSEUDO pour l'asymétrie
        // des conséquences qui a fait revoir ce choix.
        let v: Vec<String> = volumes(MOUNTS).iter().map(|v| v.path.display().to_string()).collect();
        assert_eq!(v, vec!["/", "/boot/firmware", "/media/ma cle", "/mnt/ritornello/nas"]);
    }

    #[test]
    fn un_systeme_de_fichiers_inconnu_reste_proposable() {
        // LE défaut que la liste blanche avait causé, désormais épinglé. Trois
        // cas rencontrés pour de vrai :
        //   - `9p` : /mnt/c sous WSL, donc tout le disque de la machine hôte ;
        //   - `fuseblk` : un disque USB en NTFS monté par ntfs-3g, le cas le
        //     plus banal d'une clé venue de Windows ;
        //   - `virtiofs` : un partage de machine virtuelle.
        // Aucun n'était prévu, et chacun rendait un vrai disque inatteignable
        // sans le moindre contournement offert à l'utilisateur.
        let m = "\
C:\\134 /mnt/c 9p rw,noatime 0 0
/dev/sdb1 /media/usb fuseblk rw,relatime 0 0
partage /mnt/hote virtiofs rw 0 0
";
        let v: Vec<String> = volumes(m).iter().map(|v| v.path.display().to_string()).collect();
        assert_eq!(v, vec!["/media/usb", "/mnt/c", "/mnt/hote"]);
        assert!(parcourable(m, Path::new("/mnt/c/projets/musique")));
        assert!(parcourable(m, Path::new("/media/usb/Albums")));
    }

    #[test]
    fn un_recouvrement_conteneurise_reste_visible() {
        // `overlay` est délibérément absent de la liste noire : sur un système
        // conteneurisé, c'est la racine elle-même, et l'exclure rendrait tout
        // invisible — exactement l'erreur que la liste blanche commettait.
        let m = "overlay / overlay rw 0 0\nproc /proc proc rw 0 0\n";
        assert!(parcourable(m, Path::new("/srv/musique")));
        assert!(!parcourable(m, Path::new("/proc/self")));
    }

    #[test]
    fn un_point_de_montage_avec_espace_echappe_est_deechappe() {
        // /proc/mounts échappe l'espace en \040. Sans ce traitement, la clé
        // « ma cle » serait proposée sous un nom que le système de fichiers ne
        // connaît pas, et le parcours échouerait à l'ouverture.
        assert!(volumes(MOUNTS).iter().any(|v| v.path == Path::new("/media/ma cle")));
    }

    #[test]
    fn le_montage_proprietaire_est_le_plus_long_prefixe() {
        // LA règle qui rend la garde correcte. Un test naïf « commence par un
        // volume » accepterait /proc/self/root, puisque /proc commence par /,
        // qui est un volume.
        let p = proprietaire(MOUNTS, Path::new("/boot/firmware/config.txt")).unwrap();
        assert_eq!(p.path, PathBuf::from("/boot/firmware"));
        let p = proprietaire(MOUNTS, Path::new("/home/pi/musique")).unwrap();
        assert_eq!(p.path, PathBuf::from("/"));
    }

    #[test]
    fn les_pseudo_systemes_de_fichiers_ne_sont_pas_parcourables() {
        // Pas pour le secret — ils sont lisibles de toute façon — mais parce
        // qu'un « tout ajouter » lancé sur /proc partirait dans les liens
        // récursifs de /proc/self.
        assert!(!parcourable(MOUNTS, Path::new("/proc/self")));
        assert!(!parcourable(MOUNTS, Path::new("/sys/class")));
        assert!(!parcourable(MOUNTS, Path::new("/run/user/1000")));
        assert!(!parcourable(MOUNTS, Path::new("/dev/shm")));
    }

    #[test]
    fn un_chemin_sous_un_vrai_volume_est_parcourable() {
        assert!(parcourable(MOUNTS, Path::new("/media/ma cle/Albums")));
        assert!(parcourable(MOUNTS, Path::new("/home/pi/musique")));
        assert!(parcourable(MOUNTS, Path::new("/")));
    }

    #[test]
    fn un_surmontage_est_celui_qui_compte() {
        // Deux montages au même endroit : c'est le dernier qui est visible,
        // comme pour le noyau. Se tromper ici ferait déclarer parcourable un
        // chemin que le tmpfs a recouvert.
        let m = "/dev/sda1 /media/x ext4 rw 0 0\ntmpfs /media/x tmpfs rw 0 0\n";
        assert_eq!(proprietaire(m, Path::new("/media/x/a")).unwrap().fstype, "tmpfs");
        assert!(!parcourable(m, Path::new("/media/x/a")));
    }

    #[test]
    fn une_ligne_tronquee_est_ignoree_sans_paniquer() {
        // /proc/mounts est lu à chaud : une ligne partielle ne doit pas faire
        // tomber la page entière.
        assert!(volumes("/dev/sda1\n\n/dev/sdb1 /media/y\n").is_empty());
    }
}
