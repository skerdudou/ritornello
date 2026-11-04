//! What the machine offers yt-dlp: python3 (the plugin's one system
//! prerequisite) and, for the wrapper's second attempt only, a JavaScript
//! runtime.
//!
//! The runtime order, and why (measured on the device, armv7l):
//! 1. the plugin's own deno, only on x86_64 and aarch64: deno publishes
//!    nothing for 32-bit ARM;
//! 2. the plugin's own node 22, only on 32-bit ARM (`arch == "arm"`): node 22
//!    is the last line nodejs.org builds for `linux-armv7l` (none since node
//!    24). It comes before the system's because it is the one the plugin keeps
//!    up to date;
//! 3. a system `node` of at least 22.0.0, yt-dlp's own minimum
//!    (`yt_dlp/utils/_jsruntime.py`; Debian 13's node 20.19.2 is refused);
//! 4. a system `qjs` (QuickJS, at least 2023-12-09; QuickJS-ng any
//!    version), marked slow: ~125 s per video;
//! 5. none.
//!
//! Deno does not sign its releases. Its zip is checked against the
//! `.sha256sum` published next to it, in the same release: that catches a
//! damaged download, not a compromised release. That is the whole of deno's
//! trust chain here, and why it is only ever a fallback.
//!
//! Node.js does sign them. `SHASUMS256.txt.sig` of `latest-v22.x` must be a
//! detached signature of `SHASUMS256.txt` by one of the embedded Node.js
//! release keys ([`super::verify::NODE_RELEASE_KEYS`]; a bound signing subkey
//! of one counts), that signed list names the armv7l tarball and its
//! SHA-256, the tarball must hash to it; then only `bin/node` is taken out of
//! it (~108 MB; the whole install would be 187 MB), and it must answer the
//! listed version before it replaces the one in place. Unlike deno, which is
//! installed once, node is updated: whenever the signed list names a newer
//! version than the one installed. Never an older one: an old list, validly
//! signed, replayed by a mirror or on the path, would otherwise bring back a
//! node with known holes ([`RuntimeCause::Rollback`]). Node 22's maintenance
//! ends on 2027-04-30
//! ([`NODE22_END_OF_LIFE`]): the plugin then keeps the last one as it is, and
//! the page says so; nothing is deleted.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pgp::composed::SignedPublicKey;
use serde::Serialize;

use super::fetch::{Fetch, Limit};
use super::install::{extract_one, extract_tar_gz_entry, write_executable};
use super::verify::{VerifyError, check_signature_by_any, listed_sum, sha256};

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JsRuntime {
    Deno,
    Node,
    Quickjs,
}

impl JsRuntime {
    /// The name `--js-runtimes` takes.
    pub fn name(self) -> &'static str {
        match self {
            JsRuntime::Deno => "deno",
            JsRuntime::Node => "node",
            JsRuntime::Quickjs => "quickjs",
        }
    }
}

/// What the page's capabilities panel shows, and what the wrapper is
/// written with.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Capabilities {
    /// python3's version, `None` when there is no python3.
    pub python3: Option<String>,
    pub js_runtime: Option<JsRuntime>,
    /// Too slow to play with (QuickJS); a download still succeeds.
    pub js_runtime_slow: bool,
    /// The runtime in use is the plugin's own node 22 and its maintenance
    /// has ended: kept as it is. Set by the tool when read, from its clock
    /// ([`unmaintained`]); a probe leaves it false.
    pub js_runtime_unmaintained: bool,
    /// The exact command that would fix what is missing, when one exists.
    pub apt_hint: Option<String>,
    /// `<name>:<absolute path>`, as `--js-runtimes` takes it.
    #[serde(skip)]
    pub runtime_arg: Option<String>,
    /// The python3 that answered: the wrapper's interpreter.
    #[serde(skip)]
    pub python3_path: Option<PathBuf>,
    /// The runtime in use is the plugin's own node.
    #[serde(skip)]
    pub own_node: bool,
}

/// Where to look, and for which machine.
#[derive(Clone, Debug)]
pub struct Env {
    /// The directories searched for python3, node and qjs, in order.
    pub search: Vec<PathBuf>,
    pub data_dir: PathBuf,
    /// `std::env::consts::ARCH`.
    pub arch: String,
}

pub const PROBE_DEADLINE: Duration = Duration::from_secs(10);

pub fn which(name: &str, search: &[PathBuf]) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    search
        .iter()
        .map(|d| d.join(name))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// stdout and stderr of `bin args`, when it ran within `deadline` (its whole
/// group is killed otherwise). The exit status is not judged: `qjs --help`
/// exits 1.
pub(crate) async fn capture(bin: &Path, args: &[String], deadline: Duration) -> Result<(bool, String, String), String> {
    use tokio::io::AsyncReadExt;
    let mut child = crate::ytdlp::spawn_in_own_group(bin, args).await.map_err(|e| format!("{}: {e}", bin.display()))?;
    let pgid = child.id();
    let (mut so, mut se) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ran = tokio::time::timeout(deadline, async {
        let (a, b, status) = tokio::join!(so.read_to_end(&mut out), se.read_to_end(&mut err), child.wait());
        a.and(b).and(status)
    })
    .await;
    match ran {
        Ok(Ok(status)) => {
            Ok((status.success(), String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned()))
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            crate::ytdlp::kill_group(pgid);
            let _ = child.wait().await;
            Err(format!("{} timed out", bin.display()))
        }
    }
}

/// The numbers of a version, the way yt-dlp reads them leniently:
/// `22.1.0`, `2025-04-26`, `2.9.8+abc` -> [22,1,0], [2025,4,26], [2,9,8].
pub(crate) fn numbers(version: &str) -> Vec<u64> {
    version
        .split(['.', '-'])
        .map_while(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

fn at_least(version: &str, min: &[u64]) -> bool {
    let v = numbers(version);
    !v.is_empty() && v.as_slice() >= min
}

async fn output_of(bin: &Path, arg: &str, deadline: Duration) -> Option<String> {
    let (_, out, err) = capture(bin, &[arg.to_string()], deadline).await.ok()?;
    Some(format!("{out}\n{err}"))
}

fn first_word_after(text: &str, prefix: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix(prefix).and_then(|r| r.split_whitespace().next()).map(str::to_string))
}

/// python3 and its version, when it answers.
pub async fn python3(search: &[PathBuf]) -> Option<(PathBuf, String)> {
    let bin = which("python3", search)?;
    let version = first_word_after(&output_of(&bin, "--version", PROBE_DEADLINE).await?, "Python ")?;
    Some((bin, version))
}

/// The deno asset for this machine; `None` where deno publishes no build.
pub fn deno_asset(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("deno-x86_64-unknown-linux-gnu.zip"),
        "aarch64" => Some("deno-aarch64-unknown-linux-gnu.zip"),
        _ => None,
    }
}

/// The one architecture the plugin installs its own node on: 32-bit ARM,
/// where deno publishes nothing.
pub const NODE_ARCH: &str = "arm";

/// The runtime the plugin installs itself on `arch`, if any.
pub fn own_runtime(arch: &str) -> Option<JsRuntime> {
    if deno_asset(arch).is_some() {
        Some(JsRuntime::Deno)
    } else if arch == NODE_ARCH {
        Some(JsRuntime::Node)
    } else {
        None
    }
}

pub fn own_deno(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("deno")
}

pub fn own_node(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("node")
}

async fn usable_deno(env: &Env) -> Option<PathBuf> {
    deno_asset(&env.arch)?;
    let bin = own_deno(&env.data_dir);
    let text = output_of(&bin, "--version", PROBE_DEADLINE).await?;
    at_least(&first_word_after(&text, "deno ")?, &[2, 3, 0]).then_some(bin)
}

/// What `node --version` answers, without its `v`: `22.23.4`.
async fn node_version(bin: &Path, deadline: Duration) -> Option<String> {
    let text = output_of(bin, "--version", deadline).await?;
    text.lines().find_map(|l| l.trim().strip_prefix('v').map(str::to_string))
}

/// The version of the plugin's own node, when there is one that answers.
pub async fn own_node_version(data_dir: &Path) -> Option<String> {
    node_version(&own_node(data_dir), PROBE_DEADLINE).await
}

/// The plugin's own node, on 32-bit ARM only, when it answers at least 22.
async fn usable_own_node(env: &Env) -> Option<PathBuf> {
    if env.arch != NODE_ARCH {
        return None;
    }
    let bin = own_node(&env.data_dir);
    at_least(&node_version(&bin, PROBE_DEADLINE).await?, &[22, 0, 0]).then_some(bin)
}

async fn usable_node(search: &[PathBuf]) -> Option<PathBuf> {
    let bin = which("node", search)?;
    at_least(&node_version(&bin, PROBE_DEADLINE).await?, &[22, 0, 0]).then_some(bin)
}

async fn usable_quickjs(search: &[PathBuf]) -> Option<PathBuf> {
    let bin = which("qjs", search)?;
    let text = output_of(&bin, "--help", PROBE_DEADLINE).await?;
    if let Some(v) = first_word_after(&text, "QuickJS-ng version") {
        return (!numbers(&v).is_empty()).then_some(bin);
    }
    let v = first_word_after(&text, "QuickJS version")?;
    at_least(&v, &[2023, 12, 9]).then_some(bin)
}

/// Probes everything; a few short processes, each under a deadline.
pub async fn detect(env: &Env) -> Capabilities {
    let (python3_path, python3) = python3(&env.search).await.unzip();
    let apt_hint = python3.is_none().then(|| "sudo apt install python3".to_string());
    let mut own_node = false;
    let found = if let Some(p) = usable_deno(env).await {
        Some((JsRuntime::Deno, p))
    } else if let Some(p) = usable_own_node(env).await {
        own_node = true;
        Some((JsRuntime::Node, p))
    } else if let Some(p) = usable_node(&env.search).await {
        Some((JsRuntime::Node, p))
    } else {
        usable_quickjs(&env.search).await.map(|p| (JsRuntime::Quickjs, p))
    };
    Capabilities {
        python3,
        js_runtime: found.as_ref().map(|(r, _)| *r),
        js_runtime_slow: matches!(found, Some((JsRuntime::Quickjs, _))),
        js_runtime_unmaintained: false,
        apt_hint,
        runtime_arg: found.map(|(r, p)| format!("{}:{}", r.name(), p.display())),
        python3_path,
        own_node,
    }
}

/// The end of node 22's maintenance: 2027-04-30 is its last day, per the
/// schedule of https://github.com/nodejs/release#release-schedule ("Jod",
/// End-of-Life 2027-04-30). This is the first second after it,
/// 2027-05-01T00:00:00Z, in Unix seconds.
pub const NODE22_END_OF_LIFE: u64 = 1_809_129_600;

/// The runtime of `caps` is the plugin's own node 22, at `now`, past its
/// end of life.
pub fn unmaintained(caps: &Capabilities, now: u64) -> bool {
    caps.own_node && caps.js_runtime == Some(JsRuntime::Node) && now >= NODE22_END_OF_LIFE
}

pub const DENO_BASE: &str = "https://github.com/denoland/deno/releases/latest/download/";
const DENO_ZIP: Limit = Limit { max_bytes: 256 << 20, deadline: Duration::from_secs(900) };
const DENO_SUM: Limit = Limit { max_bytes: 4096, deadline: Duration::from_secs(30) };
const DENO_MAX_BINARY: u64 = 512 << 20;

/// The newest node 22, whatever its number: the signed list there names it.
pub const NODE_BASE: &str = "https://nodejs.org/dist/latest-v22.x/";
pub const NODE_SUMS: &str = "SHASUMS256.txt";
pub const NODE_SIG: &str = "SHASUMS256.txt.sig";
/// The real list is 4 KB, its signature 119 bytes.
const NODE_SMALL: Limit = Limit { max_bytes: 1 << 20, deadline: Duration::from_secs(30) };
/// The real armv7l tarball is ~25 MB.
const NODE_TARBALL: Limit = Limit { max_bytes: 128 << 20, deadline: Duration::from_secs(900) };
/// The real `bin/node` is 107 563 500 bytes (v22.23.4).
const NODE_MAX_BINARY: u64 = 256 << 20;
/// Its first start on the device reads 108 MB from the card.
const NODE_FIRST_RUN: Duration = Duration::from_secs(60);

/// Why the plugin's own runtime (deno, or node on armv7) could not be
/// installed or updated. A variant per thing the plugin says; what the
/// network or the system said stays raw inside.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeError {
    pub runtime: JsRuntime,
    pub cause: RuntimeCause,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeCause {
    /// No build is published for this machine (the architecture).
    NoBuild(String),
    Fetch(String),
    /// The signed list is not vouched for by an embedded release key.
    BadSignature(String),
    NotListed,
    /// The signed list names something else than one build of the line
    /// expected (another major, several candidates).
    Unexpected(String),
    Mismatch,
    Extract(String),
    /// The new binary does not answer the version it was listed as.
    Version(String),
    Write(String),
    /// The signed list names an older version than the highest one ever
    /// installed (the one in place, or the one `settings.toml` records): an
    /// old list replayed. Refused before the tarball, the node in place kept.
    Rollback { installed: String, listed: String },
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.runtime.name();
        match &self.cause {
            RuntimeCause::NoBuild(arch) => write!(f, "no {name} for {arch}"),
            RuntimeCause::Fetch(e) => write!(f, "{name}: download failed: {e}"),
            RuntimeCause::BadSignature(e) => write!(f, "{name}: the list of sums is refused: bad signature: {e}"),
            RuntimeCause::NotListed => write!(f, "{name}: the download is not in the published sums"),
            RuntimeCause::Unexpected(e) => write!(f, "{name}: the published sums are not the expected ones: {e}"),
            RuntimeCause::Mismatch => write!(f, "{name}: the download does not match its published sum"),
            RuntimeCause::Extract(e) => write!(f, "{name}: {e}"),
            RuntimeCause::Version(e) => write!(f, "{name}: the new binary answers {e}"),
            RuntimeCause::Write(e) => write!(f, "{name}: {e}"),
            RuntimeCause::Rollback { installed, listed } => {
                write!(f, "{name}: the signed list names {listed}, older than the installed {installed}: refused")
            }
        }
    }
}

impl RuntimeError {
    /// The code the page translates (`runtime_<code>` keys), with the
    /// runtime's wire name as `runtime`.
    pub fn fault(&self) -> crate::tool::UpdateFault {
        use crate::tool::fault_with;
        let runtime = ("runtime", self.runtime.name());
        match &self.cause {
            RuntimeCause::NoBuild(arch) => fault_with("no_build", &[runtime, ("arch", arch)]),
            RuntimeCause::Fetch(e) => fault_with("fetch", &[runtime, ("detail", e)]),
            RuntimeCause::BadSignature(e) => fault_with("bad_signature", &[runtime, ("detail", e)]),
            RuntimeCause::NotListed => fault_with("not_listed", &[runtime]),
            RuntimeCause::Unexpected(e) => fault_with("unexpected", &[runtime, ("detail", e)]),
            RuntimeCause::Mismatch => fault_with("mismatch", &[runtime]),
            RuntimeCause::Extract(e) => fault_with("extract", &[runtime, ("detail", e)]),
            RuntimeCause::Version(e) => fault_with("version", &[runtime, ("detail", e)]),
            RuntimeCause::Write(e) => fault_with("write", &[runtime, ("detail", e)]),
            RuntimeCause::Rollback { installed, listed } => {
                fault_with("rollback", &[runtime, ("installed", installed), ("listed", listed)])
            }
        }
    }
}

/// Downloads deno for `arch`, checks it against its published sum, and puts
/// it at `bin/deno`. Refused on a machine deno publishes nothing for.
pub async fn install_deno(fetch: &dyn Fetch, data_dir: &Path, arch: &str) -> Result<(), RuntimeError> {
    let err = |cause| RuntimeError { runtime: JsRuntime::Deno, cause };
    let asset = deno_asset(arch).ok_or_else(|| err(RuntimeCause::NoBuild(arch.to_string())))?;
    let sums = fetch.get(&format!("{DENO_BASE}{asset}.sha256sum"), DENO_SUM).await.map_err(|e| err(RuntimeCause::Fetch(e)))?;
    let want = listed_sum(&sums, asset).map_err(|_| err(RuntimeCause::NotListed))?;
    let zip = fetch.get(&format!("{DENO_BASE}{asset}"), DENO_ZIP).await.map_err(|e| err(RuntimeCause::Fetch(e)))?;
    if sha256(&zip) != want {
        return Err(err(RuntimeCause::Mismatch));
    }
    let binary = tokio::task::spawn_blocking(move || extract_one(&zip, "deno", DENO_MAX_BINARY))
        .await
        .map_err(|e| err(RuntimeCause::Extract(e.to_string())))?
        .map_err(|e| err(RuntimeCause::Extract(e)))?;
    write_executable(&own_deno(data_dir), &binary).map_err(|e| err(RuntimeCause::Write(e.to_string())))
}

/// The armv7l tarball a list of sums names: its version (read from its
/// name, `node-v<version>-linux-armv7l.tar.gz`) and its name. Exactly one
/// such line, of node 22.
pub fn node_release(sums: &[u8]) -> Result<(String, String), RuntimeCause> {
    let text = String::from_utf8_lossy(sums);
    let found: Vec<(String, String)> = text
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.trim_end_matches('\r').split_once(' ')?;
            let name = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*')).unwrap_or(rest);
            let version = name.strip_prefix("node-v")?.strip_suffix("-linux-armv7l.tar.gz")?;
            Some((version.to_string(), name.to_string()))
        })
        .collect();
    let [(version, name)] = found.as_slice() else {
        return Err(if found.is_empty() {
            RuntimeCause::NotListed
        } else {
            RuntimeCause::Unexpected(format!("{} armv7l tarballs", found.len()))
        });
    };
    let well_formed = version.split('.').count() == 3
        && version.split('.').all(|n| !n.is_empty() && n.len() <= 6 && n.chars().all(|c| c.is_ascii_digit()));
    if !well_formed || !version.starts_with("22.") {
        return Err(RuntimeCause::Unexpected(format!("node {version:?}, not 22")));
    }
    Ok((version.clone(), name.clone()))
}

/// What [`install_node`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeInstall {
    /// The signed list names the version already installed: nothing fetched
    /// past the list.
    Current(String),
    Installed(String),
}

/// Puts the newest signed node 22 for armv7l at `bin/node`, unless the
/// signed list names `installed` (the version the node in place answers).
/// Refused on any other architecture, before anything is fetched. Whatever
/// fails, the node in place stays.
///
/// Never a rollback: a listed version older than `installed` or than
/// `highest` (the highest one ever installed, as `settings.toml` records it,
/// so that deleting `bin/node` brings no older one back) is refused before
/// the tarball is fetched. The very version of `highest`, with no node in
/// place answering it, is installed again: a repair, not a rollback.
/// Unlike yt-dlp's guard, nothing overrides this one, on purpose: node 22's
/// `latest-v22.x` only ever moves forward, so an older signed list there is
/// never one to take, and there is no channel to switch.
pub async fn install_node(
    fetch: &dyn Fetch,
    data_dir: &Path,
    arch: &str,
    keys: &[SignedPublicKey],
    installed: Option<&str>,
    highest: Option<&str>,
) -> Result<NodeInstall, RuntimeError> {
    let err = |cause| RuntimeError { runtime: JsRuntime::Node, cause };
    if arch != NODE_ARCH {
        return Err(err(RuntimeCause::NoBuild(arch.to_string())));
    }
    let get = |name: &str, limit: Limit| {
        let url = format!("{NODE_BASE}{name}");
        async move { fetch.get(&url, limit).await.map_err(|e| err(RuntimeCause::Fetch(e))) }
    };
    let sums = get(NODE_SUMS, NODE_SMALL).await?;
    let sig = get(NODE_SIG, NODE_SMALL).await?;
    check_signature_by_any(&sums, &sig, keys).map_err(|e| {
        err(match e {
            VerifyError::BadSignature(why) => RuntimeCause::BadSignature(why),
            other => RuntimeCause::BadSignature(other.to_string()),
        })
    })?;
    let (version, asset) = node_release(&sums).map_err(err)?;
    if installed == Some(version.as_str()) {
        return Ok(NodeInstall::Current(version));
    }
    if let Some(floor) = [installed, highest].into_iter().flatten().max_by_key(|v| numbers(v))
        && numbers(&version) < numbers(floor)
    {
        return Err(err(RuntimeCause::Rollback { installed: floor.to_string(), listed: version }));
    }
    let want = listed_sum(&sums, &asset).map_err(|_| err(RuntimeCause::NotListed))?;
    let tarball = get(&asset, NODE_TARBALL).await?;
    if sha256(&tarball) != want {
        return Err(err(RuntimeCause::Mismatch));
    }

    let bin = own_node(data_dir);
    let new = data_dir.join("bin").join("node.new");
    std::fs::create_dir_all(data_dir.join("bin")).map_err(|e| err(RuntimeCause::Write(e.to_string())))?;
    let entry = format!("node-v{version}-linux-armv7l/bin/node");
    let staged = new.clone();
    tokio::task::spawn_blocking(move || extract_tar_gz_entry(&tarball, &entry, &staged, NODE_MAX_BINARY))
        .await
        .map_err(|e| err(RuntimeCause::Extract(e.to_string())))?
        .map_err(|e| err(RuntimeCause::Extract(e)))?;
    // Run once where it will live, before it replaces anything.
    let answered = node_version(&new, NODE_FIRST_RUN).await;
    if answered.as_deref() != Some(version.as_str()) {
        let _ = std::fs::remove_file(&new);
        let said = answered.map_or_else(|| "no version".to_string(), |v| format!("v{v} instead of v{version}"));
        return Err(err(RuntimeCause::Version(said)));
    }
    if let Err(e) = std::fs::rename(&new, &bin) {
        let _ = std::fs::remove_file(&new);
        return Err(err(RuntimeCause::Write(format!("{}: {e}", bin.display()))));
    }
    Ok(NodeInstall::Installed(version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::testkit::*;
    use pgp::crypto::hash::HashAlgorithm;

    fn env(search: Vec<PathBuf>, data: &Path, arch: &str) -> Env {
        Env { search, data_dir: data.to_path_buf(), arch: arch.into() }
    }

    #[test]
    fn versions_are_read_leniently_and_compared_by_number() {
        assert_eq!(numbers("22.1.0"), vec![22, 1, 0]);
        assert_eq!(numbers("2025-04-26"), vec![2025, 4, 26]);
        assert_eq!(numbers("2.9.8+abc"), vec![2, 9, 8]);
        assert!(at_least("22.0.0", &[22, 0, 0]) && !at_least("21.99.0", &[22, 0, 0]));
        assert!(at_least("100.0", &[22, 0, 0]) && !at_least("20.19.2", &[22, 0, 0]));
        assert!(!at_least("unknown", &[1]));
    }

    #[tokio::test]
    async fn python3_missing_is_said_with_the_command_that_fixes_it() {
        let empty = tempfile::tempdir().unwrap();
        let caps = detect(&env(vec![empty.path().into()], empty.path(), "arm")).await;
        assert_eq!(caps.python3, None);
        assert_eq!(caps.apt_hint.as_deref(), Some("sudo apt install python3"));
        assert_eq!(caps.js_runtime, None);
        assert_eq!(caps.runtime_arg, None);
    }

    #[tokio::test]
    async fn python3_is_found_with_its_version_and_no_hint() {
        let (_g, search) = python_only_path();
        let caps = detect(&env(vec![search], Path::new("/nonexistent"), "arm")).await;
        let v = caps.python3.expect("python3 found");
        assert!(v.starts_with("3."), "{v}");
        assert_eq!(caps.apt_hint, None);
        assert!(caps.python3_path.unwrap().ends_with("python3"));
    }

    #[tokio::test]
    async fn node_counts_from_22_and_quickjs_is_the_slow_last_resort() {
        let sys = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let s = vec![sys.path().to_path_buf()];

        write_script(sys.path(), "node", "echo v20.19.2");
        let caps = detect(&env(s.clone(), data.path(), "arm")).await;
        assert_eq!(caps.js_runtime, None, "Debian 13's node is too old for yt-dlp");

        write_script(sys.path(), "qjs", "echo 'QuickJS version 2025-04-26'; exit 1");
        let caps = detect(&env(s.clone(), data.path(), "arm")).await;
        assert_eq!((caps.js_runtime, caps.js_runtime_slow), (Some(JsRuntime::Quickjs), true));
        assert_eq!(caps.runtime_arg, Some(format!("quickjs:{}", sys.path().join("qjs").display())));

        write_script(sys.path(), "node", "echo v22.0.0");
        let caps = detect(&env(s.clone(), data.path(), "arm")).await;
        assert_eq!((caps.js_runtime, caps.js_runtime_slow), (Some(JsRuntime::Node), false));
        assert_eq!(caps.runtime_arg, Some(format!("node:{}", sys.path().join("node").display())));
    }

    #[tokio::test]
    async fn an_old_quickjs_is_refused_and_quickjs_ng_is_taken() {
        let sys = tempfile::tempdir().unwrap();
        let s = vec![sys.path().to_path_buf()];
        write_script(sys.path(), "qjs", "echo 'QuickJS version 2021-03-27'");
        assert_eq!(detect(&env(s.clone(), sys.path(), "arm")).await.js_runtime, None);
        write_script(sys.path(), "qjs", "echo 'QuickJS-ng version 0.10.1'");
        assert_eq!(detect(&env(s.clone(), sys.path(), "arm")).await.js_runtime, Some(JsRuntime::Quickjs));
    }

    #[tokio::test]
    async fn the_own_deno_comes_first_but_only_where_deno_publishes_builds() {
        let sys = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let s = vec![sys.path().to_path_buf()];
        write_script(sys.path(), "node", "echo v22.1.0");
        std::fs::create_dir(data.path().join("bin")).unwrap();
        write_script(&data.path().join("bin"), "deno", "echo 'deno 2.9.8 (stable, release, x86_64-unknown-linux-gnu)'");

        let caps = detect(&env(s.clone(), data.path(), "x86_64")).await;
        assert_eq!(caps.js_runtime, Some(JsRuntime::Deno));
        assert_eq!(caps.runtime_arg, Some(format!("deno:{}", own_deno(data.path()).display())));
        let caps = detect(&env(s.clone(), data.path(), "arm")).await;
        assert_eq!(caps.js_runtime, Some(JsRuntime::Node), "no deno build exists for armv7");

        // A deno older than yt-dlp's minimum is no runtime.
        write_script(&data.path().join("bin"), "deno", "echo 'deno 2.2.9 (stable)'");
        assert_eq!(detect(&env(s, data.path(), "aarch64")).await.js_runtime, Some(JsRuntime::Node));
    }

    fn deno_zip(binary: &str) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut w = zip::ZipWriter::new(&mut out);
            w.start_file("deno", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(binary.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        out.into_inner()
    }

    #[tokio::test]
    async fn deno_is_installed_when_its_published_sum_matches() {
        let data = tempfile::tempdir().unwrap();
        let fetch = FakeFetch::default();
        let asset = "deno-x86_64-unknown-linux-gnu.zip";
        let zip = deno_zip("#!/bin/sh\necho 'deno 2.9.8'\n");
        fetch.serve(&format!("{DENO_BASE}{asset}.sha256sum"), Ok(sums_for(&[(asset, &zip)])));
        fetch.serve(&format!("{DENO_BASE}{asset}"), Ok(zip));
        install_deno(&fetch, data.path(), "x86_64").await.unwrap();
        let e = env(vec![], data.path(), "x86_64");
        assert_eq!(usable_deno(&e).await, Some(own_deno(data.path())));
    }

    #[tokio::test]
    async fn deno_that_does_not_match_its_sum_is_refused() {
        let data = tempfile::tempdir().unwrap();
        let fetch = FakeFetch::default();
        let asset = "deno-aarch64-unknown-linux-gnu.zip";
        fetch.serve(&format!("{DENO_BASE}{asset}.sha256sum"), Ok(sums_for(&[(asset, b"what was published")])));
        fetch.serve(&format!("{DENO_BASE}{asset}"), Ok(deno_zip("tampered")));
        let err = install_deno(&fetch, data.path(), "aarch64").await.unwrap_err();
        assert_eq!(err, RuntimeError { runtime: JsRuntime::Deno, cause: RuntimeCause::Mismatch });
        assert!(!own_deno(data.path()).exists());
    }

    #[tokio::test]
    async fn no_deno_is_ever_fetched_for_armv7() {
        let data = tempfile::tempdir().unwrap();
        let fetch = FakeFetch::default();
        assert!(install_deno(&fetch, data.path(), "arm").await.is_err());
        assert!(fetch.asked().is_empty());
    }

    // ---- the plugin's own node, on armv7 ----------------------------------

    fn node_asset(version: &str) -> String {
        format!("node-v{version}-linux-armv7l.tar.gz")
    }

    fn node_entry(version: &str) -> String {
        format!("node-v{version}-linux-armv7l/bin/node")
    }

    /// A fake node binary: a script answering `--version` with `v<answers>`.
    fn node_script(answers: &str) -> String {
        format!("#!/bin/sh\necho v{answers}\n")
    }

    /// A tarball shaped like the real one (a directory, links next to the
    /// binary, then the binary), its binary answering `answers`.
    fn node_tarball(version: &str, answers: &str) -> Vec<u8> {
        let dir = format!("node-v{version}-linux-armv7l/");
        let bin_dir = format!("{dir}bin/");
        let npm = format!("{dir}bin/npm");
        let changelog = format!("{dir}CHANGELOG.md");
        let script = node_script(answers);
        tar_gz(&[
            (&dir, TarItem::Dir),
            (&changelog, TarItem::File("changes")),
            (&bin_dir, TarItem::Dir),
            (&npm, TarItem::Symlink("../lib/node_modules/npm/bin/npm-cli.js")),
            (&node_entry(version), TarItem::File(&script)),
        ])
    }

    /// Publishes `tarball` as the armv7l build of `version`, in a list
    /// signed by the test key (with another platform's line, as the real
    /// list has).
    fn publish_node(fetch: &FakeFetch, version: &str, tarball: &[u8]) {
        let asset = node_asset(version);
        let sums = sums_for(&[(&format!("node-v{version}-linux-arm64.tar.gz"), b"arm64"), (&asset, tarball)]);
        fetch.serve(&format!("{NODE_BASE}{NODE_SIG}"), Ok(sign(&sums)));
        fetch.serve(&format!("{NODE_BASE}{NODE_SUMS}"), Ok(sums));
        fetch.serve(&format!("{NODE_BASE}{asset}"), Ok(tarball.to_vec()));
    }

    fn keys() -> Vec<SignedPublicKey> {
        vec![public(other_key()), public(test_key())]
    }

    fn node_err(cause: RuntimeCause) -> RuntimeError {
        RuntimeError { runtime: JsRuntime::Node, cause }
    }

    async fn installed_node(data: &Path, version: &str) {
        let fetch = FakeFetch::default();
        publish_node(&fetch, version, &node_tarball(version, version));
        assert_eq!(install_node(&fetch, data, "arm", &keys(), None, None).await, Ok(NodeInstall::Installed(version.into())));
    }

    /// Whatever failed, the node in place answers as before and nothing
    /// staged is left.
    async fn assert_node_still(data: &Path, version: &str) {
        assert_eq!(own_node_version(data).await.as_deref(), Some(version));
        assert!(!data.join("bin/node.new").exists(), "a staged node.new was left");
    }

    /// The real list of `latest-v22.x`, read the way the install reads it.
    #[test]
    fn the_real_list_names_one_armv7l_node_22_and_its_sum() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sums/node-v22.23.4/SHASUMS256.txt");
        let sums = std::fs::read(p).unwrap();
        let (version, asset) = node_release(&sums).unwrap();
        assert_eq!((version.as_str(), asset.as_str()), ("22.23.4", "node-v22.23.4-linux-armv7l.tar.gz"));
        // The .tar.xz next to it is not taken, nor the arm64 build.
        assert_eq!(
            crate::tool::verify::to_hex(&listed_sum(&sums, &asset).unwrap()),
            "4318e0c68f62e18583dae85eb645f55c7f2b8a2e8d1fffe91a2e8abd046880ab"
        );
    }

    #[test]
    fn a_list_without_exactly_one_armv7l_node_22_is_refused() {
        let one = |v: &str| (node_asset(v), b"t".to_vec());
        let list = |entries: &[(String, Vec<u8>)]| {
            let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
            sums_for(&refs)
        };
        assert_eq!(node_release(&list(&[("node-v22.1.0-linux-arm64.tar.gz".into(), b"x".to_vec())])), Err(RuntimeCause::NotListed));
        assert!(matches!(node_release(&list(&[one("23.0.0")])), Err(RuntimeCause::Unexpected(_))));
        assert!(matches!(node_release(&list(&[one("22.1.0"), one("22.2.0")])), Err(RuntimeCause::Unexpected(_))));
        for odd in ["22.1", "22.1.0.1", "22.1.x", "22..1"] {
            assert!(matches!(node_release(&list(&[one(odd)])), Err(RuntimeCause::Unexpected(_))), "{odd}");
        }
        assert_eq!(node_release(&list(&[one("22.10.3")])), Ok(("22.10.3".into(), node_asset("22.10.3"))));
    }

    #[tokio::test]
    async fn node_is_installed_from_its_signed_list_and_comes_before_the_systems() {
        let sys = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write_script(sys.path(), "node", "echo v22.1.0");
        installed_node(data.path(), "22.5.1").await;
        assert_node_still(data.path(), "22.5.1").await;
        // Only the binary was written: the tarball's other entries are not.
        let bin: Vec<_> = std::fs::read_dir(data.path().join("bin")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(bin, vec![std::ffi::OsString::from("node")]);
        let caps = detect(&env(vec![sys.path().into()], data.path(), "arm")).await;
        assert_eq!((caps.js_runtime, caps.own_node), (Some(JsRuntime::Node), true));
        assert_eq!(caps.runtime_arg, Some(format!("node:{}", own_node(data.path()).display())));
    }

    #[tokio::test]
    async fn the_own_node_is_used_only_on_armv7() {
        let sys = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write_script(sys.path(), "node", "echo v22.1.0");
        installed_node(data.path(), "22.5.1").await;
        for arch in ["aarch64", "riscv64"] {
            let caps = detect(&env(vec![sys.path().into()], data.path(), arch)).await;
            assert_eq!(caps.runtime_arg, Some(format!("node:{}", sys.path().join("node").display())), "{arch}");
            assert!(!caps.own_node, "{arch}");
        }
        // An own node older than yt-dlp's minimum is no runtime.
        write_script(&data.path().join("bin"), "node", "echo v20.19.2");
        let caps = detect(&env(vec![sys.path().into()], data.path(), "arm")).await;
        assert_eq!((caps.runtime_arg, caps.own_node), (Some(format!("node:{}", sys.path().join("node").display())), false));
    }

    #[tokio::test]
    async fn no_node_is_ever_fetched_off_armv7() {
        let data = tempfile::tempdir().unwrap();
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.5.1", &node_tarball("22.5.1", "22.5.1"));
        for arch in ["x86_64", "aarch64", "riscv64"] {
            let r = install_node(&fetch, data.path(), arch, &keys(), None, None).await;
            assert_eq!(r, Err(node_err(RuntimeCause::NoBuild(arch.into()))));
        }
        assert!(fetch.asked().is_empty(), "{:?}", fetch.asked());
    }

    #[tokio::test]
    async fn the_listed_version_already_installed_downloads_nothing_more() {
        let data = tempfile::tempdir().unwrap();
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.5.1", &node_tarball("22.5.1", "22.5.1"));
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.5.1"), None).await;
        assert_eq!(r, Ok(NodeInstall::Current("22.5.1".into())));
        assert_eq!(fetch.asked(), vec![format!("{NODE_BASE}{NODE_SUMS}"), format!("{NODE_BASE}{NODE_SIG}")]);
        // Another installed version: the tarball is fetched.
        fetch.forget();
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.4.0"), None).await;
        assert_eq!(r, Ok(NodeInstall::Installed("22.5.1".into())));
        assert!(fetch.asked().contains(&format!("{NODE_BASE}{}", node_asset("22.5.1"))));
    }

    #[tokio::test]
    async fn a_newer_listed_node_replaces_the_installed_one() {
        let data = tempfile::tempdir().unwrap();
        installed_node(data.path(), "22.5.1").await;
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.6.0", &node_tarball("22.6.0", "22.6.0"));
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.5.1"), None).await;
        assert_eq!(r, Ok(NodeInstall::Installed("22.6.0".into())));
        assert_node_still(data.path(), "22.6.0").await;
    }

    fn rollback(installed: &str, listed: &str) -> Result<NodeInstall, RuntimeError> {
        Err(node_err(RuntimeCause::Rollback { installed: installed.into(), listed: listed.into() }))
    }

    /// An old list, validly signed, replayed: refused before the tarball, the
    /// node in place kept. 22.9.5 is older than 22.10.0 by number, though not
    /// as text.
    #[tokio::test]
    async fn an_older_listed_node_is_refused_and_the_installed_one_kept() {
        let data = tempfile::tempdir().unwrap();
        installed_node(data.path(), "22.10.0").await;
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.9.5", &node_tarball("22.9.5", "22.9.5"));
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.10.0"), None).await;
        assert_eq!(r, rollback("22.10.0", "22.9.5"));
        assert_eq!(fetch.asked(), vec![format!("{NODE_BASE}{NODE_SUMS}"), format!("{NODE_BASE}{NODE_SIG}")]);
        assert_node_still(data.path(), "22.10.0").await;
        // The same with the record agreeing.
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.10.0"), Some("22.10.0")).await;
        assert_eq!(r, rollback("22.10.0", "22.9.5"));
        assert_node_still(data.path(), "22.10.0").await;
    }

    /// The node in place older than the highest recorded (put back by hand,
    /// or an earlier rollback): the record is the floor, compared by number.
    #[tokio::test]
    async fn the_recorded_highest_node_is_the_floor_when_it_is_above_the_installed_one() {
        let data = tempfile::tempdir().unwrap();
        installed_node(data.path(), "22.5.1").await;
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.6.0", &node_tarball("22.6.0", "22.6.0"));
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.5.1"), Some("22.10.0")).await;
        assert_eq!(r, rollback("22.10.0", "22.6.0"));
        assert_node_still(data.path(), "22.5.1").await;
    }

    /// `bin/node` deleted: the record alone still refuses an older list, and
    /// takes the recorded version itself again (a repair).
    #[tokio::test]
    async fn with_no_node_in_place_the_record_still_refuses_an_older_list() {
        let data = tempfile::tempdir().unwrap();
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.9.5", &node_tarball("22.9.5", "22.9.5"));
        let r = install_node(&fetch, data.path(), "arm", &keys(), None, Some("22.10.0")).await;
        assert_eq!(r, rollback("22.10.0", "22.9.5"));
        assert!(!own_node(data.path()).exists());
        assert_eq!(fetch.asked().len(), 2, "nothing past the list: {:?}", fetch.asked());

        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.10.0", &node_tarball("22.10.0", "22.10.0"));
        let r = install_node(&fetch, data.path(), "arm", &keys(), None, Some("22.10.0")).await;
        assert_eq!(r, Ok(NodeInstall::Installed("22.10.0".into())));
        assert_node_still(data.path(), "22.10.0").await;
    }

    #[tokio::test]
    async fn a_node_list_signed_by_an_unknown_key_is_refused_before_the_tarball() {
        let data = tempfile::tempdir().unwrap();
        installed_node(data.path(), "22.5.1").await;
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.6.0", &node_tarball("22.6.0", "22.6.0"));
        let sums = fetch.answers.lock().unwrap()[&format!("{NODE_BASE}{NODE_SUMS}")].clone().unwrap();
        fetch.serve(&format!("{NODE_BASE}{NODE_SIG}"), Ok(sign_with(rsa_key(), HashAlgorithm::Sha512, &sums)));
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.5.1"), None).await;
        assert!(matches!(r, Err(RuntimeError { cause: RuntimeCause::BadSignature(_), .. })), "{r:?}");
        assert_eq!(fetch.asked().len(), 2, "nothing past the list: {:?}", fetch.asked());
        assert_node_still(data.path(), "22.5.1").await;
    }

    #[tokio::test]
    async fn a_tarball_that_does_not_match_its_signed_sum_is_refused() {
        let data = tempfile::tempdir().unwrap();
        installed_node(data.path(), "22.5.1").await;
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.6.0", &node_tarball("22.6.0", "22.6.0"));
        fetch.serve(&format!("{NODE_BASE}{}", node_asset("22.6.0")), Ok(node_tarball("22.6.0", "22.6.0-tampered")));
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.5.1"), None).await;
        assert_eq!(r, Err(node_err(RuntimeCause::Mismatch)));
        assert_node_still(data.path(), "22.5.1").await;
    }

    /// Publishes a tarball of `items` for 22.6.0 and installs it over 22.5.1.
    async fn install_shaped(items: &[(&str, TarItem<'_>)]) -> (tempfile::TempDir, Result<NodeInstall, RuntimeError>) {
        let data = tempfile::tempdir().unwrap();
        installed_node(data.path(), "22.5.1").await;
        let fetch = FakeFetch::default();
        publish_node(&fetch, "22.6.0", &tar_gz(items));
        let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.5.1"), None).await;
        (data, r)
    }

    #[tokio::test]
    async fn a_node_entry_that_is_a_link_is_refused() {
        let entry = node_entry("22.6.0");
        for item in [TarItem::Symlink("/usr/bin/node"), TarItem::HardLink("node-v22.6.0-linux-armv7l/lib/x")] {
            let (data, r) = install_shaped(&[(&entry, item)]).await;
            match r {
                Err(RuntimeError { cause: RuntimeCause::Extract(why), .. }) => assert!(why.ends_with("is a link"), "{why}"),
                other => panic!("{other:?}"),
            }
            assert_node_still(data.path(), "22.5.1").await;
        }
        let (data, r) = install_shaped(&[(&entry, TarItem::Dir)]).await;
        assert!(matches!(r, Err(RuntimeError { cause: RuntimeCause::Extract(_), .. })), "{r:?}");
        assert_node_still(data.path(), "22.5.1").await;
    }

    #[tokio::test]
    async fn a_binary_under_any_other_path_is_not_taken() {
        let script = node_script("22.6.0");
        for path in [
            "node-v22.6.0-linux-armv7l/lib/node",
            "./node-v22.6.0-linux-armv7l/bin/node",
            "node-v22.6.0-linux-armv7l/bin/../bin/node",
            "../node-v22.6.0-linux-armv7l/bin/node",
            "/node-v22.6.0-linux-armv7l/bin/node",
            "node-v22.5.1-linux-armv7l/bin/node",
            "bin/node",
        ] {
            let (data, r) = install_shaped(&[(path, TarItem::File(&script))]).await;
            match r {
                Err(RuntimeError { cause: RuntimeCause::Extract(why), .. }) => assert!(why.ends_with("is not in the archive"), "{path}: {why}"),
                other => panic!("{path}: {other:?}"),
            }
            assert_node_still(data.path(), "22.5.1").await;
            assert!(!data.path().join("node-v22.6.0-linux-armv7l").exists() && !data.path().join("lib").exists(), "{path}");
        }
    }

    #[tokio::test]
    async fn a_binary_that_does_not_answer_its_listed_version_is_refused() {
        for answers in ["23.0.0", "22.5.9", "nothing"] {
            let data = tempfile::tempdir().unwrap();
            installed_node(data.path(), "22.5.1").await;
            let fetch = FakeFetch::default();
            publish_node(&fetch, "22.6.0", &node_tarball("22.6.0", answers));
            let r = install_node(&fetch, data.path(), "arm", &keys(), Some("22.5.1"), None).await;
            assert!(matches!(r, Err(RuntimeError { cause: RuntimeCause::Version(_), .. })), "{answers}: {r:?}");
            assert_node_still(data.path(), "22.5.1").await;
        }
    }

    #[test]
    fn the_unmaintained_flag_needs_the_own_node_and_the_date() {
        let own = Capabilities { js_runtime: Some(JsRuntime::Node), own_node: true, ..Capabilities::default() };
        let system = Capabilities { own_node: false, ..own.clone() };
        assert!(!unmaintained(&own, NODE22_END_OF_LIFE - 1));
        assert!(unmaintained(&own, NODE22_END_OF_LIFE));
        assert!(!unmaintained(&system, NODE22_END_OF_LIFE + 1));
        // 2027-04-30 is the last maintained day.
        assert_eq!(NODE22_END_OF_LIFE % 86_400, 0);
        assert_eq!(NODE22_END_OF_LIFE / 86_400, days_from_epoch(2027, 5, 1));
    }

    /// Days from 1970-01-01 to a date of the proleptic Gregorian calendar.
    fn days_from_epoch(y: i64, m: i64, d: i64) -> u64 {
        // Howard Hinnant's days_from_civil.
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        (era * 146_097 + doe - 719_468) as u64
    }

    #[test]
    fn every_runtime_fault_names_its_runtime() {
        let f = node_err(RuntimeCause::Write("x".into())).fault();
        assert_eq!((f.code, f.params["runtime"].as_str(), f.params["detail"].as_str()), ("write", "node", "x"));
        let f = RuntimeError { runtime: JsRuntime::Deno, cause: RuntimeCause::NoBuild("arm".into()) }.fault();
        assert_eq!((f.code, f.params["runtime"].as_str(), f.params["arch"].as_str()), ("no_build", "deno", "arm"));
        let f = node_err(RuntimeCause::Rollback { installed: "22.10.0".into(), listed: "22.9.5".into() }).fault();
        assert_eq!(
            (f.code, f.params["runtime"].as_str(), f.params["installed"].as_str(), f.params["listed"].as_str()),
            ("rollback", "node", "22.10.0", "22.9.5")
        );
    }

    /// Manual: the real armv7l tarball (`NODE_REAL_TARBALL`, downloaded from
    /// `latest-v22.x`), checked against the real signed list and unpacked
    /// the way the install does, without running it (another machine).
    #[test]
    #[ignore = "manual: needs the real 25 MB tarball, see the doc comment"]
    fn manual_unpack_of_the_real_node_tarball() {
        let tarball = std::fs::read(std::env::var("NODE_REAL_TARBALL").unwrap()).unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sums/node-v22.23.4");
        let (sums, sig) = (std::fs::read(dir.join("SHASUMS256.txt")).unwrap(), std::fs::read(dir.join("SHASUMS256.txt.sig")).unwrap());
        check_signature_by_any(&sums, &sig, &crate::tool::verify::node_keys().unwrap()).unwrap();
        let (version, asset) = node_release(&sums).unwrap();
        assert_eq!(sha256(&tarball), listed_sum(&sums, &asset).unwrap());
        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join("node.new");
        let started = std::time::Instant::now();
        extract_tar_gz_entry(&tarball, &node_entry(&version), &dest, NODE_MAX_BINARY).unwrap();
        eprintln!("unpacked in {:?}", started.elapsed());
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 107_563_500);
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 1);
    }
}
