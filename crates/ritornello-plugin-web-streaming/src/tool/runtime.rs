//! What the machine offers yt-dlp: python3 (the plugin's one system
//! prerequisite) and, for the wrapper's second attempt only, a JavaScript
//! runtime.
//!
//! The runtime order, and why (measured on the device, armv7l):
//! 1. the plugin's own deno, only on x86_64 and aarch64: deno publishes
//!    nothing for 32-bit ARM;
//! 2. a system `node` of at least 22.0.0, yt-dlp's own minimum
//!    (`yt_dlp/utils/_jsruntime.py`; Debian 13's node 20.19.2 is refused);
//! 3. a system `qjs` (QuickJS, at least 2023-12-09; QuickJS-ng any
//!    version), marked slow: ~125 s per video;
//! 4. none.
//!
//! Deno does not sign its releases. Its zip is checked against the
//! `.sha256sum` published next to it, in the same release: that catches a
//! damaged download, not a compromised release. That is the whole of deno's
//! trust chain here, and why it is only ever a fallback.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::fetch::{Fetch, Limit};
use super::install::{extract_one, write_executable};
use super::verify::{listed_sum, sha256};

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
    /// The exact command that would fix what is missing, when one exists.
    pub apt_hint: Option<String>,
    /// `<name>:<absolute path>`, as `--js-runtimes` takes it.
    #[serde(skip)]
    pub runtime_arg: Option<String>,
    /// The python3 that answered: the wrapper's interpreter.
    #[serde(skip)]
    pub python3_path: Option<PathBuf>,
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

async fn output_of(bin: &Path, arg: &str) -> Option<String> {
    let (_, out, err) = capture(bin, &[arg.to_string()], PROBE_DEADLINE).await.ok()?;
    Some(format!("{out}\n{err}"))
}

fn first_word_after(text: &str, prefix: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix(prefix).and_then(|r| r.split_whitespace().next()).map(str::to_string))
}

/// python3 and its version, when it answers.
pub async fn python3(search: &[PathBuf]) -> Option<(PathBuf, String)> {
    let bin = which("python3", search)?;
    let version = first_word_after(&output_of(&bin, "--version").await?, "Python ")?;
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

pub fn own_deno(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("deno")
}

async fn usable_deno(env: &Env) -> Option<PathBuf> {
    deno_asset(&env.arch)?;
    let bin = own_deno(&env.data_dir);
    let text = output_of(&bin, "--version").await?;
    at_least(&first_word_after(&text, "deno ")?, &[2, 3, 0]).then_some(bin)
}

async fn usable_node(search: &[PathBuf]) -> Option<PathBuf> {
    let bin = which("node", search)?;
    let text = output_of(&bin, "--version").await?;
    let v = text.lines().find_map(|l| l.trim().strip_prefix('v').map(str::to_string))?;
    at_least(&v, &[22, 0, 0]).then_some(bin)
}

async fn usable_quickjs(search: &[PathBuf]) -> Option<PathBuf> {
    let bin = which("qjs", search)?;
    let text = output_of(&bin, "--help").await?;
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
    let found = if let Some(p) = usable_deno(env).await {
        Some((JsRuntime::Deno, p))
    } else if let Some(p) = usable_node(&env.search).await {
        Some((JsRuntime::Node, p))
    } else {
        usable_quickjs(&env.search).await.map(|p| (JsRuntime::Quickjs, p))
    };
    Capabilities {
        python3,
        js_runtime: found.as_ref().map(|(r, _)| *r),
        js_runtime_slow: matches!(found, Some((JsRuntime::Quickjs, _))),
        apt_hint,
        runtime_arg: found.map(|(r, p)| format!("{}:{}", r.name(), p.display())),
        python3_path,
    }
}

pub const DENO_BASE: &str = "https://github.com/denoland/deno/releases/latest/download/";
const DENO_ZIP: Limit = Limit { max_bytes: 256 << 20, deadline: Duration::from_secs(900) };
const DENO_SUM: Limit = Limit { max_bytes: 4096, deadline: Duration::from_secs(30) };
const DENO_MAX_BINARY: u64 = 512 << 20;

/// Downloads deno for `arch`, checks it against its published sum, and puts
/// it at `bin/deno`. Refused on a machine deno publishes nothing for.
/// Why the plugin's own deno could not be installed. A variant per thing the
/// plugin says; what the network or the system said stays raw inside.
#[derive(Debug, Clone, PartialEq)]
pub enum DenoError {
    /// Deno publishes no build for this machine (the architecture).
    NoBuild(String),
    Fetch(String),
    NotListed,
    Mismatch,
    Extract(String),
    Write(String),
}

impl std::fmt::Display for DenoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DenoError::NoBuild(arch) => write!(f, "no deno for {arch}"),
            DenoError::Fetch(e) => write!(f, "deno: download failed: {e}"),
            DenoError::NotListed => write!(f, "deno: the zip is not in the published sums"),
            DenoError::Mismatch => write!(f, "deno: the zip does not match its published sum"),
            DenoError::Extract(e) => write!(f, "deno: {e}"),
            DenoError::Write(e) => write!(f, "deno: {e}"),
        }
    }
}

impl DenoError {
    /// The code the page translates (`runtime_<code>` keys).
    pub fn fault(&self) -> crate::tool::UpdateFault {
        use crate::tool::fault_with;
        match self {
            DenoError::NoBuild(arch) => fault_with("no_build", &[("arch", arch)]),
            DenoError::Fetch(e) => fault_with("fetch", &[("detail", e)]),
            DenoError::NotListed => fault_with("not_listed", &[]),
            DenoError::Mismatch => fault_with("mismatch", &[]),
            DenoError::Extract(e) => fault_with("extract", &[("detail", e)]),
            DenoError::Write(e) => fault_with("write", &[("detail", e)]),
        }
    }
}

pub async fn install_deno(fetch: &dyn Fetch, data_dir: &Path, arch: &str) -> Result<(), DenoError> {
    let asset = deno_asset(arch).ok_or_else(|| DenoError::NoBuild(arch.to_string()))?;
    let sums = fetch.get(&format!("{DENO_BASE}{asset}.sha256sum"), DENO_SUM).await.map_err(DenoError::Fetch)?;
    let want = listed_sum(&sums, asset).map_err(|_| DenoError::NotListed)?;
    let zip = fetch.get(&format!("{DENO_BASE}{asset}"), DENO_ZIP).await.map_err(DenoError::Fetch)?;
    if sha256(&zip) != want {
        return Err(DenoError::Mismatch);
    }
    let binary = tokio::task::spawn_blocking(move || extract_one(&zip, "deno", DENO_MAX_BINARY))
        .await
        .map_err(|e| DenoError::Extract(e.to_string()))?
        .map_err(DenoError::Extract)?;
    write_executable(&own_deno(data_dir), &binary).map_err(|e| DenoError::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::testkit::*;

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
        assert_eq!(err, DenoError::Mismatch);
        assert!(!own_deno(data.path()).exists());
    }

    #[tokio::test]
    async fn no_deno_is_ever_fetched_for_armv7() {
        let data = tempfile::tempdir().unwrap();
        let fetch = FakeFetch::default();
        assert!(install_deno(&fetch, data.path(), "arm").await.is_err());
        assert!(fetch.asked().is_empty());
    }
}
