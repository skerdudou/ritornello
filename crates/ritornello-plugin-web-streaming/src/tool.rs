//! yt-dlp, installed and kept up to date by the plugin itself.
//!
//! This is the one place where the product downloads and runs code that no
//! release of ours shipped, so nothing is put in place unless yt-dlp's own
//! signature vouches for it ([`verify`]), nothing replaces a newer copy with
//! an older one unasked, and nothing in place is lost when a step fails
//! ([`install`]).
//!
//! The same place keeps the fallback's JavaScript runtime ([`runtime`]):
//! deno on x86_64 and aarch64, installed once, checked against its
//! published sum only; node 22 on armv7, checked against Node.js's own
//! release signature and updated whenever its signed list names another
//! version, until node 22's end of life (2027-04-30), after which the last
//! one is kept as it is.
//!
//! The layout under the plugin's data directory:
//!
//! - `yt-dlp/`       the extracted zipapp (`yt_dlp`, `yt_dlp_ejs`), with
//!   its bytecode cache;
//! - `bin/yt-dlp`    the wrapper ([`wrapper`]): what mpv and the plugin run;
//! - `bin/deno`      the plugin's own deno, on x86_64 and aarch64 only;
//! - `bin/node`      the plugin's own node 22, on armv7 only (`node.new`
//!   while a new one is checked);
//! - `yt-dlp.conf`   handed to yt-dlp by the wrapper;
//! - `cache/`        yt-dlp's cache;
//! - `settings.toml` the channel, the last check, what is installed.
//!
//! When: [`schedule`] runs at start (probe the machine, rewrite the wrapper
//! and the conf, so a new plugin version brings its own), then every hour
//! [`Tool::update`] acts if nothing is installed or the last check is more
//! than a day old. The page asks for a forced update with
//! [`Tool::request_update`], which returns at once: the scheduler runs it and
//! the page follows `updating` and `error` in [`Tool::status`].
//!
//! The swap waits for the download job in progress ([`ToolGate`]): a job
//! never sees yt-dlp change under it. Nothing else waits for that gate: a
//! route must never hang on a job that may last hours. What the gate does
//! not cover: a playback (mpv runs the wrapper for a few seconds) or a probe
//! of the page that starts during the swap's two renames can fail once, and
//! a Python process that spans the swap may import a module it had not
//! loaded yet from the new tree. The window is two renames.

pub mod fetch;
pub mod install;
pub mod runtime;
pub mod verify;
pub mod wrapper;

#[cfg(test)]
pub(crate) mod testkit;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pgp::composed::SignedPublicKey;
use serde::Serialize;

use crate::settings::{Channel, InstalledRecord, Settings};
use fetch::{Fetch, Limit};
use runtime::Capabilities;
use verify::VerifyError;

/// The release asset installed, on every architecture: the zipapp (there is
/// no bare armv7l binary, and the zipped one starts slower than the
/// extracted zipapp; measured).
pub const ASSET: &str = "yt-dlp";
pub const SUMS: &str = "SHA2-256SUMS";
pub const SIG: &str = "SHA2-256SUMS.sig";

/// A day, in seconds: the most often an unforced check goes to the network.
pub const DAY: u64 = 24 * 60 * 60;

/// Held for reading by the download job in progress, for writing by the
/// swap: an update waits for the job, and the next job waits for the swap.
pub type ToolGate = Arc<tokio::sync::RwLock<()>>;

/// Unix seconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

const SMALL: Limit = Limit { max_bytes: 1 << 20, deadline: Duration::from_secs(30) };
const ASSET_LIMIT: Limit = Limit { max_bytes: 64 << 20, deadline: Duration::from_secs(300) };
const LATEST: Duration = Duration::from_secs(30);
/// The first start compiles the bytecode: 5.2 s on the device (measured).
const WARM_UP: Duration = Duration::from_secs(120);
/// `yt_dlp/version.py` of the real zipapp is a few hundred bytes.
const VERSION_PY_MAX: u64 = 64 << 10;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Installed {
    pub version: String,
    pub channel: Channel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Not due (checked less than a day ago), nothing asked.
    Skipped,
    /// The signed list names what is already installed.
    Current(Installed),
    Installed(Installed),
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateError {
    /// Another update is running.
    Busy,
    Python3Missing,
    /// python3 is there, but its path cannot be a shebang's interpreter.
    Python3Unusable(String),
    /// The network, before any verdict: tried again at the next tick.
    Fetch(String),
    /// The release is not the one yt-dlp's key vouches for.
    Refused(VerifyError),
    /// Signed, but older than what is installed, or another build under the
    /// installed number: only a forced update or a change of channel takes
    /// it.
    Rollback(String),
    /// Verified, but it would not unpack or run: the old copy stays.
    Install(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Busy => write!(f, "an update is already running"),
            UpdateError::Python3Missing => write!(f, "python3 missing"),
            UpdateError::Python3Unusable(e) => write!(f, "python3 unusable: {e}"),
            UpdateError::Fetch(e) => write!(f, "download failed: {e}"),
            UpdateError::Refused(e) => write!(f, "refused: {e}"),
            UpdateError::Rollback(e) => write!(f, "refused: {e} (a forced update takes it)"),
            UpdateError::Install(e) => write!(f, "install failed: {e}"),
        }
    }
}

/// An update failure as the page receives it: a code the page translates,
/// and parameters: versions the plugin knows, or the raw text of an outside
/// voice (`detail`: the network, the system, python).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateFault {
    pub code: &'static str,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub params: std::collections::BTreeMap<String, String>,
}

impl UpdateError {
    /// One code per way an update fails. `Rollback` carries the plugin's own
    /// sentence (see [`judge_rollback`]): its three shapes are read back here,
    /// next to where they are written, and a test holds the two together.
    pub fn fault(&self) -> UpdateFault {
        let with = fault_with;
        match self {
            UpdateError::Busy => with("busy", &[]),
            UpdateError::Python3Missing => with("python3_missing", &[]),
            UpdateError::Python3Unusable(e) => with("python3_unusable", &[("detail", e)]),
            UpdateError::Fetch(e) => with("fetch", &[("detail", e)]),
            UpdateError::Refused(VerifyError::BadSignature(why)) => with("refused_bad_signature", &[("detail", why)]),
            UpdateError::Refused(VerifyError::NotListed) => with("refused_not_listed", &[]),
            UpdateError::Refused(VerifyError::Mismatch) => with("refused_mismatch", &[]),
            UpdateError::Rollback(e) => {
                if let Some((offered, installed)) = e.split_once(OLDER) {
                    with("rollback_older", &[("offered", offered), ("installed", installed)])
                } else if let Some(offered) = e.strip_prefix(OTHER_BUILD_1).and_then(|r| r.strip_suffix(OTHER_BUILD_2)) {
                    with("rollback_other_build", &[("offered", offered)])
                } else if let Some(installed) = e.strip_prefix(UNREADABLE) {
                    with("rollback_unreadable", &[("installed", installed)])
                } else {
                    with("rollback_other", &[("detail", e)])
                }
            }
            UpdateError::Install(e) => install_fault(e),
        }
    }
}

pub(crate) fn fault_with(code: &'static str, pairs: &[(&str, &str)]) -> UpdateFault {
    UpdateFault { code, params: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
}

// The sentences the install writes itself, which `install_fault` reads back.
const NO_PACKAGE: &str = "the archive holds no yt_dlp package";
const DOES_NOT_START: &str = "it does not start: ";
const NO_VERSION: &str = "it answers no version: ";
const SWAP: &str = "swap: ";
const WRITE: &str = "wrapper: ";

/// A failed install as a code. What the plugin wrote is a code of its own;
/// what python, yt-dlp or the system said stays raw, as `detail`. The unpacking
/// (`install::extract`) writes its own sentences too: its two limits and its
/// refusals are recognised by their shape, and a test builds each one for real.
fn install_fault(e: &str) -> UpdateFault {
    if e == NO_PACKAGE {
        fault_with("install_no_package", &[])
    } else if let Some(d) = e.strip_prefix(DOES_NOT_START) {
        fault_with("install_does_not_start", &[("detail", d)])
    } else if let Some(d) = e.strip_prefix(NO_VERSION) {
        fault_with("install_no_version", &[("detail", d)])
    } else if let Some(d) = e.strip_prefix(SWAP) {
        fault_with("install_swap", &[("detail", d)])
    } else if let Some(d) = e.strip_prefix(WRITE) {
        if d == "python3 missing" { fault_with("python3_missing", &[]) } else { fault_with("install_write", &[("detail", d)]) }
    } else if let Some(d) = e.strip_prefix("not a zip archive: ") {
        fault_with("install_not_zip", &[("detail", d)])
    } else if e.contains(" entries, more than ") || e.contains(" bytes unpacked") || e.contains(": more than ") {
        fault_with("install_extract_limit", &[])
    } else if e.starts_with("unsafe name ") || e.ends_with(" is a link") {
        fault_with("install_extract_unsafe", &[])
    } else {
        // The system's own words (a path and an error), or a sentence of a
        // newer plugin.
        fault_with("install", &[("detail", e)])
    }
}

/// What the page shows of the tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub installed: bool,
    pub version: Option<String>,
    pub channel: Channel,
    /// An update was asked for or runs.
    pub updating: bool,
    /// The update is ready and waits for the download job in progress.
    pub waiting_for_jobs: bool,
    /// The last update's failure in English, for the log and the tests; the
    /// page gets `error_code` and translates it.
    #[serde(skip)]
    pub error: Option<String>,
    /// The same failure as a code the page translates, with the raw text of
    /// an outside voice (the network, the system) as a parameter.
    pub error_code: Option<UpdateFault>,
    /// The failure above is a refusal a forced update takes (an older
    /// release, another build of the same number): the page offers "install
    /// anyway" only then. A bad signature is never one.
    pub forceable: bool,
    pub last_check: Option<u64>,
    /// The last failure to install or update the plugin's own runtime (deno,
    /// or node on armv7), in English (log and tests); the page gets
    /// `runtime_error_code`.
    #[serde(skip)]
    pub runtime_error: Option<String>,
    pub runtime_error_code: Option<UpdateFault>,
}

/// Everything the tool depends on from the outside, so tests can replace it.
pub struct Deps {
    pub fetch: Arc<dyn Fetch>,
    pub now: Clock,
    pub search: Vec<PathBuf>,
    pub arch: String,
    pub gate: ToolGate,
    /// The key the signed list must verify with: yt-dlp's, but for tests.
    pub key: SignedPublicKey,
    /// The keys node's signed list must verify with, on armv7: Node.js's
    /// release keys, but for tests.
    pub node_keys: Vec<SignedPublicKey>,
}

#[derive(Default)]
struct Flags {
    /// A request of the page the scheduler has not run yet: `Some(force)`.
    requested: Option<bool>,
    updating: bool,
    waiting_for_jobs: bool,
    error: Option<String>,
    error_code: Option<UpdateFault>,
    forceable: bool,
    runtime_error: Option<runtime::RuntimeError>,
    runtime_failed_at: Option<u64>,
}

struct Inner {
    data_dir: PathBuf,
    deps: Deps,
    settings: Mutex<Settings>,
    flags: Mutex<Flags>,
    caps: Mutex<Capabilities>,
    one_at_a_time: tokio::sync::Mutex<()>,
    runtime_one_at_a_time: tokio::sync::Mutex<()>,
    requests: tokio::sync::Notify,
    /// Serialises the writes of the wrapper and the conf.
    files: Mutex<()>,
}

#[derive(Clone)]
pub struct Tool {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Clears the "updating" flags however the update ends, a dropped future
/// included.
struct UpdatingGuard<'a>(&'a Inner);

impl Drop for UpdatingGuard<'_> {
    fn drop(&mut self) {
        let mut f = lock(&self.0.flags);
        f.updating = false;
        f.waiting_for_jobs = false;
    }
}

/// The version `yt_dlp/version.py` declares (`__version__ = '2026.10.26'`,
/// or annotated, `__version__: str = '…'`), read as text: nothing of the
/// archive runs before it is judged.
pub fn declared_version(version_py: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(version_py).ok()?;
    let value = text.lines().find_map(|l| {
        let (name, value) = l.split_once('=')?;
        let name = name.split_once(':').map_or(name, |(n, _)| n);
        (name.trim() == "__version__").then(|| value.trim())
    })?;
    let unquoted = value
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
        .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))?;
    let ok = !unquoted.is_empty()
        && unquoted.len() <= 64
        && unquoted.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        && !runtime::numbers(unquoted).is_empty();
    ok.then(|| unquoted.to_string())
}

/// Whether a signed release (`offered`, its declared version when it could
/// be read; `sha256`) may replace what `installed` records. Refused, on the
/// same channel and unforced: an older version (a rollback, or a frozen
/// mirror replaying an old release), another build under the installed
/// number (a re-signed release is not something the plugin decides to trust
/// on its own), and a version that cannot be read (a comparison is due and
/// cannot be made). The operator's forced update overrides all three.
///
/// Nothing is protected when nothing works, and no comparison is due: no
/// record (a first install, a data directory deleted), an install that is
/// not intact, a forced update or a change of channel take any signed
/// release, readable `version.py` or not, so a format change in yt-dlp can
/// never freeze a fresh device.
const UNREADABLE: &str = "the release's version cannot be read to compare it with the installed ";
const OLDER: &str = " is older than the installed ";
const OTHER_BUILD_1: &str = "another build of ";
const OTHER_BUILD_2: &str = " than the one installed";

pub fn judge_rollback(
    offered: Option<&str>,
    sha256: &str,
    installed: Option<&InstalledRecord>,
    intact: bool,
    channel: Channel,
    force: bool,
) -> Result<(), String> {
    let Some(r) = installed else { return Ok(()) };
    if !intact || force || r.channel != channel {
        return Ok(());
    }
    let Some(offered) = offered else {
        return Err(format!("{UNREADABLE}{}", r.version));
    };
    match runtime::numbers(offered).cmp(&runtime::numbers(&r.version)) {
        std::cmp::Ordering::Less => Err(format!("{offered}{OLDER}{}", r.version)),
        std::cmp::Ordering::Equal if r.sha256 != sha256 => Err(format!("{OTHER_BUILD_1}{offered}{OTHER_BUILD_2}")),
        _ => Ok(()),
    }
}

impl Tool {
    /// The real thing: GitHub and nodejs.org over https, the system clock,
    /// `PATH`, this machine, the embedded keys of yt-dlp and Node.js.
    pub fn new(data_dir: &Path, gate: ToolGate) -> anyhow::Result<Self> {
        let search = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()))
            .collect();
        let deps = Deps {
            fetch: Arc::new(fetch::Https::default()),
            now: Arc::new(|| {
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
            }),
            search,
            arch: std::env::consts::ARCH.to_string(),
            gate,
            key: verify::ytdlp_key().map_err(|e| anyhow::anyhow!("the embedded yt-dlp key: {e}"))?,
            node_keys: verify::node_keys().map_err(|e| anyhow::anyhow!("the embedded Node.js keys: {e}"))?,
        };
        Ok(Self::with_deps(data_dir, deps))
    }

    /// Rolls back an update a crash interrupted, then reads the settings.
    pub fn with_deps(data_dir: &Path, deps: Deps) -> Self {
        install::recover(data_dir);
        let settings = Settings::load(&data_dir.join("settings.toml"));
        Self {
            inner: Arc::new(Inner {
                data_dir: data_dir.to_path_buf(),
                deps,
                settings: Mutex::new(settings),
                flags: Mutex::new(Flags::default()),
                caps: Mutex::new(Capabilities::default()),
                one_at_a_time: tokio::sync::Mutex::new(()),
                runtime_one_at_a_time: tokio::sync::Mutex::new(()),
                requests: tokio::sync::Notify::new(),
                files: Mutex::new(()),
            }),
        }
    }

    fn now(&self) -> u64 {
        (self.inner.deps.now)()
    }

    fn settings_path(&self) -> PathBuf {
        self.inner.data_dir.join("settings.toml")
    }

    fn wrapper_path(&self) -> PathBuf {
        crate::ytdlp::Ytdlp::new(&self.inner.data_dir).bin().to_path_buf()
    }

    fn installed_now(&self) -> bool {
        self.wrapper_path().is_file() && install::intact(&self.inner.data_dir)
    }

    pub fn status(&self) -> Status {
        let s = lock(&self.inner.settings).clone();
        let f = lock(&self.inner.flags);
        Status {
            installed: self.installed_now(),
            version: s.installed.map(|i| i.version),
            channel: s.channel,
            updating: f.updating || f.requested.is_some(),
            waiting_for_jobs: f.waiting_for_jobs,
            error: f.error.clone(),
            error_code: f.error_code.clone(),
            forceable: f.forceable,
            last_check: s.last_check,
            runtime_error: f.runtime_error.as_ref().map(ToString::to_string),
            runtime_error_code: f.runtime_error.as_ref().map(runtime::RuntimeError::fault),
        }
    }

    pub fn settings(&self) -> Settings {
        lock(&self.inner.settings).clone()
    }

    /// Changes the settings and writes them (off any admin route's thread:
    /// the caller is a task).
    pub fn edit_settings(&self, edit: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
        let mut s = lock(&self.inner.settings);
        let mut next = s.clone();
        edit(&mut next);
        next.save(&self.settings_path())?;
        *s = next;
        Ok(())
    }

    /// The capabilities of the last probe (cheap), dated by the clock: past
    /// node 22's end of life, the plugin's own node is said unmaintained.
    pub fn capabilities(&self) -> Capabilities {
        let caps = lock(&self.inner.caps).clone();
        self.dated(caps)
    }

    fn dated(&self, mut caps: Capabilities) -> Capabilities {
        caps.js_runtime_unmaintained = runtime::unmaintained(&caps, self.now());
        caps
    }

    fn runtime_env(&self) -> runtime::Env {
        runtime::Env {
            search: self.inner.deps.search.clone(),
            data_dir: self.inner.data_dir.clone(),
            arch: self.inner.deps.arch.clone(),
        }
    }

    /// Probes the machine again (a few short processes, each under a
    /// deadline); rewrites the wrapper when what it is written with changed.
    /// Never waits for a download job: the rewrite is an atomic rename, a
    /// wrapper already running keeps the file it started from, and a job
    /// picks the new one up at its next video.
    pub async fn reprobe(&self) -> Capabilities {
        let caps = runtime::detect(&self.runtime_env()).await;
        let changed = {
            let mut c = lock(&self.inner.caps);
            let changed = c.runtime_arg != caps.runtime_arg || c.python3_path != caps.python3_path;
            *c = caps.clone();
            changed
        };
        if changed
            && self.installed_now()
            && let Err(e) = self.write_wrapper(&caps)
        {
            tracing::warn!("yt-dlp wrapper not rewritten: {e}");
        }
        self.dated(caps)
    }

    /// The page's "Check again": a [`reprobe`](Self::reprobe), then, when
    /// python3 is there and nothing is installed yet, the install requested
    /// at once (unforced): the operator who just installed python3 would
    /// otherwise see "not installed yet" until the hourly tick.
    pub async fn recheck(&self) -> Capabilities {
        let caps = self.reprobe().await;
        if caps.python3_path.is_some() && !self.installed_now() {
            self.request_update(false);
        }
        caps
    }

    /// At start: probes, then rewrites the wrapper and the conf of an
    /// installed yt-dlp whatever they hold, so a new version of the plugin
    /// brings its own (atomic renames: a playback running the old wrapper
    /// keeps it to the end).
    pub async fn start(&self) {
        let caps = self.reprobe().await;
        if self.installed_now()
            && let Err(e) = self.write_conf().and_then(|()| self.write_wrapper(&caps))
        {
            tracing::warn!("yt-dlp wrapper or conf not refreshed: {e}");
        }
    }

    /// Serialised with the conf's writes: start, a reprobe (the page's, or
    /// deno's) and an install may all rewrite them at once.
    fn write_wrapper(&self, caps: &Capabilities) -> Result<(), String> {
        let python3 = caps.python3_path.as_deref().ok_or("python3 missing")?;
        let text = wrapper::wrapper(&self.inner.data_dir, python3, caps.runtime_arg.as_deref())?;
        let _one = lock(&self.inner.files);
        install::write_executable(&self.wrapper_path(), text.as_bytes()).map_err(|e| e.to_string())
    }

    fn write_conf(&self) -> Result<(), String> {
        let conf = wrapper::conf(&self.inner.data_dir)?;
        let _one = lock(&self.inner.files);
        crate::entries::write_atomically(&self.inner.data_dir.join(wrapper::CONF), conf.as_bytes())
            .map_err(|e| format!("{e:#}"))
    }

    /// Nothing installed, or not checked for a day. A last check in the
    /// future (a clock set back, a Pi without a battery-backed clock) counts
    /// as stale.
    pub fn due(&self) -> bool {
        if !self.installed_now() {
            return true;
        }
        let now = self.now();
        let last = lock(&self.inner.settings).last_check;
        last.is_none_or(|t| t > now || now - t >= DAY)
    }

    /// What the page calls, for its two actions; returns at once
    /// (`status().updating` turns true) and the scheduler runs it:
    /// - `force = false`, "check now": past the daily limit, the rollback
    ///   guard still on;
    /// - `force = true`, "accept anyway": also takes an older release,
    ///   another build of the installed number, or one whose version cannot
    ///   be read.
    ///
    /// Two requests before the scheduler runs make one, forced if either
    /// was. Never call [`update`](Self::update) from a route: its swap waits
    /// for the running download job, which may take hours.
    pub fn request_update(&self, force: bool) {
        {
            let mut f = lock(&self.inner.flags);
            f.requested = Some(f.requested.unwrap_or(false) || force);
        }
        self.inner.requests.notify_one();
    }

    /// [`update_with`](Self::update_with)`(force, force)`: the hourly tick
    /// is `update(false)`.
    pub async fn update(&self, force: bool) -> Result<Update, UpdateError> {
        self.update_with(force, force).await
    }

    /// Checks the channel of the settings and installs its latest release
    /// when it differs from what is installed. Only when [`due`](Self::due)
    /// unless `check_now`; the rollback guard yields only to
    /// `accept_anyway`. One at a time. Then, outside that section, the
    /// plugin's own runtime where the machine takes one: deno (x86_64,
    /// aarch64), or node (armv7), checked at the same pace as yt-dlp.
    pub async fn update_with(&self, check_now: bool, accept_anyway: bool) -> Result<Update, UpdateError> {
        let result = self.update_ytdlp(check_now, accept_anyway).await;
        if matches!(result, Ok(Update::Installed(_) | Update::Current(_))) {
            self.ensure_deno().await;
            self.ensure_node().await;
        }
        result
    }

    async fn update_ytdlp(&self, check_now: bool, accept_anyway: bool) -> Result<Update, UpdateError> {
        let Ok(_one) = self.inner.one_at_a_time.try_lock() else {
            return Err(UpdateError::Busy);
        };
        if !check_now && !self.due() {
            return Ok(Update::Skipped);
        }
        lock(&self.inner.flags).updating = true;
        let _clear = UpdatingGuard(&self.inner);
        let result = self.check_and_install(accept_anyway).await;

        // A verdict on the release counts as a check; a network failure does
        // not, so the next tick tries again.
        let verdict = matches!(
            result,
            Ok(_) | Err(UpdateError::Refused(_) | UpdateError::Rollback(_) | UpdateError::Install(_))
        );
        if verdict {
            let now = self.now();
            if let Err(e) = self.edit_settings(|s| s.last_check = Some(now)) {
                tracing::warn!("settings.toml not written: {e:#}");
            }
        }
        {
            let mut f = lock(&self.inner.flags);
            f.error = result.as_ref().err().map(ToString::to_string);
            f.error_code = result.as_ref().err().map(UpdateError::fault);
            f.forceable = matches!(result, Err(UpdateError::Rollback(_)));
        }
        match &result {
            Ok(Update::Installed(i)) => tracing::info!("yt-dlp {} installed ({:?})", i.version, i.channel),
            Ok(_) => {}
            Err(e) => tracing::warn!("yt-dlp update: {e}"),
        }
        result
    }

    async fn fetch(&self, name: &str, base: &str, limit: Limit) -> Result<Vec<u8>, UpdateError> {
        self.inner.deps.fetch.get(&format!("{base}{name}"), limit).await.map_err(UpdateError::Fetch)
    }

    async fn check_and_install(&self, accept_anyway: bool) -> Result<Update, UpdateError> {
        let caps = self.reprobe().await;
        let (Some(_), Some(python3)) = (caps.python3.clone(), caps.python3_path.clone()) else {
            return Err(UpdateError::Python3Missing);
        };
        // The wrapper names it in its shebang: refused here, before anything
        // is downloaded, rather than after the swap.
        wrapper::check_interpreter(&python3).map_err(UpdateError::Python3Unusable)?;
        let channel = lock(&self.inner.settings).channel;
        // One tag for everything: never a list of one release and the asset
        // of the next.
        let location = self.inner.deps.fetch.redirect_of(&channel.latest_url(), LATEST).await.map_err(UpdateError::Fetch)?;
        let tag = channel.tag_from_location(&location).map_err(UpdateError::Fetch)?;
        let base = channel.download_base(&tag);
        let sums = self.fetch(SUMS, &base, SMALL).await?;
        let sig = self.fetch(SIG, &base, SMALL).await?;
        verify::check_signature(&sums, &sig, &self.inner.deps.key).map_err(UpdateError::Refused)?;
        let want = verify::listed_sum(&sums, ASSET).map_err(UpdateError::Refused)?;
        let want_hex = verify::to_hex(&want);

        let record = lock(&self.inner.settings).installed.clone();
        // The same bytes are the same tool, whichever channel listed them:
        // nothing to download, only the channel to record.
        if let Some(r) = &record
            && r.sha256 == want_hex
            && self.installed_now()
        {
            if r.channel != channel
                && let Err(e) = self.edit_settings(|s| {
                    if let Some(i) = s.installed.as_mut() {
                        i.channel = channel;
                    }
                })
            {
                tracing::warn!("settings.toml not written: {e:#}");
            }
            return Ok(Update::Current(Installed { version: r.version.clone(), channel }));
        }

        let asset = self.fetch(ASSET, &base, ASSET_LIMIT).await?;
        // The whole chain again, on the bytes that will be unpacked.
        verify::verify(&sums, &sig, &self.inner.deps.key, &asset, ASSET).map_err(UpdateError::Refused)?;
        // Read only to compare: the version installed is the one the warm-up
        // answers.
        let declared = install::extract_one(&asset, "yt_dlp/version.py", VERSION_PY_MAX)
            .ok()
            .and_then(|v| declared_version(&v));
        judge_rollback(declared.as_deref(), &want_hex, record.as_ref(), self.installed_now(), channel, accept_anyway)
            .map_err(UpdateError::Rollback)?;

        let data = self.inner.data_dir.clone();
        let new = data.join(install::NEW_DIR);
        let staged = {
            let new = new.clone();
            tokio::task::spawn_blocking(move || {
                if new.symlink_metadata().is_ok() {
                    std::fs::remove_dir_all(&new).map_err(|e| format!("{}: {e}", new.display()))?;
                }
                install::extract(&asset, &new, install::YTDLP_LIMITS)?;
                if !new.join("yt_dlp/__init__.py").is_file() {
                    return Err(NO_PACKAGE.to_string());
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        };
        let version = match staged {
            Ok(()) => self.warm_up(&python3, &new).await,
            Err(e) => Err(e),
        };
        let version = match version {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&new);
                return Err(UpdateError::Install(e));
            }
        };

        lock(&self.inner.flags).waiting_for_jobs = true;
        {
            let _jobs_done = self.inner.deps.gate.write().await;
            lock(&self.inner.flags).waiting_for_jobs = false;
            if let Err(e) = install::swap_in(&data) {
                let _ = std::fs::remove_dir_all(&new);
                return Err(UpdateError::Install(format!("{SWAP}{e}")));
            }
            self.write_conf().map_err(|e| UpdateError::Install(format!("{WRITE}{e}")))?;
            self.write_wrapper(&self.capabilities()).map_err(|e| UpdateError::Install(format!("{WRITE}{e}")))?;
        }

        let record = InstalledRecord { version: version.clone(), channel, sha256: want_hex, tag: Some(tag) };
        if let Err(e) = self.edit_settings(|s| s.installed = Some(record)) {
            tracing::warn!("settings.toml not written: {e:#}");
        }
        Ok(Update::Installed(Installed { version, channel }))
    }

    /// Runs the new package once (`--version`): compiles its bytecode where
    /// it will live, proves it starts, and says its version.
    async fn warm_up(&self, python3: &Path, package_dir: &Path) -> Result<String, String> {
        let args = wrapper::warm_up_args(package_dir)?;
        let (ok, out, err) = runtime::capture(python3, &args, WARM_UP).await?;
        let last = |s: &str| s.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_string);
        if !ok {
            return Err(format!("{DOES_NOT_START}{}", last(&err).unwrap_or_else(|| "no message".into())));
        }
        let version = last(&out).unwrap_or_default();
        let plausible = !version.is_empty()
            && version.len() <= 64
            && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
        if !plausible {
            return Err(format!("{NO_VERSION}{version:?}"));
        }
        Ok(version)
    }

    /// The plugin's own deno, where deno publishes builds, when no usable one
    /// is there (absent, or older than yt-dlp accepts). Its failure is no
    /// failure of yt-dlp's update: deno is only the fallback's runtime. Runs
    /// outside the update's section (a 45 MB download must not hold
    /// `updating`), one at a time, and not again within a day of a failure.
    async fn ensure_deno(&self) {
        if runtime::deno_asset(&self.inner.deps.arch).is_none()
            || self.capabilities().js_runtime == Some(runtime::JsRuntime::Deno)
        {
            return;
        }
        let Ok(_one) = self.inner.runtime_one_at_a_time.try_lock() else { return };
        let now = self.now();
        if self.failed_within_a_day(now) {
            return;
        }
        let r = runtime::install_deno(self.inner.deps.fetch.as_ref(), &self.inner.data_dir, &self.inner.deps.arch).await;
        if let Err(e) = &r {
            tracing::warn!("deno not installed: {e}");
        }
        self.record_runtime(now, r.err());
        self.reprobe().await;
    }

    /// The last runtime install failed less than a day ago (a failure dated
    /// in the future, a clock set back, does not count).
    fn failed_within_a_day(&self, now: u64) -> bool {
        lock(&self.inner.flags).runtime_failed_at.is_some_and(|t| t <= now && now - t < DAY)
    }

    fn record_runtime(&self, now: u64, error: Option<runtime::RuntimeError>) {
        let mut f = lock(&self.inner.flags);
        f.runtime_failed_at = error.is_some().then_some(now);
        f.runtime_error = error;
    }

    /// The plugin's own node 22, on armv7 only, installed when absent and
    /// **updated** whenever the signed list of `latest-v22.x` names another
    /// version than the one in place answers (deno is only installed when
    /// absent). Its failure is no failure of yt-dlp's update, and whatever
    /// fails (the list unreachable, a releaser whose key is not embedded),
    /// the node in place keeps serving. One at a time, not again within a
    /// day of a failure, outside the update's section (a 25 MB download).
    async fn ensure_node(&self) {
        if self.inner.deps.arch != runtime::NODE_ARCH {
            return;
        }
        let Ok(_one) = self.inner.runtime_one_at_a_time.try_lock() else { return };
        let now = self.now();
        if self.failed_within_a_day(now) {
            return;
        }
        let installed = runtime::own_node_version(&self.inner.data_dir).await;
        let r = runtime::install_node(
            self.inner.deps.fetch.as_ref(),
            &self.inner.data_dir,
            &self.inner.deps.arch,
            &self.inner.deps.node_keys,
            installed.as_deref(),
        )
        .await;
        match &r {
            Ok(runtime::NodeInstall::Installed(v)) => tracing::info!("node {v} installed"),
            Ok(runtime::NodeInstall::Current(_)) => {}
            Err(e) => tracing::warn!("node not installed: {e}"),
        }
        let changed = matches!(r, Ok(runtime::NodeInstall::Installed(_)));
        self.record_runtime(now, r.err());
        if changed {
            self.reprobe().await;
        }
    }

    /// The hourly tick: an update when due, the rollback guard on.
    async fn run_tick(&self) {
        if self.due() {
            let _ = self.update(false).await;
        }
    }

    /// The page's request: now, the guard as the page asked.
    async fn run_request(&self) {
        let force = lock(&self.inner.flags).requested.take().unwrap_or(false);
        let _ = self.update_with(true, force).await;
    }
}

/// At start: probes the machine and refreshes the wrapper and conf; then
/// updates when due, every hour, and whenever the page asks.
pub async fn schedule(tool: Tool) {
    tool.start().await;
    let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
    loop {
        tokio::select! {
            _ = tick.tick() => tool.run_tick().await,
            () = tool.inner.requests.notified() => tool.run_request().await,
        }
    }
}

#[cfg(test)]
mod tests;
