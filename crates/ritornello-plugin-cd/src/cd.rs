use anyhow::{bail, Context, Result};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

// linux/cdrom.h
const CDROM_DRIVE_STATUS: libc::c_ulong = 0x5326;
const CDSL_CURRENT: libc::c_int = libc::c_int::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveStatus {
    NoDisc,
    TrayOpen,
    NotReady,
    DiscOk,
    Unknown(i32),
}

/// Interroge le lecteur via l'ioctl CDROM_DRIVE_STATUS.
/// O_NONBLOCK est indispensable : sans lui, open() échoue quand il n'y a pas de disque.
pub fn drive_status(dev: &Path) -> Result<DriveStatus> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(dev)
        .with_context(|| format!("ouverture de {}", dev.display()))?;
    // SAFETY: ioctl en lecture seule sur un fd valide ; l'argument est un int passé
    // par valeur comme le définit linux/cdrom.h pour CDROM_DRIVE_STATUS.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), CDROM_DRIVE_STATUS, CDSL_CURRENT) };
    Ok(match r {
        1 => DriveStatus::NoDisc,
        2 => DriveStatus::TrayOpen,
        3 => DriveStatus::NotReady,
        4 => DriveStatus::DiscOk,
        other => DriveStatus::Unknown(other),
    })
}

/// Poll toutes les 2 s ; retourne `true`/`false` sur changement de présence du disque.
pub async fn watch(dev: PathBuf, tx: tokio::sync::mpsc::Sender<bool>) {
    let mut present = false;
    loop {
        let now = matches!(drive_status(&dev), Ok(DriveStatus::DiscOk));
        if now != present {
            present = now;
            let _ = tx.send(now).await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// TOC du disque via l'utilitaire cd-discid (paquet Debian).
///
/// La sortie brute (`NTRACKS OFF1 … OFFN LEADOUT`) est ce qui part dans
/// l'identité du morceau, telle quelle : c'est une description standard de
/// disque, et c'est au plugin `metadata` de la mettre au format qu'attend son
/// fournisseur. Ce plugin-ci ne connaît aucun fournisseur de métadonnées.
pub fn read_toc(dev: &str) -> Result<String> {
    let out = std::process::Command::new("cd-discid")
        .arg("--musicbrainz")
        .arg(dev)
        .output()
        .context("cd-discid introuvable (apt install cd-discid)")?;
    if !out.status.success() {
        bail!("cd-discid a échoué: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Nombre de pistes annoncé par la TOC, avec vérification de cohérence.
///
/// C'est tout ce dont ce plugin a besoin de la TOC : borner l'index de piste.
/// La validation est néanmoins complète, parce qu'une TOC incohérente doit être
/// refusée **ici** plutôt que d'être envoyée dans l'identité, où elle ferait
/// interroger un service tiers pour rien.
pub fn toc_ntracks(raw: &str) -> Result<usize> {
    let nums: Vec<u64> = raw
        .split_whitespace()
        .map(|s| s.parse::<u64>())
        .collect::<Result<_, _>>()
        .context("sortie cd-discid non numérique")?;
    if nums.len() < 3 {
        bail!("sortie cd-discid trop courte: {raw:?}");
    }
    let ntracks = nums[0] as usize;
    if nums.len() != ntracks + 2 {
        bail!("sortie cd-discid incohérente ({} champs pour {} pistes)", nums.len(), ntracks);
    }
    Ok(ntracks)
}

pub fn eject(dev: &str) {
    let _ = std::process::Command::new("eject").arg(dev).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compte_les_pistes_dune_toc_bien_formee() {
        // 3 pistes, offsets 150/22767/41887, leadout 63000
        assert_eq!(toc_ntracks("3 150 22767 41887 63000\n").unwrap(), 3);
    }

    #[test]
    fn toc_invalide_rejete() {
        assert!(toc_ntracks("").is_err());
        assert!(toc_ntracks("3 150 22767\n").is_err()); // pas assez de champs
        assert!(toc_ntracks("abc def\n").is_err());
    }
}
