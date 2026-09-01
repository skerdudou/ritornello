//! Circuit breaker for embedded cover extraction: bounds the `lofty` call,
//! which may never return on a silent network share, and remembers the mount
//! points that no longer answer.
//!
//! # Why this bound exists
//!
//! `player::mpv::embedded_cover` opens and walks the file **currently
//! playing** with `lofty`, a strictly blocking call. That file may come from a
//! network share, and this project has already lived through the incident this
//! causes without a bound: a sleeping cifs mount made a whole admin page
//! disappear, an IO that never completes holding the loop that should have
//! answered everything else (see the project memory, and
//! `ritornello-plugin-files::health`, which solved the same problem for
//! duration probing). Here, the caller is directly the core's event loop:
//! without this bound, a silent share would freeze mpv, the commands and HTTP
//! all at once, not just an admin page.
//!
//! # Why not `ritornello-plugin-files::health` directly
//!
//! This module takes the **shape** of that circuit breaker (timeout +
//! `spawn_blocking` + mark per mount point) without depending on it: the core
//! must not bind itself to the `files` plugin for a mechanism of its own, and
//! it needs neither `volumes::browsable` (the blacklist of pseudo filesystems)
//! nor `group`/`missing` (designed to probe thousands of paths at once) — the
//! core only ever handles a single file at a time, the one mpv just opened.
//!
//! # Why an abandoned thread, and why only one per mount
//!
//! A system call in uninterruptible sleep cannot be killed — even `SIGKILL`
//! does not wake it. Once the timeout elapses, the `spawn_blocking` thread is
//! therefore **lost** until the kernel hands control back. That is why the
//! mount point is marked: subsequent calls return immediately, without
//! consuming a second one — otherwise changing tracks several times in a row on
//! the same silent share would lose a pool thread every time, never getting one
//! back.
//!
//! That abandoned thread is also the **only recovery detector**: when the
//! kernel finally releases it, it clears the mark.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Timeout granted to the extraction of an embedded cover.
///
/// Under the five seconds that `MpvIpc::command` already tolerates for an mpv
/// response: a stuck extraction must not become, on its own, the longest wait
/// the core loop can suffer.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// Tracks the responsiveness of the mount points traversed by the file
/// currently playing.
pub struct Health {
    /// Mount points from which a call never came back.
    unreachable: Arc<Mutex<HashSet<PathBuf>>>,
    timeout: Duration,
    /// Provider of `/proc/mounts`, injectable for tests.
    mounts: Box<dyn Fn() -> String + Send + Sync>,
}

impl Default for Health {
    fn default() -> Self {
        Self::new()
    }
}

impl Health {
    pub fn new() -> Self {
        Self {
            unreachable: Arc::new(Mutex::new(HashSet::new())),
            timeout: TIMEOUT,
            mounts: Box::new(|| std::fs::read_to_string("/proc/mounts").unwrap_or_default()),
        }
    }

    /// Test variant: short timeout and frozen `/proc/mounts`.
    #[cfg(test)]
    pub fn for_test(timeout: Duration, mounts: String) -> Self {
        Self { unreachable: Arc::new(Mutex::new(HashSet::new())), timeout, mounts: Box::new(move || mounts.clone()) }
    }

    /// Mount point owning `path`: the longest prefix in `mounts` that precedes
    /// it. Falls back to `path` itself if none matches (no privilege to read
    /// `/proc/mounts`, test environment) — for lack of anything better to
    /// attach a possible failure to.
    ///
    /// Unlike `ritornello-plugin-files::volumes::owner`, of which this is the
    /// full version, this module has no need to exclude pseudo filesystems
    /// (`proc`, `tmpfs`...): it only serves to group failures per mount, never
    /// to decide whether a path is browsable.
    fn owner(mounts: &str, path: &Path) -> PathBuf {
        mounts
            .lines()
            .filter_map(|l| {
                let mut c = l.split_whitespace();
                let _source = c.next()?;
                let point = c.next()?;
                Some(PathBuf::from(point.replace("\\040", " ").replace("\\011", "\t")))
            })
            .filter(|p| path.starts_with(p))
            .max_by_key(|p| p.as_os_str().len())
            .unwrap_or_else(|| path.to_path_buf())
    }

    /// Runs `f` off the async thread, under a timeout, on the account of the
    /// mount point owning `path`.
    ///
    /// Returns `None` without **executing anything** if that mount point is
    /// already known silent, `None` too if the timeout elapses or if `f`
    /// panics. A `None` therefore never says "no cover" on its own: it says
    /// "we don't know", which the caller treats anyway as "nothing to show",
    /// exactly like the absence of an image in the tags.
    pub async fn bounded<T, F>(&self, path: &Path, f: F) -> Option<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let key = Self::owner(&(self.mounts)(), path);
        if self.unreachable.lock().unwrap().contains(&key) {
            return None;
        }
        // `spawn_blocking` and not the current thread: even bounded, the call
        // must leave the async thread, otherwise it holds the whole rest of the
        // core loop for the entire timeout.
        let mut task = tokio::task::spawn_blocking(f);
        // `&mut task`: the `JoinHandle` remains ours after expiry, which lets us
        // hand the abandoned thread over to the watch task below.
        match tokio::time::timeout(self.timeout, &mut task).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::warn!("embedded cover extraction on {} failed: {e}", path.display());
                None
            }
            Err(_) => {
                tracing::warn!(
                    "{} did not answer within {:?}: treating its mount point {} as unresponsive",
                    path.display(),
                    self.timeout,
                    key.display()
                );
                self.unreachable.lock().unwrap().insert(key.clone());
                let unreachable = Arc::clone(&self.unreachable);
                tokio::spawn(async move {
                    // Waits for the lost thread. This task may never finish; it
                    // only costs one task, whereas retrying would cost a pool
                    // thread for every new track on the same share.
                    let _ = task.await;
                    tracing::info!("{} answers again", key.display());
                    unreachable.lock().unwrap().remove(&key);
                });
                None
            }
        }
    }

    /// Mount points currently silent. Reserved for tests: nothing displays
    /// this information anywhere else yet (unlike the `files` plugin, which
    /// shows it on its page).
    #[cfg(test)]
    pub fn silent(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.unreachable.lock().unwrap().iter().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "/dev/root / ext4 rw 0 0\n\
                          //192.168.1.15/musique /mnt/ritornello/nas cifs ro,soft 0 0\n";

    fn health() -> Health {
        Health::for_test(Duration::from_millis(50), MOUNTS.to_string())
    }

    #[tokio::test]
    async fn a_call_that_answers_returns_its_value() {
        let s = health();
        assert_eq!(s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || 7).await, Some(7));
        assert!(s.silent().is_empty(), "a call that returned must not mark anyone");
    }

    #[tokio::test]
    async fn a_call_that_never_returns_hands_back_control_and_marks_its_mount() {
        let s = health();
        let start = std::time::Instant::now();
        let r = s
            .bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || {
                std::thread::sleep(Duration::from_millis(400));
                7
            })
            .await;
        assert_eq!(r, None);
        // The bound is only worth its price if it hands back control *before*
        // the end of the call: without the measurement, a `None` could just as
        // well come from a call that simply failed after its 400 ms.
        assert!(start.elapsed() < Duration::from_millis(300), "{:?}", start.elapsed());
        assert_eq!(s.silent(), vec![PathBuf::from("/mnt/ritornello/nas")]);
    }

    #[tokio::test]
    async fn a_marked_mount_no_longer_consumes_a_thread() {
        let s = health();
        s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;

        // It is the *execution* that is forbidden, not just the result: every
        // call that ran would lose one more pool thread, and the pool is
        // finite. The flag proves the closure did not run.
        static RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        RAN.store(false, std::sync::atomic::Ordering::SeqCst);
        let r = s
            .bounded(Path::new("/mnt/ritornello/nas/autre/b.mp3"), || {
                RAN.store(true, std::sync::atomic::Ordering::SeqCst)
            })
            .await;
        assert_eq!(r, None);
        assert!(!RAN.load(std::sync::atomic::Ordering::SeqCst), "the second call should not have run");
    }

    #[tokio::test]
    async fn a_marked_mount_does_not_open_the_others() {
        let s = health();
        s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;
        // `/` is another mount: the sleeping NAS must not make local tracks
        // unreadable, which would be curing by amputation.
        assert_eq!(s.bounded(Path::new("/home/pi/musique/a.mp3"), || 7).await, Some(7));
    }

    #[tokio::test]
    async fn the_mark_clears_when_the_mount_answers_again() {
        let s = health();
        s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(150));
        })
        .await;
        assert!(!s.silent().is_empty(), "the mount must first be marked");

        // The abandoned thread eventually comes back; it, and it alone, reopens
        // the circuit breaker.
        for _ in 0..100 {
            if s.silent().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(s.silent().is_empty(), "the mark should have cleared by itself");
        assert_eq!(s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || 7).await, Some(7));
    }

    #[tokio::test]
    async fn without_a_known_mount_the_path_itself_is_the_key() {
        let s = Health::for_test(Duration::from_millis(50), String::new());
        s.bounded(Path::new("/home/pi/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(150));
        })
        .await;
        assert_eq!(s.silent(), vec![PathBuf::from("/home/pi/a.mp3")]);
    }
}
