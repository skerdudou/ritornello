//! Binaire **racine** : réconcilie les montages déclarés.
//!
//! Lancé par `ritornello-media-mount.service`, lui-même démarré par le plugin
//! via `systemctl` et autorisé par polkit sur cette seule unité.
//!
//! Il consomme une configuration écrite par un processus **non privilégié**. Il
//! revalide donc tout ce qu'il lit : la validation faite côté plugin ne compte
//! pas comme une garantie, elle n'est qu'une politesse envers l'utilisateur.

use anyhow::{Context, Result};
use ritornello_plugin_files::mount::{est_monte_dans, points_de_montage};
use ritornello_plugin_files::mount_options::mount_command;
use ritornello_plugin_files::roots::{RootKind, Roots, MOUNT_ROOT};
use std::path::{Path, PathBuf};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Lit `uid` et `gid` de l'utilisateur du service dans `/etc/passwd`.
///
/// Une lecture de fichier plutôt qu'une dépendance à `nix` ou `libc` : c'est
/// trois lignes, testable, et cela évite de tirer une caisse entière dans un
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
/// `mount::points_de_montage` : une seule implémentation de cette règle, sans
/// quoi les deux binaires divergeraient sur un détail rare — l'un traitant la
/// tabulation échappée et pas l'autre, par exemple.
fn montes_sous_racine(proc_mounts: &str) -> Vec<PathBuf> {
    points_de_montage(proc_mounts).filter(|p| p.starts_with(MOUNT_ROOT)).collect()
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
    // Ceinture et bretelles : `load` valide déjà, mais cette ligne est ce qui
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
    for monte in montes_sous_racine(&proc_mounts) {
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

    for r in roots.root.iter().filter(|r| r.kind == RootKind::Smb) {
        let point = r.mount_point();
        if est_monte_dans(&proc_mounts, &point) {
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
        assert!(est_monte_dans(PROC_MOUNTS, Path::new("/mnt/ritornello/nas")));
        assert!(!est_monte_dans(PROC_MOUNTS, Path::new("/mnt/ritornello/autre")));
    }

    #[test]
    fn un_point_de_montage_avec_espace_echappe_est_reconnu() {
        // /proc/mounts échappe l'espace en \040. Sans ce traitement, le partage
        // passerait pour non monté et serait remonté à chaque réconciliation.
        assert!(est_monte_dans(PROC_MOUNTS, Path::new("/mnt/ritornello/ma musique")));
    }

    #[test]
    fn seuls_les_montages_sous_la_racine_sont_candidats_au_demontage() {
        // Le binaire tourne en root : il ne doit jamais démonter quoi que ce
        // soit hors de son domaine, /proc et /media/usb compris.
        let sous = montes_sous_racine(PROC_MOUNTS);
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
        // les fichiers à root, et le service ne pourrait plus les lire.
        assert_eq!(uid_gid("ritornello:x\n", "ritornello"), None);
        assert_eq!(uid_gid("ritornello:x:abc:997::/:/bin/sh\n", "ritornello"), None);
    }
}
