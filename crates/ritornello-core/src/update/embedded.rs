//! Reads the manifest block of a switched-off plugin's binary.
//!
//! A plugin that is switched off is never started, so it cannot announce its
//! version. The SDK embeds a small block in every plugin binary instead
//! (`ritornello_plugin_sdk::embedded`), and this module finds it by reading the
//! file as bytes. It reads; it never runs, `exec`s or maps the file.
//!
//! Every read is bounded, because the path is data the core did not make:
//!
//! - the marker is unique and the payload is capped (in the SDK scanner);
//! - the file must be a regular file (checked here, through symlinks): opening
//!   a FIFO for reading blocks until a writer shows up, and a device can
//!   stream for ever;
//! - the file is at most [`SCAN_LIMIT`] bytes;
//! - the whole read has a deadline, [`READ_DEADLINE`]: no HTTP route may
//!   block, and an SD card can stall.
//!
//! Anything that goes wrong is "unknown" (`None`), never an error that could
//! reach the page.

use ritornello_plugin_sdk::embedded::{scan, Embedded};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The largest file read, and the most the scanner reads from it.
pub const SCAN_LIMIT: u64 = 64 * 1024 * 1024;

/// How long one file may take to read before its version stays unknown.
pub const READ_DEADLINE: Duration = Duration::from_secs(5);

/// Reads the block of the file at `path`, synchronously. `None` for anything
/// that is not a regular file of at most [`SCAN_LIMIT`] bytes carrying exactly
/// one well-formed block. Call it through [`read_block_within`] from async
/// code.
pub fn read_block(path: &Path) -> Option<Embedded> {
    // `metadata` follows symlinks, so a link to a device or a FIFO is refused
    // here, before anything is opened.
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > SCAN_LIMIT {
        return None;
    }
    scan(File::open(path).ok()?, SCAN_LIMIT)
}

/// [`read_block`] off the async threads and under `deadline`. A timeout, a
/// panic or a missing block all leave the version unknown.
pub async fn read_block_within(path: PathBuf, deadline: Duration) -> Option<Embedded> {
    let shown = path.display().to_string();
    let job = tokio::task::spawn_blocking(move || read_block(&path));
    match tokio::time::timeout(deadline, job).await {
        Ok(Ok(Some(block))) => Some(block),
        Ok(Ok(None)) => {
            tracing::info!("no readable manifest block in {shown}; its version stays unknown");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!("reading the manifest block of {shown} failed: {e}");
            None
        }
        Err(_) => {
            tracing::warn!(
                "reading the manifest block of {shown} took longer than {deadline:?}; \
                 its version stays unknown"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_plugin_sdk::embedded::encode;
    use std::io::Write;
    use std::time::Instant;

    const REPO: &str = "https://github.com/skerdudou/ritornello";

    fn file_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn a_file_carrying_a_block_is_read() {
        let mut bytes = vec![0xAB; 3000];
        bytes.extend_from_slice(&encode("0.2.0", Some(REPO)));
        bytes.extend(std::iter::repeat_n(0xAB, 3000));
        let f = file_with(&bytes);
        assert_eq!(
            read_block(f.path()),
            Some(Embedded {
                version: "0.2.0".into(),
                repository: Some(REPO.into()),
            })
        );
    }

    #[test]
    fn a_file_without_a_block_is_unknown() {
        let f = file_with(&[0xAB; 5000]);
        assert_eq!(read_block(f.path()), None);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_block(&dir.path().join("missing")), None);
    }

    #[test]
    fn a_file_larger_than_the_cap_is_unknown() {
        let f = file_with(&encode("0.2.0", None));
        assert!(read_block(f.path()).is_some(), "the block alone is read");
        f.as_file().set_len(SCAN_LIMIT + 1).unwrap();
        // The scanner would also give up past its limit, but only after
        // reading 64 MiB (about a second in a debug build): the size check
        // is what makes the refusal immediate.
        let started = Instant::now();
        assert_eq!(read_block(f.path()), None);
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "took {:?}: the file was read",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_path_that_is_not_a_regular_file_is_unknown_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        assert_eq!(
            read_block_within(dir.path().to_path_buf(), Duration::from_secs(2)).await,
            None
        );

        // Opening a FIFO for reading blocks until a writer opens it: without
        // the regular-file check the read would sit there until the deadline.
        let fifo = dir.path().join("fifo");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        // Even when an assertion below fails, release a blocked opener (a
        // broken implementation leaves one): the runtime waits for blocking
        // threads on drop, and the suite must never hang.
        let _release = ReleaseFifo(fifo.clone());
        assert_eq!(
            read_block_within(fifo.clone(), Duration::from_secs(2)).await,
            None
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "took {:?}",
            started.elapsed()
        );
    }

    /// On drop, opens the FIFO's write end without blocking: that wakes a
    /// reader stuck in `open`. Fails harmlessly when nobody is waiting.
    struct ReleaseFifo(PathBuf);

    impl Drop for ReleaseFifo {
        fn drop(&mut self) {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.0);
        }
    }
}
