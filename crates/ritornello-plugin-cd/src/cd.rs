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

/// Queries the drive through the CDROM_DRIVE_STATUS ioctl.
/// O_NONBLOCK is essential: without it, open() fails when there is no disc.
pub fn drive_status(dev: &Path) -> Result<DriveStatus> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(dev)
        .with_context(|| format!("opening {}", dev.display()))?;
    // SAFETY: read-only ioctl on a valid fd; the argument is an int passed by
    // value, as linux/cdrom.h defines it for CDROM_DRIVE_STATUS.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), CDROM_DRIVE_STATUS, CDSL_CURRENT) };
    Ok(match r {
        1 => DriveStatus::NoDisc,
        2 => DriveStatus::TrayOpen,
        3 => DriveStatus::NotReady,
        4 => DriveStatus::DiscOk,
        other => DriveStatus::Unknown(other),
    })
}

/// Polls every 2 s; sends `true`/`false` whenever the disc presence changes.
pub async fn watch(dev: PathBuf, tx: tokio::sync::mpsc::Sender<bool>) {
    let mut present = false;
    // A probe error counts as "no disc" for presence — an unplugged drive must
    // not make the plugin panic — but it is logged **on the first failure**
    // (then silenced until things return to normal): a binary outside the
    // `cdrom` group, or a wrong RITORNELLO_CD_DEV, displayed "no disc" forever
    // without a single log line, while the logs are the only diagnostic tool
    // on the device.
    let mut last_error_reported = false;
    loop {
        let now = match drive_status(&dev) {
            Ok(status) => {
                last_error_reported = false;
                status == DriveStatus::DiscOk
            }
            Err(e) => {
                if !last_error_reported {
                    tracing::warn!("cd drive probe {}: {e:#}", dev.display());
                    last_error_reported = true;
                }
                false
            }
        };
        if now != present {
            present = now;
            let _ = tx.send(now).await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Disc TOC through the cd-discid utility (Debian package).
///
/// The raw output (`NTRACKS OFF1 … OFFN LEADOUT`) is what goes into the track
/// identity, as is: it is a standard disc description, and it is up to the
/// `metadata` plugin to put it in the format its provider expects. This plugin
/// knows no metadata provider at all.
pub fn read_toc(dev: &str) -> Result<String> {
    let out = std::process::Command::new("cd-discid")
        .arg("--musicbrainz")
        .arg(dev)
        .output()
        .context("cd-discid not found (apt install cd-discid)")?;
    if !out.status.success() {
        bail!("cd-discid failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Number of tracks announced by the TOC, with a consistency check.
///
/// This is all this plugin needs from the TOC: bounding the track index. The
/// validation is nevertheless complete, because an inconsistent TOC must be
/// refused **here** rather than sent into the identity, where it would make a
/// third-party service get queried for nothing.
pub fn toc_ntracks(raw: &str) -> Result<usize> {
    let nums: Vec<u64> = raw
        .split_whitespace()
        .map(|s| s.parse::<u64>())
        .collect::<Result<_, _>>()
        .context("non-numeric cd-discid output")?;
    if nums.len() < 3 {
        bail!("cd-discid output too short: {raw:?}");
    }
    let ntracks = nums[0] as usize;
    if nums.len() != ntracks + 2 {
        bail!("inconsistent cd-discid output ({} fields for {} tracks)", nums.len(), ntracks);
    }
    Ok(ntracks)
}

pub fn eject(dev: &str) {
    // Best-effort — the tray may be stuck, that is physical — but never
    // silent: an `eject` binary missing from the system gave a tray that never
    // opens, with no trace to put next to the key that was pressed.
    match std::process::Command::new("eject").arg(dev).status() {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!("eject {dev}: {status}"),
        Err(e) => tracing::warn!("eject {dev}: {e} (eject package installed?)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_tracks_of_a_well_formed_toc() {
        // 3 tracks, offsets 150/22767/41887, leadout 63000
        assert_eq!(toc_ntracks("3 150 22767 41887 63000\n").unwrap(), 3);
    }

    #[test]
    fn invalid_toc_is_rejected() {
        assert!(toc_ntracks("").is_err());
        assert!(toc_ntracks("3 150 22767\n").is_err()); // not enough fields
        assert!(toc_ntracks("abc def\n").is_err());
    }
}
