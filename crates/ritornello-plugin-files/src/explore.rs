//! The state of the two source-declaration wizards.
//!
//! Extracted from `admin.rs`, which was reaching 800 lines: the wizard
//! operations would have formed a second topic there, unrelated to playlist
//! management.
//!
//! Since the admin protocol is request/response and pushes nothing, a network
//! connection cannot be awaited within the request: a powered-off NAS would
//! exceed the core's 5 s cap and the request would be killed before having
//! reported anything. `connect` and `browse` therefore spawn a task and return
//! immediately; the page follows progress by polling, exactly as for the scan.

use crate::smb::{self, Credentials};
use crate::{scan, volumes};
use ritornello_i18n::Catalog;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Cap on one `smbclient` call. Generous — a NAS waking up takes its time —
/// but finite: the page must always end up learning something.
const SMB_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Local,
    Smb,
}

/// What the page reads of the wizard in progress.
///
/// **Contains no credentials.** The guarantee is carried by the type, as for
/// `Root`: the serialized structure has no passphrase field, so there is
/// nothing to filter and nothing to forget to filter.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct View {
    pub open: bool,
    pub kind: Option<String>,
    pub host: String,
    pub share: String,
    pub path: String,
    pub shares: Vec<String>,
    pub dirs: Vec<String>,
    pub audio_count: usize,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct Browser {
    /// Where to place the transient `smbclient` authentication file.
    ///
    /// The **runtime** directory, never the one of the persisted credentials:
    /// the latter lives under `/etc` and is only writable in production, which
    /// made the wizard fail in development with a "Permission denied" that
    /// seemed to blame SMB.
    work_dir: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    smb_ok: Arc<AtomicBool>,
    view: Arc<Mutex<View>>,
    /// Credentials of the current dialog, indexed by host.
    ///
    /// In memory and **never serialized**: the passphrase crosses the wire
    /// once, at connection, and not on every click in the tree.
    sessions: Arc<Mutex<HashMap<String, Credentials>>>,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Circuit breaker of the media paths, shared with the Admin half.
    ///
    /// The local wizard reads the disk on every descent: a volume that does
    /// not respond must return a refusal, not jam the admin loop.
    health: Arc<crate::health::Health>,
}

impl Browser {
    pub fn new(
        work_dir: PathBuf,
        catalog: Arc<RwLock<Catalog>>,
        smb_ok: Arc<AtomicBool>,
        health: Arc<crate::health::Health>,
    ) -> Self {
        Self {
            work_dir,
            catalog,
            smb_ok,
            health,
            view: Arc::new(Mutex::new(View::default())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            task: None,
        }
    }

    fn phrase(&self, key: &str) -> String {
        self.catalog.read().unwrap().get(key).to_string()
    }

    pub fn open(&mut self, kind: Kind) {
        self.cancel();
        *self.view.lock().unwrap() = View {
            open: true,
            kind: Some(match kind {
                Kind::Local => "local".to_string(),
                Kind::Smb => "smb".to_string(),
            }),
            ..View::default()
        };
    }

    pub fn close(&mut self) {
        self.cancel();
        // The credentials die with the dialog: leaving them in memory would
        // let a passphrase outlive what collected it, with nothing ever
        // reclaiming it.
        self.sessions.lock().unwrap().clear();
        *self.view.lock().unwrap() = View::default();
    }

    fn cancel(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }

    pub fn credentials(&self, host: &str) -> Option<Credentials> {
        self.sessions.lock().unwrap().get(host).map(|c| Credentials {
            user: c.user.clone(),
            password: c.password.clone(),
            domain: c.domain.clone(),
        })
    }

    /// Contents of a folder on the device.
    ///
    /// Synchronous: a local file system answers well within the core's cap,
    /// and making this asynchronous would only add a polling round trip
    /// between each opened level.
    pub async fn local(&mut self, path: &str) -> Result<(), String> {
        let path_buf = std::path::PathBuf::from(path);
        let mounts = volumes::read_proc_mounts();
        // Canonicalization **and** listing under a single circuit breaker:
        // both touch the disk, and on a reconnecting volume neither returns.
        // A `None` here is a refusal, not an empty folder.
        let c = path_buf.clone();
        let Some(read) = self
            .health
            .bounded(&path_buf, move || {
                let canon = c.canonicalize().ok()?;
                Some((canon.clone(), scan::list_dir(&canon)))
            })
            .await
        else {
            return Err(self.phrase("root_unresponsive").replace("{path}", path));
        };
        let Some((canon, contents)) = read else {
            return Err(self.phrase("bad_local_path").replace("{path}", path));
        };
        if !volumes::browsable(&mounts, &canon) {
            return Err(self.phrase("bad_local_path").replace("{path}", path));
        }
        let contents = contents.map_err(|e| e.message(&self.catalog.read().unwrap()))?;
        let mut v = self.view.lock().unwrap();
        v.path = canon.display().to_string();
        v.dirs = contents.dirs;
        v.audio_count = contents.audio.len();
        v.error = None;
        v.busy = false;
        Ok(())
    }

    /// Connects to a host and enumerates its shares.
    pub fn connect(&mut self, host: String, user: String, password: String, domain: String) {
        self.cancel();
        if !user.is_empty() {
            self.sessions
                .lock()
                .unwrap()
                .insert(host.clone(), Credentials { user, password, domain });
        }
        if !self.smb_ok.load(Ordering::Relaxed) {
            self.failure(smb::SmbError::NotInstalled, &host);
            return;
        }
        {
            let mut v = self.view.lock().unwrap();
            v.host = host.clone();
            v.share = String::new();
            v.path = String::new();
            v.shares.clear();
            v.dirs.clear();
            v.busy = true;
            v.error = None;
        }
        let creds = self.credentials(&host);
        let dir = self.work_dir.clone();
        let view = self.view.clone();
        let catalog = self.catalog.clone();
        self.task = Some(tokio::spawn(async move {
            let r = smb::list_shares(&host, creds.as_ref(), &dir, SMB_TIMEOUT).await;
            let mut v = view.lock().unwrap();
            v.busy = false;
            match r {
                Ok(shares) => {
                    v.shares = shares;
                    v.error = None;
                }
                Err(e) => {
                    tracing::warn!("listing shares of {host}: {e}");
                    v.error = Some(e.message(&catalog.read().unwrap(), &host));
                }
            }
        }));
    }

    /// Goes back to the list of shares already obtained, **without
    /// reconnecting**.
    ///
    /// Distinct from `connect` on purpose: the shares are already known, and
    /// relaunching a network call to go back would make a navigation gesture
    /// that needs nothing wait — or even fail.
    ///
    /// Without this operation, once a share was chosen there was no way to try
    /// another one without closing the dialog.
    pub fn to_shares(&mut self) {
        self.cancel();
        let mut v = self.view.lock().unwrap();
        v.share = String::new();
        v.path = String::new();
        v.dirs.clear();
        v.audio_count = 0;
        v.busy = false;
        v.error = None;
    }

    /// Lists a folder of a share.
    pub fn browse(&mut self, share: String, path: String) {
        self.cancel();
        let host = self.view.lock().unwrap().host.clone();
        if !self.smb_ok.load(Ordering::Relaxed) {
            self.failure(smb::SmbError::NotInstalled, &host);
            return;
        }
        {
            let mut v = self.view.lock().unwrap();
            v.share = share.clone();
            v.path = path.clone();
            v.dirs.clear();
            v.audio_count = 0;
            v.busy = true;
            v.error = None;
        }
        let creds = self.credentials(&host);
        let dir = self.work_dir.clone();
        let view = self.view.clone();
        let catalog = self.catalog.clone();
        self.task = Some(tokio::spawn(async move {
            let r = smb::list_dir(&host, &share, &path, creds.as_ref(), &dir, SMB_TIMEOUT).await;
            let mut v = view.lock().unwrap();
            v.busy = false;
            match r {
                Ok(entries) => {
                    v.dirs = entries.iter().filter(|e| e.dir).map(|e| e.name.clone()).collect();
                    v.audio_count = entries
                        .iter()
                        .filter(|e| !e.dir && scan::is_audio(std::path::Path::new(&e.name)))
                        .count();
                    v.error = None;
                }
                Err(e) => {
                    tracing::warn!("listing //{host}/{share}/{path}: {e}");
                    v.error = Some(e.message(&catalog.read().unwrap(), &host));
                }
            }
        }));
    }

    fn failure(&self, e: smb::SmbError, host: &str) {
        let mut v = self.view.lock().unwrap();
        v.busy = false;
        v.error = Some(e.message(&self.catalog.read().unwrap(), host));
    }

    pub fn view(&self) -> serde_json::Value {
        serde_json::to_value(&*self.view.lock().unwrap()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volumes::fixture::divert_proc_mounts;
    use std::sync::atomic::AtomicBool;

    fn browser(dir: &std::path::Path) -> Browser {
        Browser::new(
            dir.join("creds"),
            Arc::new(std::sync::RwLock::new(Catalog::load(
                "files",
                "en",
                std::path::Path::new("/inexistant"),
                crate::FILES_EN,
            ))),
            Arc::new(AtomicBool::new(true)),
            Arc::new(crate::health::Health::new()),
        )
    }

    #[tokio::test]
    async fn the_password_appears_in_no_view() {
        // It has no reason to travel back to the browser: the page sent it
        // once, it does not need to read it back to display a tree of dirs.
        let dir = tempfile::tempdir().unwrap();
        let mut e = browser(dir.path());
        e.open(Kind::Smb);
        e.connect("nas".into(), "steven".into(), "secret-du-nas".into(), String::new());
        let text = serde_json::to_string(&e.view()).unwrap();
        assert!(!text.contains("secret-du-nas"), "{text}");
        assert!(!text.contains("password"), "{text}");
    }

    #[tokio::test]
    async fn closing_clears_the_session() {
        // Otherwise a passphrase would outlive in memory the dialog that
        // collected it, with nothing ever reclaiming it.
        let dir = tempfile::tempdir().unwrap();
        let mut e = browser(dir.path());
        e.open(Kind::Smb);
        e.connect("nas".into(), "steven".into(), "secret".into(), String::new());
        assert!(e.credentials("nas").is_some());
        e.close();
        assert!(e.credentials("nas").is_none());
    }

    #[tokio::test]
    async fn a_local_path_outside_any_volume_is_refused() {
        // The browsing guard. Without it, the page would address /proc/self
        // and the tree would wander into the recursive links.
        let dir = tempfile::tempdir().unwrap();
        let _guard =
            divert_proc_mounts(dir.path(), "proc /proc proc rw 0 0\n/dev/sda1 / ext4 rw 0 0\n");
        let mut e = browser(dir.path());
        e.open(Kind::Local);
        let err = e.local("/proc/self").await.unwrap_err();
        assert!(err.contains(' '), "raw key: {err}");
    }

    #[tokio::test]
    async fn a_local_folder_returns_its_subfolders_and_its_audio_count() {
        // The audio file count is what says we are in the right place:
        // without it one picks a folder hoping.
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir_all(media.join("Album")).unwrap();
        std::fs::write(media.join("a.mp3"), b"").unwrap();
        std::fs::write(media.join("b.flac"), b"").unwrap();
        std::fs::write(media.join("notes.txt"), b"").unwrap();
        let _guard = divert_proc_mounts(
            dir.path(),
            &format!("/dev/sda1 {} ext4 rw 0 0\n", dir.path().display()),
        );
        let mut e = browser(dir.path());
        e.open(Kind::Local);
        e.local(&media.display().to_string()).await.unwrap();
        let v = e.view();
        assert_eq!(v["dirs"], serde_json::json!(["Album"]));
        assert_eq!(v["audio_count"], 2, "notes.txt is not an audio file");
    }
}
