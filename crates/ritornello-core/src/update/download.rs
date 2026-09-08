//! Getting bytes onto the disk, and refusing to before they do damage.
//!
//! Two guards, and they answer different questions. `enough_room` asks whether
//! the device can afford this at all — a Pi whose root filesystem fills up
//! does not boot, and no update is worth that. `COMPRESSED_MAX` asks whether
//! one response is claiming to be something it should not be, applied **while
//! reading chunk by chunk**: checking an announced `Content-Length` protects
//! from nothing, it is declarative. Same idiom as the cover fetch.

use crate::system::Usage;
use crate::update::release::USER_AGENT;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 64 MiB per archive, compressed. The plugin bundle is the largest thing this
/// repository publishes and is well under it; a response that exceeds it is
/// not one of ours.
pub const COMPRESSED_MAX: usize = 64 * 1024 * 1024;

/// 256 MB of root filesystem the updater will not touch, whatever it is asked.
pub const ROOM_MARGIN_KB: u64 = 256 * 1024;

#[derive(Debug)]
pub enum DownloadError {
    NoRoom { available_kb: u64, needed_bytes: usize },
    TooLarge(usize),
    Http(String),
    /// The archive's hash is not the one `SHA256SUMS` gave for it.
    Digest { expected: String, got: String },
    /// `SHA256SUMS` has no line for this archive.
    NoDigest(String),
    Io(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRoom { available_kb, needed_bytes } => write!(
                f,
                "not enough room: {available_kb} kB available, {needed_bytes} bytes to download plus what they become plus a {ROOM_MARGIN_KB} kB margin"
            ),
            Self::TooLarge(cap) => write!(f, "the response exceeded {cap} bytes"),
            Self::Http(d) => write!(f, "fetching: {d}"),
            Self::Digest { expected, got } => write!(f, "digest mismatch: SHA256SUMS says {expected}, the download hashes to {got}"),
            Self::NoDigest(name) => write!(f, "SHA256SUMS has no line for {name}"),
            Self::Io(d) => write!(f, "writing: {d}"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// `<state>/staging`, under the service's own state directory so it can be
/// written without privilege.
///
/// Deliberately NOT `/var/lib/ritornello-update`, whose near-identical name
/// hides the boundary that matters: this one the service writes and root
/// distrusts; that one only root writes and the service can merely read.
pub fn staging_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("staging")
}

pub fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    // `{:02x}` per byte rather than a hex crate: one line, no dependency in a
    // graph that already compiles for three targets.
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Room for the archive, for what it decompresses to, for the copy kept aside
/// — and a margin.
///
/// Three times the download rather than one: the archive lands in staging, its
/// contents are extracted, and the privileged side keeps the previous binaries
/// aside before replacing them. Sizing this on the download alone is how a
/// device runs out of space halfway through an update.
pub fn enough_room(disk: Option<Usage>, needed_bytes: usize) -> bool {
    let Some(disk) = disk else {
        // An x86 box in a container, or a filesystem statvfs cannot read. The
        // System tab already shows an absent sensor as "—" rather than as a
        // fault; refusing to update over a missing metric would be a new class
        // of stuck device.
        return true;
    };
    let needed_kb = (needed_bytes as u64 / 1024) * 3 + ROOM_MARGIN_KB;
    disk.available_kb > needed_kb
}

/// One client for every request of one check, with the User-Agent GitHub
/// requires — it answers 403 without one.
pub fn client() -> Result<reqwest::Client, DownloadError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        // A check must not hang: the page polls for its outcome, and a request
        // with no deadline is how "checking…" becomes permanent.
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| DownloadError::Http(e.to_string()))
}

/// For the release JSON and for `SHA256SUMS`: small bodies, read whole.
pub async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<(u16, String), DownloadError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| DownloadError::Http(e.to_string()))?;
    let status = response.status().as_u16();
    // The status travels back rather than being turned into an error: 404 on
    // the latest-release endpoint means "no release published yet", which the
    // page must show as a statement and not as a fault.
    let body = response.text().await.map_err(|e| DownloadError::Http(e.to_string()))?;
    Ok((status, body))
}

/// An archive, capped while streaming.
pub async fn fetch_capped(
    client: &reqwest::Client,
    url: &str,
    cap: usize,
) -> Result<Vec<u8>, DownloadError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| DownloadError::Http(e.to_string()))?;
    if !response.status().is_success() {
        return Err(DownloadError::Http(format!("{} for {url}", response.status())));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| DownloadError::Http(e.to_string()))?
    {
        if bytes.len() + chunk.len() > cap {
            return Err(DownloadError::TooLarge(cap));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::Usage;

    #[test]
    fn the_digest_is_the_lowercase_hex_sha256() {
        // The empty string's SHA-256, a value that can be checked against any
        // implementation rather than against this one.
        assert_eq!(
            digest_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn room_is_judged_on_the_archive_plus_what_it_will_become_plus_a_margin() {
        let disk = Usage { total_kb: 8_000_000, available_kb: 700_000 };
        // 100 MiB of archives on 700 MB free: fine.
        assert!(enough_room(Some(disk), 100 * 1024 * 1024));
        // 400 MiB: not fine, because the archive is downloaded AND extracted
        // AND the previous binaries are kept aside. Three copies, not one.
        assert!(!enough_room(Some(disk), 400 * 1024 * 1024));
    }

    #[test]
    fn the_margin_is_what_stops_a_pi_from_filling_its_root() {
        // Exactly the margin free, and a one-byte download: still refused. A
        // device whose root filesystem is full does not boot, and an update is
        // never worth that.
        let disk = Usage { total_kb: 8_000_000, available_kb: ROOM_MARGIN_KB };
        assert!(!enough_room(Some(disk), 1));
    }

    /// An x86 box in a container, or a filesystem statvfs cannot read.
    ///
    /// Proceeding is the right answer: the System tab already shows every
    /// absent sensor as "—" rather than as a fault, and refusing to update
    /// because a metric is missing would be a new class of stuck device.
    #[test]
    fn an_unknown_disk_does_not_block_the_update() {
        assert!(enough_room(None, 100 * 1024 * 1024));
    }
}
