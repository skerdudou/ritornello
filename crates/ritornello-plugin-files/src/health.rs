//! Circuit breaker for media paths: bounds every system call that may never
//! return, and remembers the mount points that have stopped answering.
//!
//! # Why this bound exists
//!
//! The Admin half serves its requests **serially** and the core gives up on
//! them after five seconds. A single system call that never completes therefore
//! wedges the whole plugin, page included. Measured on 2026-08-17 on the device:
//! a blocked cifs mount made even `ui.js` time out, although it is nothing but
//! an `include_str!` with no lock and no I/O — the loop was already held up by
//! the previous request.
//!
//! # Why it cannot be tuned at mount time
//!
//! `mount.cifs` already receives `soft` (see `mount_options`, where a test pins
//! it). `soft` bounds the retries of an operation on an **established** session,
//! not the reconnection, which can last minutes. No cifs setting brings the
//! worst case under the core's five seconds: the bound has to live on the
//! caller's side.
//!
//! # Why an abandoned thread, and why only one
//!
//! A system call in uninterruptible sleep cannot be killed — even `SIGKILL`
//! does not wake it. Once the timeout has elapsed, the `spawn_blocking` thread
//! is therefore **lost** until the kernel hands control back. That is why the
//! mount point gets marked: subsequent calls return immediately, without
//! consuming a second thread. At most one abandoned thread per mount point.
//!
//! That abandoned thread is also the **only recovery detector**: when the
//! kernel finally releases it, it clears the mark. Probing again to find out
//! whether the mount answers would cost one more thread per attempt.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::volumes;

/// Time granted to a system call on a media path.
///
/// Well under the core's five seconds: a `get_data` that hits a silent mount
/// must be handed back within the timeout, with margin left for the rest of
/// the response.
pub const TIMEOUT: Duration = Duration::from_millis(1500);

/// Tracks the responsiveness of the mount points traversed by media paths.
pub struct Health {
    /// Mount points for which a probe never came back.
    unreachable: Arc<Mutex<HashSet<PathBuf>>>,
    timeout: Duration,
    /// Provider of `/proc/mounts`, injectable for tests — same approach as
    /// `volumes::read_proc_mounts`, which it calls by default.
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
            mounts: Box::new(volumes::read_proc_mounts),
        }
    }

    /// Test variant: short timeout, frozen `/proc/mounts`, and a list of mount
    /// points already considered silent.
    ///
    /// Public, and not behind `#[cfg(test)]`: the Admin half's tests live in
    /// the binary, which consumes this library compiled **without**
    /// `cfg(test)`. A shortcut hidden there would be invisible to them, and the
    /// Admin half is precisely the one that must be put in front of a silent
    /// mount without having one at hand.
    pub fn for_test(timeout: Duration, mounts: String, silent: Vec<PathBuf>) -> Self {
        Self {
            unreachable: Arc::new(Mutex::new(silent.into_iter().collect())),
            timeout,
            mounts: Box::new(move || mounts.clone()),
        }
    }

    /// Mount point owning `path`, the circuit breaker's key.
    ///
    /// Blocking is a property of the **mount**, not of the declared root: two
    /// roots on the same share go down together, and a path picked in the
    /// wizard is covered without being declared anywhere.
    fn key(mounts: &str, path: &Path) -> PathBuf {
        volumes::owner(mounts, path)
            .map(|v| v.path)
            // No owning mount: the path itself makes an honest key, for want
            // of anything better to attach the failure to.
            .unwrap_or_else(|| path.to_path_buf())
    }

    /// True if a probe on this mount point never came back.
    pub fn unreachable(&self, path: &Path) -> bool {
        let key = Self::key(&(self.mounts)(), path);
        self.unreachable.lock().unwrap().contains(&key)
    }

    /// Mount points currently silent, so the page can say so.
    pub fn silent(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.unreachable.lock().unwrap().iter().cloned().collect();
        v.sort();
        v
    }

    /// Runs `f` off the async thread, under a timeout, charged to the mount
    /// point owning `path`.
    ///
    /// Returns `None` without **running anything** if that mount point is
    /// already known to be silent, `None` as well if the timeout elapses or if
    /// `f` panics. A `None` therefore never means "absent": it means "unknown",
    /// which the caller must report as such rather than turn into a fact.
    pub async fn bounded<T, F>(&self, path: &Path, f: F) -> Option<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let key = Self::key(&(self.mounts)(), path);
        if self.unreachable.lock().unwrap().contains(&key) {
            return None;
        }
        // `spawn_blocking` and not the current thread: even bounded, the call
        // must leave the async thread, otherwise it holds up the runtime's
        // other tasks for the whole timeout.
        let mut task = tokio::task::spawn_blocking(f);
        // `&mut task`: the `JoinHandle` stays ours after expiry, which lets us
        // hand it over to the watcher below.
        match tokio::time::timeout(self.timeout, &mut task).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::warn!("probe of {} failed: {e}", path.display());
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
                    // Waits for the lost thread. This task may never finish;
                    // it costs only one task, where reprobing would cost a
                    // pool thread per attempt.
                    let _ = task.await;
                    tracing::info!("{} answers again", key.display());
                    unreachable.lock().unwrap().remove(&key);
                });
                None
            }
        }
    }

    /// Groups the indices of `paths` by owning mount point.
    ///
    /// Grouping before acting is what makes the bound affordable: a single
    /// timeout covers every track of a given share. Without it, a playlist of
    /// two thousand tracks on a silent share would cost two thousand timeouts.
    ///
    /// The result is sorted by mount point: for the same payload, the page
    /// must receive the same thing from one poll to the next.
    pub fn group(&self, paths: &[PathBuf]) -> Vec<(PathBuf, Vec<usize>)> {
        let mounts = (self.mounts)();
        let mut groups: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for (i, c) in paths.iter().enumerate() {
            groups.entry(Self::key(&mounts, c)).or_default().push(i);
        }
        let mut v: Vec<(PathBuf, Vec<usize>)> = groups.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Says, for each path, whether it is missing — `None` when its mount
    /// point does not answer.
    ///
    /// `None` and not `true`: showing "not found" for a sleeping share would
    /// blame the files for a failure that belongs to the mount, and send
    /// people looking for the fault in the wrong place.
    ///
    /// Paths are grouped by mount point: a single timeout covers every track
    /// of a given share, instead of one per track.
    pub async fn missing(&self, paths: &[PathBuf]) -> Vec<Option<bool>> {
        let mut out = vec![None; paths.len()];
        for (_, indices) in self.group(paths) {
            let batch: Vec<PathBuf> = indices.iter().map(|&i| paths[i].clone()).collect();
            let anchor = batch[0].clone();
            let measure =
                self.bounded(&anchor, move || batch.iter().map(|p| !p.is_file()).collect::<Vec<_>>());
            if let Some(v) = measure.await {
                for (n, &i) in indices.iter().enumerate() {
                    out[i] = v.get(n).copied();
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Two distinct mounts, so that one's circuit breaker does not open the
    /// other's.
    const MOUNTS: &str = "/dev/root / ext4 rw 0 0\n\
                          //192.168.1.15/musique /mnt/ritornello/nas cifs ro,soft 0 0\n";

    fn health() -> Health {
        Health::for_test(Duration::from_millis(50), MOUNTS.to_string(), Vec::new())
    }

    #[tokio::test]
    async fn a_call_that_answers_returns_its_value() {
        let s = health();
        assert_eq!(s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || 7).await, Some(7));
        assert!(s.silent().is_empty(), "a call that returned must not mark anyone");
    }

    /// The call **never** returns on its own: it blocks on a channel that the
    /// test only releases at the end.
    ///
    /// The property guarded is the same as before — the bound is only worth
    /// its price if it hands control back *before* the call ends — but it used
    /// to be proven by a wall-clock margin: 300 ms measured against a 50 ms
    /// timeout and a 400 ms call. A fast-execution assumption, hence a flake as
    /// soon as the other test binaries load the machine.
    ///
    /// A call that does not finish until allowed to makes the `None` true **by
    /// construction**: no load can let the call win the race, where a 400 ms
    /// `sleep` could. The test's `timeout` now only guards the blatant
    /// regression — a bound that waited for the call instead of bounding it
    /// would hang this test, and this line punishes that with a message rather
    /// than by timing out silently.
    ///
    /// Not to be replaced by `tokio::time::pause()`: measured, the virtual
    /// clock does not advance while a `spawn_blocking` task is in flight, so
    /// the call won and the assertion flipped to `Some(7)`.
    #[tokio::test]
    async fn a_call_that_never_returns_hands_back_control_and_marks_its_mount() {
        let s = health();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let r = tokio::time::timeout(
            Duration::from_secs(10),
            s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), move || {
                let _ = wait.recv();
                7
            }),
        )
        .await
        .expect("the bound must hand back control at its timeout, not wait for the call");
        assert_eq!(r, None);
        assert_eq!(s.silent(), vec![PathBuf::from("/mnt/ritornello/nas")]);
        // Release the blocking thread, otherwise the runtime shutdown would wait for it.
        let _ = release.send(());
    }

    #[tokio::test]
    async fn a_marked_mount_consumes_no_more_threads() {
        let s = health();
        s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;

        // It is the *execution* that is forbidden, not only the result: every
        // call that ran would lose one more pool thread, and the pool is
        // finite. The flag proves the closure did not run.
        static RAN: AtomicBool = AtomicBool::new(false);
        RAN.store(false, Ordering::SeqCst);
        let r = s
            .bounded(Path::new("/mnt/ritornello/nas/autre/b.mp3"), || {
                RAN.store(true, Ordering::SeqCst)
            })
            .await;
        assert_eq!(r, None);
        assert!(!RAN.load(Ordering::SeqCst), "the second call should not have run");
    }

    #[tokio::test]
    async fn a_marked_mount_does_not_open_the_others() {
        let s = health();
        s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;
        // `/` is another mount: the sleeping NAS must not make the local
        // sources unusable, which would be curing by amputation.
        assert_eq!(s.bounded(Path::new("/home/pi/musique/a.mp3"), || 7).await, Some(7));
    }

    #[tokio::test]
    async fn the_mark_clears_when_the_mount_answers_again() {
        let s = health();
        s.bounded(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(150));
        })
        .await;
        assert!(!s.silent().is_empty(), "the mount must be marked first");

        // The abandoned thread eventually comes back; it, and it alone,
        // closes the circuit breaker again.
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
    async fn missing_distinguishes_absent_from_undetermined() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present.mp3");
        std::fs::write(&present, b"x").unwrap();
        let absent = dir.path().join("absent.mp3");

        // The temporary directory's mount is described as `/`, the NAS one
        // stays separate: the answer must be known for the first two and
        // undetermined for the third.
        let s = Health::for_test(
            Duration::from_millis(50),
            MOUNTS.to_string(),
            vec![PathBuf::from("/mnt/ritornello/nas")],
        );

        let r = s
            .missing(&[
                present.clone(),
                absent.clone(),
                PathBuf::from("/mnt/ritornello/nas/c.mp3"),
            ])
            .await;
        assert_eq!(r, vec![Some(false), Some(true), None]);
    }
}
