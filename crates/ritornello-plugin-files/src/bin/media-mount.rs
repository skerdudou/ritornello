//! Binaire **racine** : réconcilie les montages déclarés.
//!
//! Lancé par `ritornello-media-mount.service`, lui-même démarré par le plugin
//! via `systemctl` et autorisé par polkit sur cette seule unité.
//!
//! Il consomme une configuration écrite par un processus **non privilégié**. Il
//! revalide donc tout ce qu'il read : la validation faite côté plugin ne compte
//! pas comme une garantie, elle n'est qu'une politesse envers l'utilisateur.

use anyhow::{Context, Result};
use ritornello_plugin_files::mount::{is_mounted_in, mount_points};
use ritornello_plugin_files::mount_options::mount_command;
use ritornello_plugin_files::roots::{RootKind, Roots, MOUNT_ROOT};
use std::path::{Path, PathBuf};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Lit `uid` et `gid` de l'utilisateur du service in_dir `/etc/passwd`.
///
/// Une playback de fichier plutôt qu'une dépendance à `nix` ou `libc` : c'est
/// trois lines, testable, et cela évite de tirer une caisse entière in_dir un
/// binaire qui ne fait qu'appeler `mount`.
fn uid_gid(passwd: &str, utilisateur: &str) -> Option<(u32, u32)> {
    passwd.lines().find_map(|l| {
        let mut champs = l.split(':');
        (champs.next()? == utilisateur).then_some(())?;
        let _mot_de_passe = champs.next()?;
        let uid = champs.next()?.parse().ok()?;
        let gid = champs.next()?.parse().ok()?;
        Some((uid, gid))
    })
}

/// Points de montage sous `MOUNT_ROOT` actuellement montés.
///
/// L'analyse de `/proc/mounts` et son déséchappement viennent de
/// `mount::mount_points` : une seule implémentation de cette règle, sans
/// quoi les deux binaires divergeraient sur un détail rare — l'un traitant la
/// tabulation échappée et pas l'autre, par exemple.
fn mounted_under_root(proc_mounts: &str) -> Vec<PathBuf> {
    mount_points(proc_mounts).filter(|p| p.starts_with(MOUNT_ROOT)).collect()
}

/// Emplacements du programme d'aide `mount.cifs`. Les deux, et non le seul
/// `/sbin` : sur une distribution à `/usr` fusionné c'est le même fichier, sur
/// les autres ce ne l'est pas.
const CIFS_HINTS: [&str; 2] = ["/sbin/mount.cifs", "/usr/sbin/mount.cifs"];

/// `mount.cifs` est-il installé ?
///
/// Le prédicat d'existence est injecté plutôt que lu directement : la règle se
/// teste alors sans dépendre de la machine qui lance les tests, laquelle n'a ni
/// `cifs-utils` ni le droit d'en poser un fichier.
fn cifs_help<F: Fn(&str) -> bool>(existe: F) -> Option<&'static str> {
    CIFS_HINTS.into_iter().find(|c| existe(c))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let roots_path = env_or("RITORNELLO_FILES_ROOTS", "/etc/ritornello/media-roots.toml");
    let creds_dir = env_or("RITORNELLO_FILES_CREDENTIALS", "/etc/ritornello/media-credentials");
    let utilisateur = env_or("RITORNELLO_USER", "ritornello");

    let roots = match Roots::load(Path::new(&roots_path)) {
        Ok(r) => r,
        Err(e) => {
            // Absence de configuration = rien à monter, et non un échec : le
            // service est activé au démarrage de la machine, avant même qu'un
            // partage n'ait jamais été déclaré.
            if !Path::new(&roots_path).exists() {
                tracing::info!("{roots_path} does not exist yet: nothing to mount");
                return Ok(());
            }
            return Err(e).with_context(|| format!("reading {roots_path}"));
        }
    };
    // Ceinture et bretelles : `load` valide déjà, mais cette line est ce qui
    // rend l'invariant visible du côté privilégié.
    roots.validate()?;

    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    let (uid, gid) = uid_gid(&passwd, &utilisateur)
        .with_context(|| format!("no user named {utilisateur} in /etc/passwd"))?;

    let proc_mounts = std::fs::read_to_string("/proc/mounts").context("reading /proc/mounts")?;

    // Démonter d'abord ce qui n'est plus déclaré : une racine retirée de la
    // page doit disparaître, sinon le partage resterait monté jusqu'au
    // prochain redémarrage de la machine.
    let voulus: Vec<PathBuf> = roots
        .root
        .iter()
        .filter(|r| r.kind == RootKind::Smb)
        .map(|r| r.mount_point())
        .collect();
    for monte in mounted_under_root(&proc_mounts) {
        if voulus.contains(&monte) {
            continue;
        }
        let sortie = std::process::Command::new("umount").arg(&monte).output();
        match sortie {
            Ok(s) if s.status.success() => tracing::info!("unmounted {}", monte.display()),
            Ok(s) => tracing::warn!(
                "unmounting {}: {}",
                monte.display(),
                String::from_utf8_lossy(&s.stderr).trim()
            ),
            Err(e) => tracing::warn!("unmounting {}: {e}", monte.display()),
        }
    }

    // `mount -t cifs` ne monte pas par lui-même : il délègue à `mount.cifs`,
    // seul à savoir read un fichier `credentials=`. Sans ce programme, `mount`
    // appelle mount(2) directement, l'option n'est plus lue par personne et la
    // session ouverte est anonyme — refusée par le NAS. L'échec rendition est alors
    // « cannot mount //hôte/partage read-only », qui ne nomme ni
    // l'authentification ni le paquet manquant : constaté sur DietPi bookworm,
    // une heure pour l'attribuer. D'où ce contrôle préalable, qui remplace une
    // tentative dont le message égare par une line qui dit quoi installer.
    //
    // Après la boucle de démontage, et non avant : retirer un partage de la
    // page doit continuer à le démonter, ce dont `umount` s'acquitte seul.
    let a_monter = roots
        .root
        .iter()
        .filter(|r| r.kind == RootKind::Smb)
        .filter(|r| !is_mounted_in(&proc_mounts, &r.mount_point()))
        .count();
    if a_monter > 0 && cifs_help(|c| Path::new(c).exists()).is_none() {
        // `error!` puis sortie en succès : le service reste un réconciliateur
        // qui rend compte, et l'unité en échec n'apporterait rien de plus que
        // du bruit au démarrage de la machine.
        tracing::error!(
            "mount.cifs not found in /sbin or /usr/sbin: install cifs-utils \
             (see docs/installation.md); {a_monter} declared share(s) left unmounted"
        );
        return Ok(());
    }

    for r in roots.root.iter().filter(|r| r.kind == RootKind::Smb) {
        let point = r.mount_point();
        if is_mounted_in(&proc_mounts, &point) {
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&point) {
            tracing::error!("creating {}: {e}", point.display());
            continue;
        }
        let cmd = mount_command(r, Path::new(&creds_dir), uid, gid);
        match std::process::Command::new(&cmd[0]).args(&cmd[1..]).output() {
            // Un échec ne fait pas échouer le service : les autres partages
            // doivent être montés quand même, et l'utilisateur verra l'état
            // depuis la page.
            Ok(s) if s.status.success() => tracing::info!("mounted {}", r.name),
            Ok(s) => {
                tracing::error!("mounting {}: {}", r.name, String::from_utf8_lossy(&s.stderr).trim())
            }
            Err(e) => tracing::error!("mounting {}: {e}", r.name),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_MOUNTS: &str = "\
proc /proc proc rw,relatime 0 0
//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0
//192.168.1.20/photos /mnt/ritornello/ma\\040musique cifs ro 0 0
/dev/sda1 /media/usb ext4 rw 0 0
";

    #[test]
    fn un_point_de_montage_absent_de_proc_mounts_est_non_monte() {
        assert!(is_mounted_in(PROC_MOUNTS, Path::new("/mnt/ritornello/nas")));
        assert!(!is_mounted_in(PROC_MOUNTS, Path::new("/mnt/ritornello/autre")));
    }

    #[test]
    fn un_point_de_montage_avec_espace_echappe_est_reconnu() {
        // /proc/mounts échappe l'espace en \040. Sans ce traitement, le partage
        // passerait pour non monté et serait remonté à chaque réconciliation.
        assert!(is_mounted_in(PROC_MOUNTS, Path::new("/mnt/ritornello/ma musique")));
    }

    #[test]
    fn seuls_les_montages_sous_la_racine_sont_candidats_au_demontage() {
        // Le binaire tourne en root : il ne doit jamais démonter quoi que ce
        // soit hors de son domaine, /proc et /media/usb compris.
        let sous = mounted_under_root(PROC_MOUNTS);
        assert_eq!(
            sous,
            vec![
                PathBuf::from("/mnt/ritornello/nas"),
                PathBuf::from("/mnt/ritornello/ma musique")
            ]
        );
    }

    #[test]
    fn luid_et_le_gid_se_lisent_dans_passwd() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      ritornello:x:998:997::/var/lib/ritornello:/usr/sbin/nologin\n";
        assert_eq!(uid_gid(passwd, "ritornello"), Some((998, 997)));
        assert_eq!(uid_gid(passwd, "absent"), None);
    }

    #[test]
    fn un_passwd_tronque_ne_donne_pas_un_uid_faux() {
        // Mieux vaut ne rien rendre que rendre 0 : le montage porterait alors
        // les fichiers à root, et le service ne pourrait plus les read.
        assert_eq!(uid_gid("ritornello:x\n", "ritornello"), None);
        assert_eq!(uid_gid("ritornello:x:abc:997::/:/bin/sh\n", "ritornello"), None);
    }

    #[test]
    fn l_aide_cifs_est_cherchee_dans_les_deux_sbin() {
        assert_eq!(cifs_help(|c| c == "/sbin/mount.cifs"), Some("/sbin/mount.cifs"));
        // Une distribution sans /usr fusionné n'a que celui-là.
        assert_eq!(cifs_help(|c| c == "/usr/sbin/mount.cifs"), Some("/usr/sbin/mount.cifs"));
    }

    #[test]
    fn sans_cifs_utils_l_aide_est_absente() {
        // Le cas qui a coûté une heure sur l'appareil : `cifs-utils` non
        // installé, et un « cannot mount … read-only » qui ne le disait pas.
        assert_eq!(cifs_help(|_| false), None);
    }
}
