//! Getting bytes onto the disk, and refusing to before they do damage.
//!
//! Two guards, and they answer different questions. `enough_room` asks whether
//! the device can afford this at all — a Pi whose root filesystem fills up
//! does not boot, and no update is worth that. `COMPRESSED_MAX` (and
//! `TEXT_MAX` for the smaller bodies) asks whether one response is claiming
//! to be something it should not be, applied **while reading chunk by
//! chunk**: checking an announced `Content-Length` protects from nothing, it
//! is declarative. Same idiom as the cover fetch.
//!
//! `SHA256SUMS` travels in the same release over the same channel, so
//! `digest_hex` detects a corrupted or mis-served download and not a hostile
//! release; authenticity rests on HTTPS to a repository fixed at compile
//! time, not on this hash.

use crate::system::Usage;
use crate::update::release::USER_AGENT;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 64 MiB per archive, compressed. The plugin bundle is the largest thing this
/// repository publishes and is well under it; a response that exceeds it is
/// not one of ours.
pub const COMPRESSED_MAX: usize = 64 * 1024 * 1024;

/// A response body read as text has the same hostile position as an archive,
/// so it gets the same treatment. Four mebibytes is far above a hundred-release
/// listing or any SHA256SUMS, and far below what would trouble the device.
pub const TEXT_MAX: usize = 4 * 1024 * 1024;

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
/// Three times the **compressed** download — though that undercounts the
/// real cost, and it is the margin, not this factor, that closes the gap.
/// The archive itself never touches disk: it is fetched into memory and read
/// entry by entry, and only the one uncompressed binary it extracts is
/// written into staging; root then keeps the previous uncompressed binary
/// aside before the swap. So the true cost is closer to twice the
/// *uncompressed* size — typically five to six times this *compressed*
/// figure, not three. The 256 MB margin absorbs that undercount with an
/// order of magnitude to spare; raising the factor instead would refuse
/// legitimate installs on a tight disk for no measured benefit, so it stays
/// at three.
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
        .connect_timeout(std::time::Duration::from_secs(10))
        // A read timeout and NOT a total one, and the distinction is the whole
        // point: `timeout` runs from the first connect until the body is
        // finished, so any total deadline is really a minimum bandwidth
        // requirement. Sixty seconds for a 64 MiB cap demands 1 MiB/s, and the
        // plugin bundle at ~15 MB demands 250 kB/s sustained — a congested
        // Wi-Fi or ADSL link would then fail every night forever, reporting a
        // transport error rather than "too slow". `read_timeout` resets on
        // every successful read, so it bounds a server that stalls without
        // requiring the link to be fast.
        .read_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| DownloadError::Http(e.to_string()))
}

/// Reads a body chunk by chunk, refusing the moment the running total would
/// exceed `cap` — whatever `Content-Length` claims. Shared by `fetch_capped`
/// and `fetch_text`: a release listing or a `SHA256SUMS` file sits in exactly
/// the same hostile position as an archive, just behind a smaller cap.
///
/// `Content-Length`, when present, only seeds the buffer's initial capacity —
/// bounded by `cap` — as a hint, never as a bound: a body bigger than
/// declared costs a reallocation or two, never a way past the check below,
/// and a body that lies about being small buys nothing either, since every
/// chunk is still counted as it arrives.
async fn read_capped(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, DownloadError> {
    let hint = response
        .content_length()
        .map(|len| (len as usize).min(cap))
        .unwrap_or(0);
    let mut bytes = Vec::with_capacity(hint);
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

/// For the release listing and for `SHA256SUMS`: small bodies, read whole and
/// capped at `TEXT_MAX` (see `read_capped`).
pub async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<(u16, String), DownloadError> {
    let response = client
        .get(url)
        // A deadline belongs on this request and not on the client: a
        // listing or a checksum file is at most a few hundred kilobytes and
        // has no excuse to take a minute, unlike an archive on a slow link,
        // which is why the client itself carries no total timeout.
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| DownloadError::Http(e.to_string()))?;
    let status = response.status().as_u16();
    // The status travels back rather than becoming an error: the release
    // list answers `200 []` for a repository with no releases yet, and "no
    // release published" is what parsing that body says, not a status code
    // to special-case here.
    let bytes = read_capped(response, TEXT_MAX).await?;
    let body = String::from_utf8(bytes)
        .map_err(|e| DownloadError::Http(format!("body is not UTF-8: {e}")))?;
    Ok((status, body))
}

/// An archive, capped while streaming (see `read_capped`).
pub async fn fetch_capped(
    client: &reqwest::Client,
    url: &str,
    cap: usize,
) -> Result<Vec<u8>, DownloadError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| DownloadError::Http(e.to_string()))?;
    if !response.status().is_success() {
        return Err(DownloadError::Http(format!("{} for {url}", response.status())));
    }
    read_capped(response, cap).await
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
        // 1 200 000 kB free, chosen so the pair below pins the factor of
        // three from both sides: `* 4` would refuse the first row (needs
        // 1 490 944 kB) and `* 2` would accept the second (needs
        // 1 081 344 kB). A single figure that only tests one direction is not
        // enough — the controller's original 500 000/700 000 figures each
        // let at least one wrong factor through.
        let disk = Usage { total_kb: 8_000_000, available_kb: 1_200_000 };
        // 300 MiB needs (307200 * 3) + 262144 = 1 183 744 kB: fine.
        assert!(enough_room(Some(disk), 300 * 1024 * 1024));
        // 400 MiB needs (409600 * 3) + 262144 = 1 490 944 kB: not fine,
        // because the archive is downloaded AND extracted AND the previous
        // binaries are kept aside. Three copies, not one.
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

    // -- fetch_capped / fetch_text: the cap enforced against a real server --
    //
    // No `allowed_target`-style SSRF filter guards these functions the way
    // `cover::fetch` guards `cover::download`: their caller only ever hands
    // them a URL read out of a GitHub release the core itself fetched, so
    // `127.0.0.1` is not a special case to route around here.

    /// Serializes a body as `Transfer-Encoding: chunked`.
    fn chunked_body(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend_from_slice(format!("{:x}\r\n", c.len()).as_bytes());
            out.extend_from_slice(c);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    fn http_response(status_line: &str, headers: &str, body: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{status_line}\r\n{headers}\r\n").as_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Serves `response` to the first connection received on `127.0.0.1`, on
    /// a port chosen by the OS, then closes.
    async fn serve(response: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored).await;
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://127.0.0.1:{port}/asset")
    }

    /// Serves `response` then hangs without closing: if the caller reads the
    /// body regardless of what it was told, it stays blocked until the
    /// test's own timeout catches it.
    async fn serve_then_hang(response: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored).await;
                let _ = socket.write_all(&response).await;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
        format!("http://127.0.0.1:{port}/asset")
    }

    /// Serves a `Transfer-Encoding: chunked` body that never sends the
    /// terminating `0\r\n\r\n`: fresh chunks keep coming until the reader
    /// gives up or the write side breaks. A body that truly never ends is
    /// the only way to prove the cap is enforced **while streaming**, not
    /// merely once a body has finished arriving.
    async fn serve_endless_chunked(chunk_size: usize) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored).await;
                let headers =
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
                if socket.write_all(headers).await.is_err() {
                    return;
                }
                let chunk = vec![0u8; chunk_size];
                let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
                frame.extend_from_slice(&chunk);
                frame.extend_from_slice(b"\r\n");
                loop {
                    if socket.write_all(&frame).await.is_err() {
                        break;
                    }
                }
            }
        });
        format!("http://127.0.0.1:{port}/asset")
    }

    #[tokio::test]
    async fn the_cap_cuts_an_endless_chunked_stream_before_the_end_and_fast() {
        let cap = 10_000;
        let url = serve_endless_chunked(4_096).await;
        // Bounded well above what a healthy cut takes (milliseconds) but far
        // below the endless stream's actual duration: only a cap enforced
        // chunk by chunk returns inside this window.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fetch_capped(&client().unwrap(), &url, cap),
        )
        .await;
        match outcome {
            Ok(Err(DownloadError::TooLarge(c))) => assert_eq!(c, cap),
            other => panic!("expected a prompt TooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_lying_content_length_does_not_exempt_the_body_from_the_cap() {
        // `Content-Length: 5` while `Transfer-Encoding: chunked` carries
        // 20 000 bytes. RFC 7230 §3.3.3 says chunked framing overrides
        // Content-Length; if `fetch_capped` ever trusted the header instead
        // of the bytes it actually reads, this body would sail through under
        // a cap the header claims to already respect.
        let cap = 10_000;
        let chunks: Vec<Vec<u8>> = (0..5).map(|_| vec![0u8; 4_000]).collect();
        let body = chunked_body(&chunks);
        let response = http_response(
            "HTTP/1.1 200 OK",
            "Content-Type: application/octet-stream\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n",
            body,
        );
        let url = serve(response).await;
        match fetch_capped(&client().unwrap(), &url, cap).await {
            Err(DownloadError::TooLarge(c)) => assert_eq!(c, cap),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exactly_the_cap_is_ok_and_one_byte_more_is_too_large() {
        let cap = 1_000;
        let body = vec![0u8; cap];
        let response = http_response(
            "HTTP/1.1 200 OK",
            &format!("Content-Type: application/octet-stream\r\nContent-Length: {}\r\n", body.len()),
            body,
        );
        let url = serve(response).await;
        let bytes = fetch_capped(&client().unwrap(), &url, cap).await.unwrap();
        assert_eq!(bytes.len(), cap);

        let over = vec![0u8; cap + 1];
        let response = http_response(
            "HTTP/1.1 200 OK",
            &format!("Content-Type: application/octet-stream\r\nContent-Length: {}\r\n", over.len()),
            over,
        );
        let url = serve(response).await;
        match fetch_capped(&client().unwrap(), &url, cap).await {
            Err(DownloadError::TooLarge(c)) => assert_eq!(c, cap),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_non_2xx_status_is_refused_before_the_body_is_read() {
        let response = http_response(
            "HTTP/1.1 500 Internal Server Error",
            "Content-Type: text/plain\r\nContent-Length: 1000000\r\n",
            Vec::new(),
        );
        let url = serve_then_hang(response).await;
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fetch_capped(&client().unwrap(), &url, COMPRESSED_MAX),
        )
        .await
        {
            Ok(Err(DownloadError::Http(_))) => {}
            other => panic!("expected a prompt Http error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_text_refuses_a_body_over_text_max() {
        let body = vec![b'a'; TEXT_MAX + 1];
        let response = http_response(
            "HTTP/1.1 200 OK",
            &format!("Content-Type: text/plain\r\nContent-Length: {}\r\n", body.len()),
            body,
        );
        let url = serve(response).await;
        match fetch_text(&client().unwrap(), &url).await {
            Err(DownloadError::TooLarge(c)) => assert_eq!(c, TEXT_MAX),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// A body that is not valid UTF-8 must be refused, not silently
    /// repaired.
    ///
    /// This path parses a release listing and a `SHA256SUMS` file. A body
    /// patched with replacement characters (`String::from_utf8_lossy`) would
    /// still look like a string to every caller downstream, and would be
    /// handed to a JSON or line parser that then reports a malformed
    /// release — sending whoever reads that error after the wrong cause
    /// entirely, when the real fault was a body that was never text.
    #[tokio::test]
    async fn fetch_text_refuses_a_body_that_is_not_utf8() {
        let body = vec![0x80u8]; // a lone continuation byte: not valid UTF-8 on its own.
        let response = http_response(
            "HTTP/1.1 200 OK",
            &format!("Content-Type: text/plain\r\nContent-Length: {}\r\n", body.len()),
            body,
        );
        let url = serve(response).await;
        match fetch_text(&client().unwrap(), &url).await {
            Err(DownloadError::Http(_)) => {}
            other => panic!("expected a UTF-8 error, got {other:?}"),
        }
    }
}
