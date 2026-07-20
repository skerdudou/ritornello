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

/// TOC au format MusicBrainz via l'utilitaire cd-discid (paquet Debian).
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

/// Transforme la sortie `NTRACKS OFF1 … OFFN LEADOUT` en paramètre toc
/// MusicBrainz `1+NTRACKS+LEADOUT+OFF1+…+OFFN`, plus le nombre de pistes.
pub fn mb_toc_param(raw: &str) -> Result<(String, usize)> {
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
    let leadout = nums[nums.len() - 1];
    let offsets: Vec<String> = nums[1..nums.len() - 1].iter().map(u64::to_string).collect();
    Ok((format!("1+{}+{}+{}", ntracks, leadout, offsets.join("+")), ntracks))
}

pub fn eject(dev: &str) {
    let _ = std::process::Command::new("eject").arg(dev).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_musicbrainz_bien_forme() {
        // 3 pistes, offsets 150/22767/41887, leadout 63000
        let raw = "3 150 22767 41887 63000\n";
        let (toc, n) = mb_toc_param(raw).unwrap();
        assert_eq!(toc, "1+3+63000+150+22767+41887");
        assert_eq!(n, 3);
    }

    #[test]
    fn toc_invalide_rejete() {
        assert!(mb_toc_param("").is_err());
        assert!(mb_toc_param("3 150 22767\n").is_err()); // pas assez de champs
        assert!(mb_toc_param("abc def\n").is_err());
    }
}
