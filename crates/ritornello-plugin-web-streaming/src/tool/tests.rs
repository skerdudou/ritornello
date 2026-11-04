use super::testkit::*;
use super::*;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const T0: u64 = 1_800_000_000;

struct World {
    data: tempfile::TempDir,
    _search: tempfile::TempDir,
    fetch: Arc<FakeFetch>,
    clock: Arc<AtomicU64>,
    gate: ToolGate,
    tool: Tool,
}

/// The tag the tests' `latest` redirects to.
const TAG: &str = "rel-1";

fn url(channel: Channel, name: &str) -> String {
    format!("{}{name}", channel.download_base(TAG))
}

/// Serves a release of `asset` on `channel` under [`TAG`], its list signed
/// by the test key, and `latest` redirecting to it.
fn publish(fetch: &FakeFetch, channel: Channel, asset: &[u8]) {
    fetch.serve(&channel.latest_url(), Ok(format!("{}tag/{TAG}", channel.releases()).into_bytes()));
    let sums = sums_for(&[("yt-dlp.exe", b"windows"), (ASSET, asset)]);
    fetch.serve(&url(channel, SIG), Ok(sign(&sums)));
    fetch.serve(&url(channel, SUMS), Ok(sums));
    fetch.serve(&url(channel, ASSET), Ok(asset.to_vec()));
}

fn world() -> World {
    let (search_guard, search) = python_only_path();
    world_with(vec![search], search_guard)
}

fn world_with(search: Vec<PathBuf>, search_guard: tempfile::TempDir) -> World {
    // A machine the plugin installs no runtime of its own for: no deno or
    // node download in these tests unless one asks for it.
    world_on("riscv64", search, search_guard)
}

fn world_on(arch: &str, search: Vec<PathBuf>, search_guard: tempfile::TempDir) -> World {
    let data = tempfile::tempdir().unwrap();
    let fetch = Arc::new(FakeFetch::default());
    let clock = Arc::new(AtomicU64::new(T0));
    let gate = ToolGate::default();
    let c = clock.clone();
    let deps = Deps {
        fetch: fetch.clone(),
        now: Arc::new(move || c.load(Ordering::SeqCst)),
        search,
        arch: arch.into(),
        gate: gate.clone(),
        key: public(test_key()),
        node_keys: vec![public(test_key())],
    };
    let tool = Tool::with_deps(data.path(), deps);
    World { data, _search: search_guard, fetch, clock, gate, tool }
}

/// The dependencies of `w`'s tool again, for a plugin restarted on the same
/// data directory.
fn deps_of(w: &World, arch: &str) -> Deps {
    let c = w.clock.clone();
    Deps {
        fetch: w.fetch.clone(),
        now: Arc::new(move || c.load(Ordering::SeqCst)),
        search: w.tool.inner.deps.search.clone(),
        arch: arch.into(),
        gate: w.gate.clone(),
        key: public(test_key()),
        node_keys: vec![public(test_key())],
    }
}

/// What the installed wrapper answers to `--version`.
fn wrapper_version(w: &World) -> String {
    let out = retrying(|| Command::new(w.data.path().join("bin/yt-dlp")).arg("--version").output());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

async fn installed_v1(w: &World) {
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.01.01"));
    let got = w.tool.update(false).await.unwrap();
    assert_eq!(got, Update::Installed(Installed { version: "2026.01.01".into(), channel: Channel::Stable }));
    w.fetch.forget();
}

fn assert_still_v1(w: &World) {
    assert_eq!(wrapper_version(w), "2026.01.01");
    assert_eq!(w.tool.status().version.as_deref(), Some("2026.01.01"));
    assert!(!w.data.path().join(install::NEW_DIR).exists(), "nothing staged is left");
}

#[tokio::test]
async fn a_valid_signature_and_sum_install() {
    let w = world();
    assert!(!w.tool.status().installed);
    installed_v1(&w).await;
    assert_eq!(wrapper_version(&w), "2026.01.01");
    let st = w.tool.status();
    assert!(st.installed && !st.updating);
    assert_eq!((st.error, st.last_check), (None, Some(T0)));
    let s = Settings::load(&w.data.path().join("settings.toml"));
    let rec = s.installed.unwrap();
    assert_eq!((rec.version.as_str(), rec.channel), ("2026.01.01", Channel::Stable));
    assert_eq!(rec.sha256, verify::to_hex(&verify::sha256(&fake_zipapp("2026.01.01"))));
    // The conf puts yt-dlp's cache under the data directory.
    let conf = std::fs::read_to_string(w.data.path().join("yt-dlp.conf")).unwrap();
    assert!(conf.contains(&format!("--cache-dir '{}'", w.data.path().join("cache").display())), "{conf}");
    // The bytecode was compiled where the package lives.
    assert!(w.data.path().join("yt-dlp/yt_dlp/__pycache__").is_dir());
}

#[tokio::test]
async fn a_newer_release_replaces_the_installed_one() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    let got = w.tool.update(true).await.unwrap();
    assert_eq!(got, Update::Installed(Installed { version: "2026.02.02".into(), channel: Channel::Stable }));
    assert_eq!(wrapper_version(&w), "2026.02.02");
    assert!(!w.data.path().join(install::OLD_DIR).exists());
}

#[tokio::test]
async fn a_bad_signature_is_refused_and_keeps_the_old_copy() {
    let w = world();
    installed_v1(&w).await;
    let v2 = fake_zipapp("2026.02.02");
    let sums = sums_for(&[(ASSET, &v2)]);
    w.fetch.serve(&url(Channel::Stable, SUMS), Ok(sums.clone()));
    w.fetch.serve(&url(Channel::Stable, ASSET), Ok(v2));
    for sig in [sign_with(other_key(), pgp::crypto::hash::HashAlgorithm::Sha512, &sums), sign(b"another list")] {
        w.fetch.serve(&url(Channel::Stable, SIG), Ok(sig));
        let r = w.tool.update(true).await;
        assert!(matches!(r, Err(UpdateError::Refused(VerifyError::BadSignature(_)))), "{r:?}");
        assert_still_v1(&w);
        assert!(w.tool.status().error.unwrap().contains("bad signature"));
        assert!(!w.tool.status().forceable, "no \"install anyway\" for a release the key does not vouch for");
        assert!(!w.fetch.asked().contains(&url(Channel::Stable, ASSET)), "nothing fetched past a bad signature");
    }
}

#[tokio::test]
async fn an_asset_not_listed_is_refused() {
    let w = world();
    installed_v1(&w).await;
    let sums = sums_for(&[("yt-dlp.exe", b"x"), ("yt-dlp_linux", b"y")]);
    w.fetch.serve(&url(Channel::Stable, SIG), Ok(sign(&sums)));
    w.fetch.serve(&url(Channel::Stable, SUMS), Ok(sums));
    let r = w.tool.update(true).await;
    assert_eq!(r, Err(UpdateError::Refused(VerifyError::NotListed)));
    assert_still_v1(&w);
    assert!(!w.fetch.asked().contains(&url(Channel::Stable, ASSET)));
}

#[tokio::test]
async fn a_wrong_sum_is_refused() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    // The asset served is not the one the signed list names.
    w.fetch.serve(&url(Channel::Stable, ASSET), Ok(fake_zipapp("2026.06.06")));
    let r = w.tool.update(true).await;
    assert_eq!(r, Err(UpdateError::Refused(VerifyError::Mismatch)));
    assert_still_v1(&w);
}

#[tokio::test]
async fn a_failed_download_keeps_the_old_copy() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    w.fetch.serve(&url(Channel::Stable, ASSET), Err("connection reset".into()));
    w.clock.store(T0 + DAY + 1, Ordering::SeqCst);
    let r = w.tool.update(false).await;
    assert!(matches!(r, Err(UpdateError::Fetch(_))), "{r:?}");
    assert_still_v1(&w);
    // No verdict on the release: the next tick tries again.
    assert_eq!(w.tool.status().last_check, Some(T0));
    assert!(w.tool.due());
}

#[tokio::test]
async fn updates_at_most_once_a_day_unless_forced() {
    let w = world();
    installed_v1(&w).await;
    w.clock.store(T0 + DAY - 1, Ordering::SeqCst);
    assert!(!w.tool.due());
    assert_eq!(w.tool.update(false).await, Ok(Update::Skipped));
    assert!(w.fetch.asked().is_empty(), "nothing asked within the day");

    // Forced: the page's button.
    let r = w.tool.update(true).await.unwrap();
    assert!(matches!(r, Update::Current(_)), "{r:?}");
    assert!(!w.fetch.asked().is_empty());
    assert_eq!(w.tool.status().last_check, Some(T0 + DAY - 1));

    // A day after that check, unforced.
    w.fetch.forget();
    w.clock.store(T0 + 2 * DAY - 1, Ordering::SeqCst);
    assert!(w.tool.due());
    assert!(matches!(w.tool.update(false).await, Ok(Update::Current(_))));
    assert!(!w.fetch.asked().is_empty());
}

#[tokio::test]
async fn an_unchanged_release_is_not_downloaded_again() {
    let w = world();
    installed_v1(&w).await;
    let r = w.tool.update(true).await.unwrap();
    assert_eq!(r, Update::Current(Installed { version: "2026.01.01".into(), channel: Channel::Stable }));
    assert_eq!(w.fetch.asked(), vec![Channel::Stable.latest_url(), url(Channel::Stable, SUMS), url(Channel::Stable, SIG)]);
}

#[tokio::test]
async fn a_damaged_install_is_installed_again_even_if_unchanged() {
    let w = world();
    installed_v1(&w).await;
    std::fs::remove_dir_all(w.data.path().join("yt-dlp")).unwrap();
    assert!(!w.tool.status().installed && w.tool.due());
    assert!(matches!(w.tool.update(false).await, Ok(Update::Installed(_))));
    assert_still_v1(&w);
}

#[tokio::test]
async fn switching_to_nightly_installs_from_the_nightly_builds() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Nightly, &fake_zipapp("2026.01.01.123456"));
    w.tool.edit_settings(|s| s.channel = Channel::Nightly).unwrap();
    let r = w.tool.update(true).await.unwrap();
    assert_eq!(r, Update::Installed(Installed { version: "2026.01.01.123456".into(), channel: Channel::Nightly }));
    assert!(w.fetch.asked().iter().all(|u| u.starts_with(Channel::Nightly.releases())), "{:?}", w.fetch.asked());
    // Back to stable: an older version, but the operator changed channel.
    w.tool.edit_settings(|s| s.channel = Channel::Stable).unwrap();
    assert!(matches!(w.tool.update(true).await, Ok(Update::Installed(_))));
    assert_still_v1(&w);
}

#[tokio::test]
async fn the_same_bytes_on_the_other_channel_are_not_downloaded_again() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Nightly, &fake_zipapp("2026.01.01"));
    w.tool.edit_settings(|s| s.channel = Channel::Nightly).unwrap();
    let r = w.tool.update(true).await.unwrap();
    assert_eq!(r, Update::Current(Installed { version: "2026.01.01".into(), channel: Channel::Nightly }));
    assert!(!w.fetch.asked().contains(&url(Channel::Nightly, ASSET)));
    assert_eq!(w.tool.settings().installed.unwrap().channel, Channel::Nightly, "the channel is recorded");
}

#[tokio::test]
async fn an_extraction_that_fails_half_way_keeps_the_old_copy() {
    let w = world();
    installed_v1(&w).await;
    // A correctly signed archive whose second file is damaged: the first is
    // written, then the CRC check fails.
    let mut v2 = zipapp_with(
        &[
            ("yt_dlp/version.py", "__version__ = '2026.02.02'\n"),
            ("yt_dlp/__init__.py", "def main(argv=None):\n    print('v2')\n"),
            ("yt_dlp/big.py", &"#".repeat(4000)),
        ],
        zip::CompressionMethod::Stored,
    );
    let at = v2.windows(8).position(|w| w == b"########").unwrap();
    v2[at + 100] = b'!';
    publish(&w.fetch, Channel::Stable, &v2);
    let r = w.tool.update(true).await;
    assert!(matches!(&r, Err(UpdateError::Install(e)) if !e.contains("version.py")), "{r:?}");
    assert_still_v1(&w);
}

#[tokio::test]
async fn a_release_that_does_not_start_is_refused() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp(""));
    let r = w.tool.update(true).await;
    assert!(matches!(&r, Err(UpdateError::Install(e)) if e.contains("does not start")), "{r:?}");
    assert_still_v1(&w);
    // An archive without the package is refused too, before running it,
    // with or without a version.py.
    publish(&w.fetch, Channel::Stable, &zipapp(&[("__main__.py", "print(1)")]));
    let r = w.tool.update(true).await;
    assert!(matches!(&r, Err(UpdateError::Install(e)) if e == "the archive holds no yt_dlp package"), "{r:?}");
    assert_still_v1(&w);
    publish(&w.fetch, Channel::Stable, &zipapp(&[("yt_dlp/version.py", "__version__ = '2026.03.03'\n")]));
    let r = w.tool.update(true).await;
    assert!(matches!(&r, Err(UpdateError::Install(e)) if e == "the archive holds no yt_dlp package"), "{r:?}");
    assert_still_v1(&w);
}

// ---- no rollback --------------------------------------------------------

#[test]
fn the_declared_version_is_read_from_version_py_without_running_it() {
    let real = b"# Autogenerated by devscripts/update-version.py\n\n__version__ = '2026.10.26'\n\nRELEASE_GIT_HEAD = 'x'\n";
    assert_eq!(declared_version(real).as_deref(), Some("2026.10.26"));
    assert_eq!(declared_version(b"__version__ = \"2026.10.29.232701\"\n").as_deref(), Some("2026.10.29.232701"));
    // Annotated, as yt-dlp could write it one day.
    assert_eq!(declared_version(b"__version__: str = '2026.11.01'\n").as_deref(), Some("2026.11.01"));
    assert_eq!(declared_version(b"__version__ : str = \"2026.11.01\"\n").as_deref(), Some("2026.11.01"));
    for bad in [&b"__version__ = 2026\n"[..], b"__version__ = ''\n", b"__version__ = 'x; import os'\n", b"VERSION = '1'\n", b"\xff"] {
        assert_eq!(declared_version(bad), None, "{bad:?}");
    }
}

async fn refused_rollback(w: &World, force: bool) -> Result<Update, UpdateError> {
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2025.12.31"));
    w.tool.update(force).await
}

#[tokio::test]
async fn an_older_release_is_refused() {
    // The scheduler's unforced check is what a replayed old release meets.
    let w = world();
    installed_v1(&w).await;
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    let r = refused_rollback(&w, false).await;
    assert!(matches!(&r, Err(UpdateError::Rollback(e)) if e.contains("older")), "{r:?}");
    assert_still_v1(&w);
    assert!(!w.fetch.asked().is_empty());
    assert_eq!(w.tool.status().last_check, Some(T0 + DAY), "a verdict: not asked again before a day");
    assert!(w.tool.status().error.unwrap().contains("forced update"), "the page says the way out");
    assert!(w.tool.status().forceable, "and offers it");
}

#[tokio::test]
async fn an_older_release_is_taken_when_forced() {
    let w = world();
    installed_v1(&w).await;
    let r = refused_rollback(&w, true).await;
    assert_eq!(r, Ok(Update::Installed(Installed { version: "2025.12.31".into(), channel: Channel::Stable })));
}

#[tokio::test]
async fn an_older_release_is_taken_when_the_operator_changed_channel() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Nightly, &fake_zipapp("2025.12.31.000001"));
    w.tool.edit_settings(|s| s.channel = Channel::Nightly).unwrap();
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    let r = w.tool.update(false).await;
    assert!(matches!(r, Ok(Update::Installed(_))), "{r:?}");
}

#[tokio::test]
async fn a_newer_release_is_taken_unforced() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.01.02"));
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    assert!(matches!(w.tool.update(false).await, Ok(Update::Installed(_))));
    assert_eq!(wrapper_version(&w), "2026.01.02");
}

/// Another build signed under the installed number: refused unforced (a
/// re-signed release is not the plugin's to trust alone); the operator's
/// forced update is the way out after a legitimate re-publication.
#[tokio::test]
async fn another_build_under_the_installed_number_needs_the_operator() {
    let w = world();
    installed_v1(&w).await;
    let mut files = fake_package("2026.01.01");
    files.push(("yt_dlp/rebuilt.py", "# another build\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    publish(&w.fetch, Channel::Stable, &zipapp(&refs));
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    let r = w.tool.update(false).await;
    assert!(matches!(&r, Err(UpdateError::Rollback(e)) if e.contains("another build")), "{r:?}");
    assert!(!w.data.path().join("yt-dlp/yt_dlp/rebuilt.py").exists());
    assert!(matches!(w.tool.update(true).await, Ok(Update::Installed(_))));
    assert!(w.data.path().join("yt-dlp/yt_dlp/rebuilt.py").exists());
}

/// Nothing is protected when nothing works: the first install, and the
/// reinstall of a damaged copy, take whatever is signed.
#[tokio::test]
async fn a_damaged_install_takes_an_older_release() {
    let w = world();
    installed_v1(&w).await;
    std::fs::remove_dir_all(w.data.path().join("yt-dlp")).unwrap();
    let r = refused_rollback(&w, false).await;
    assert!(matches!(r, Ok(Update::Installed(_))), "{r:?}");
    assert_eq!(wrapper_version(&w), "2025.12.31");
}

/// A release whose `version.py` holds `text` instead of the usual line.
fn with_version_py(version: &str, text: &str) -> Vec<u8> {
    let mut files = fake_package(version);
    for f in files.iter_mut().filter(|(n, _)| *n == "yt_dlp/version.py") {
        f.1 = text.to_string();
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    zipapp(&refs)
}

/// A rollback guard must never freeze a fresh device: with no comparison
/// due, an unreadable version.py does not matter, and the version recorded
/// is the one the warm-up answers.
#[tokio::test]
async fn a_first_install_takes_a_release_whose_version_py_cannot_be_read() {
    let w = world();
    publish(&w.fetch, Channel::Stable, &with_version_py("2026.01.01", "__version__ = compute()\n"));
    let r = w.tool.update(false).await;
    assert_eq!(r, Ok(Update::Installed(Installed { version: "2026.01.01".into(), channel: Channel::Stable })));
    assert_eq!(wrapper_version(&w), "2026.01.01");
}

/// When a comparison is due and cannot be made, the release is refused
/// unforced, and taken when the operator accepts it anyway.
#[tokio::test]
async fn an_unreadable_version_py_on_the_same_channel_needs_the_operator() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &with_version_py("2026.02.02", "VERSION = '2026.02.02'\n"));
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    let r = w.tool.update(false).await;
    assert!(matches!(&r, Err(UpdateError::Rollback(e)) if e.contains("cannot be read")), "{r:?}");
    assert!(w.tool.status().error.unwrap().contains("forced update"), "the page says the way out");
    assert_still_v1(&w);
    assert!(matches!(w.tool.update(true).await, Ok(Update::Installed(_))));
    assert_eq!(wrapper_version(&w), "2026.02.02");
}

/// A reformatted (annotated) version.py is still read, so an older release
/// does not slip through a format change.
#[tokio::test]
async fn an_older_release_with_an_annotated_version_py_is_refused() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &with_version_py("2025.12.31", "__version__: str = '2025.12.31'\n"));
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    let r = w.tool.update(false).await;
    assert!(matches!(&r, Err(UpdateError::Rollback(e)) if e.contains("older")), "{r:?}");
    assert_still_v1(&w);
}

/// The three sentences of `judge_rollback` are read back into codes with
/// the versions they name; a new sentence that is not would show as
/// `rollback_other` with the raw text, which this test refuses.
#[test]
fn every_rollback_refusal_has_its_own_code_and_its_versions() {
    let record = InstalledRecord {
        version: "2026.10.26".into(),
        channel: Channel::Stable,
        sha256: "aa".into(),
        tag: None,
    };
    let refusal = |offered: Option<&str>, sha: &str| {
        let e = judge_rollback(offered, sha, Some(&record), true, Channel::Stable, false).unwrap_err();
        UpdateError::Rollback(e).fault()
    };
    let older = refusal(Some("2025.12.31"), "bb");
    assert_eq!(older.code, "rollback_older");
    assert_eq!(older.params["offered"], "2025.12.31");
    assert_eq!(older.params["installed"], "2026.10.26");
    let other = refusal(Some("2026.10.26"), "bb");
    assert_eq!((other.code, other.params["offered"].as_str()), ("rollback_other_build", "2026.10.26"));
    let unreadable = refusal(None, "bb");
    assert_eq!((unreadable.code, unreadable.params["installed"].as_str()), ("rollback_unreadable", "2026.10.26"));
}

#[test]
fn every_update_error_has_a_code_and_keeps_the_outside_text_as_detail() {
    let cases = [
        (UpdateError::Busy, "busy"),
        (UpdateError::Python3Missing, "python3_missing"),
        (UpdateError::Python3Unusable("/x".into()), "python3_unusable"),
        (UpdateError::Fetch("connection reset".into()), "fetch"),
        (UpdateError::Refused(VerifyError::BadSignature("why".into())), "refused_bad_signature"),
        (UpdateError::Refused(VerifyError::NotListed), "refused_not_listed"),
        (UpdateError::Refused(VerifyError::Mismatch), "refused_mismatch"),
        (UpdateError::Install("swap: denied".into()), "install_swap"),
        (UpdateError::Install("/data/x: denied".into()), "install"),
    ];
    for (error, code) in cases {
        assert_eq!(error.fault().code, code);
    }
    assert_eq!(UpdateError::Fetch("connection reset".into()).fault().params["detail"], "connection reset");
}

/// What the install writes itself is a code of its own, what the system says
/// stays raw as `detail`; the shapes are read back from the real producers.
#[test]
fn the_install_failures_the_plugin_writes_each_have_their_own_code() {
    use super::install::ZipLimits;
    let dir = tempfile::tempdir().unwrap();
    let fault = |e: String| UpdateError::Install(e).fault();
    let zip = fake_zipapp("2026.01.01");

    let e = install::extract(b"junk", &dir.path().join("a"), install::YTDLP_LIMITS).unwrap_err();
    assert_eq!(fault(e).code, "install_not_zip");
    let e = install::extract(&zip, &dir.path().join("b"), ZipLimits { max_entries: 0, max_total: 1 << 20 }).unwrap_err();
    assert_eq!(fault(e).code, "install_extract_limit");
    let e = install::extract(&zip, &dir.path().join("c"), ZipLimits { max_entries: 100, max_total: 1 }).unwrap_err();
    assert_eq!(fault(e).code, "install_extract_limit");

    assert_eq!(fault(NO_PACKAGE.into()).code, "install_no_package");
    let f = fault(format!("{DOES_NOT_START}SyntaxError: x"));
    assert_eq!((f.code, f.params["detail"].as_str()), ("install_does_not_start", "SyntaxError: x"));
    assert_eq!(fault(format!("{NO_VERSION}\"\"")).code, "install_no_version");
    let f = fault(format!("{SWAP}Permission denied (os error 13)"));
    assert_eq!((f.code, f.params["detail"].as_str()), ("install_swap", "Permission denied (os error 13)"));
    assert_eq!(fault(format!("{WRITE}Read-only file system (os error 30)")).code, "install_write");
    assert_eq!(fault(format!("{WRITE}python3 missing")).code, "python3_missing");
    assert_eq!(fault("unsafe name \"../x\"".into()).code, "install_extract_unsafe");
    // The system's own words only: the generic code, raw.
    let f = fault("/data/yt-dlp.new: Permission denied (os error 13)".into());
    assert_eq!((f.code, f.params["detail"].as_str()), ("install", "/data/yt-dlp.new: Permission denied (os error 13)"));
}

/// Every code the page can receive is a key of the English catalogue (and so,
/// by the parity test, of the four languages): `update_<code>` for an update,
/// `runtime_<code>` for the plugin's own runtime (deno or node).
#[test]
fn every_code_of_an_update_or_a_runtime_failure_has_a_key() {
    use super::runtime::{JsRuntime, RuntimeCause, RuntimeError};
    use super::verify::VerifyError;
    let en = ritornello_i18n::try_parse(crate::WEB_STREAMING_EN).unwrap();
    let update_errors = vec![
        UpdateError::Busy,
        UpdateError::Python3Missing,
        UpdateError::Python3Unusable("x".into()),
        UpdateError::Fetch("x".into()),
        UpdateError::Refused(VerifyError::BadSignature("x".into())),
        UpdateError::Refused(VerifyError::NotListed),
        UpdateError::Refused(VerifyError::Mismatch),
        UpdateError::Rollback(format!("2025.1.1{OLDER}2026.1.1")),
        UpdateError::Rollback(format!("{OTHER_BUILD_1}2026.1.1{OTHER_BUILD_2}")),
        UpdateError::Rollback(format!("{UNREADABLE}2026.1.1")),
        UpdateError::Rollback("a sentence of a newer plugin".into()),
        UpdateError::Install(NO_PACKAGE.into()),
        UpdateError::Install(format!("{DOES_NOT_START}x")),
        UpdateError::Install(format!("{NO_VERSION}x")),
        UpdateError::Install(format!("{SWAP}x")),
        UpdateError::Install(format!("{WRITE}x")),
        UpdateError::Install("not a zip archive: x".into()),
        UpdateError::Install("3 entries, more than 2".into()),
        UpdateError::Install("unsafe name \"x\"".into()),
        UpdateError::Install("anything else".into()),
    ];
    for e in update_errors {
        let code = e.fault().code;
        assert!(en.contains_key(&format!("update_{code}")), "update_{code} for {e:?}");
    }
    let causes = [
        RuntimeCause::NoBuild("arm".into()),
        RuntimeCause::Fetch("x".into()),
        RuntimeCause::BadSignature("x".into()),
        RuntimeCause::NotListed,
        RuntimeCause::Unexpected("x".into()),
        RuntimeCause::Mismatch,
        RuntimeCause::Extract("x".into()),
        RuntimeCause::Version("x".into()),
        RuntimeCause::Write("x".into()),
        RuntimeCause::Rollback { installed: "22.2.0".into(), listed: "22.1.0".into() },
    ];
    for runtime in [JsRuntime::Deno, JsRuntime::Node] {
        for cause in causes.clone() {
            let e = RuntimeError { runtime, cause };
            let f = e.fault();
            assert!(en.contains_key(&format!("runtime_{}", f.code)), "runtime_{} for {e:?}", f.code);
            assert_eq!(f.params["runtime"], runtime.name(), "{e:?}");
        }
    }
    assert_eq!(UpdateError::Rollback("something else".into()).fault().code, "rollback_other");
}

/// "Install anyway" is offered after a refusal and withdrawn once the forced
/// update went through: the flag must not outlive the failure it names.
#[tokio::test]
async fn forceable_is_back_to_false_after_a_successful_forced_update() {
    let w = world();
    installed_v1(&w).await;
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    let r = refused_rollback(&w, false).await;
    assert!(matches!(r, Err(UpdateError::Rollback(_))), "{r:?}");
    assert!(w.tool.status().forceable);
    assert_eq!(w.tool.status().error_code.unwrap().code, "rollback_older");

    let r = w.tool.update(true).await;
    assert!(matches!(r, Ok(Update::Installed(_))), "{r:?}");
    let status = w.tool.status();
    assert!(!status.forceable);
    assert_eq!((status.error, status.error_code), (None, None));
}

async fn until_settled(w: &World) {
    let started = std::time::Instant::now();
    while w.tool.status().updating {
        assert!(started.elapsed() < Duration::from_secs(30), "never finished");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The page's two actions: "check now" keeps the rollback guard, "accept
/// anyway" lifts it.
#[tokio::test]
async fn check_now_keeps_the_guard_and_accept_anyway_lifts_it() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2025.12.31"));
    tokio::spawn(schedule(w.tool.clone()));
    w.tool.request_update(false);
    until_settled(&w).await;
    assert!(w.tool.status().error.is_some_and(|e| e.contains("older")), "{:?}", w.tool.status());
    assert_still_v1(&w);
    w.tool.request_update(true);
    until_settled(&w).await;
    assert_eq!(w.tool.status().error, None);
    assert_eq!(wrapper_version(&w), "2025.12.31");
}

/// The hourly tick (its first runs at start) keeps the guard.
#[tokio::test]
async fn the_hourly_tick_keeps_the_guard() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2025.12.31"));
    w.clock.store(T0 + DAY, Ordering::SeqCst);
    tokio::spawn(schedule(w.tool.clone()));
    let started = std::time::Instant::now();
    while w.tool.status().error.is_none() {
        assert!(started.elapsed() < Duration::from_secs(30), "the tick never checked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    until_settled(&w).await;
    assert!(w.tool.status().error.unwrap().contains("older"));
    assert_still_v1(&w);
}

/// "Check now" goes past the daily limit: the check of a moment ago does
/// not make it a no-op.
#[tokio::test]
async fn check_now_goes_past_the_daily_limit() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    assert!(!w.tool.due());
    tokio::spawn(schedule(w.tool.clone()));
    w.tool.request_update(false);
    until_settled(&w).await;
    assert_eq!(wrapper_version(&w), "2026.02.02");
}

#[tokio::test]
async fn a_python3_that_cannot_be_a_shebang_is_refused_before_anything_is_fetched() {
    let long = tempfile::tempdir().unwrap();
    let dir = long.path().join("p".repeat(120));
    std::fs::create_dir(&dir).unwrap();
    std::os::unix::fs::symlink(real_python3(), dir.join("python3")).unwrap();
    let w = world_with(vec![dir], long);
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.01.01"));
    let r = w.tool.update(false).await;
    assert!(matches!(&r, Err(UpdateError::Python3Unusable(_))), "{r:?}");
    assert!(w.fetch.asked().is_empty());
    assert!(w.tool.status().error.unwrap().contains("python3 unusable"));
}

/// "Check again" once python3 is there and nothing is installed asks for
/// the install; with python3 still missing, or yt-dlp already installed, it
/// asks for nothing.
#[tokio::test]
async fn check_again_requests_the_install_once_python3_is_there() {
    let missing = tempfile::tempdir().unwrap();
    let w = world_with(vec![missing.path().to_path_buf()], tempfile::tempdir().unwrap());
    w.tool.recheck().await;
    assert!(!w.tool.status().updating, "no python3: nothing to install with");
    // The operator installs python3.
    std::os::unix::fs::symlink(real_python3(), missing.path().join("python3")).unwrap();
    w.tool.recheck().await;
    assert!(w.tool.status().updating, "the install is requested");

    let w = world();
    installed_v1(&w).await;
    w.tool.recheck().await;
    assert!(!w.tool.status().updating, "already installed");
}

#[tokio::test]
async fn reprobe_rewrites_the_shebang_when_python3_moves() {
    let first = tempfile::tempdir().unwrap();
    let (guard, second) = python_only_path();
    let w = world_with(vec![first.path().to_path_buf(), second.clone()], guard);
    installed_v1(&w).await;
    let bin = w.data.path().join("bin/yt-dlp");
    let line = |p: &Path| std::fs::read_to_string(p).unwrap().lines().next().unwrap().to_string();
    assert_eq!(line(&bin), format!("#!{} {}", second.join("python3").display(), super::wrapper::PYTHON_FLAGS));
    // A python3 appears earlier on the search path.
    std::os::unix::fs::symlink(real_python3(), first.path().join("python3")).unwrap();
    w.tool.reprobe().await;
    assert_eq!(line(&bin), format!("#!{} {}", first.path().join("python3").display(), super::wrapper::PYTHON_FLAGS));
    assert_eq!(wrapper_version(&w), "2026.01.01");
}

// ---- one tag per update ---------------------------------------------------

#[tokio::test]
async fn everything_comes_from_the_tag_latest_names_and_the_tag_is_recorded() {
    let w = world();
    installed_v1(&w).await;
    assert_eq!(w.tool.settings().installed.unwrap().tag.as_deref(), Some(TAG));
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    w.tool.update(true).await.unwrap();
    let asked = w.fetch.asked();
    assert_eq!(asked[0], Channel::Stable.latest_url());
    assert!(asked[1..].iter().all(|u| u.starts_with(&Channel::Stable.download_base(TAG))), "{asked:?}");
    // A redirect elsewhere is no tag: nothing else is fetched.
    w.fetch.forget();
    w.fetch.serve(&Channel::Stable.latest_url(), Ok(b"https://evil.example/tag/x".to_vec()));
    let r = w.tool.update(true).await;
    assert!(matches!(&r, Err(UpdateError::Fetch(e)) if e.contains("unexpected")), "{r:?}");
    assert_eq!(w.fetch.asked(), vec![Channel::Stable.latest_url()]);
}

// ---- a clock without a battery ------------------------------------------

#[tokio::test]
async fn a_last_check_in_the_future_is_stale() {
    let w = world();
    installed_v1(&w).await;
    assert!(!w.tool.due());
    w.clock.store(T0 - 1, Ordering::SeqCst);
    assert!(w.tool.due(), "the clock went back: the check of 'tomorrow' would block updates for a day more");
}

// ---- start, routes ------------------------------------------------------

#[tokio::test]
async fn a_stale_wrapper_and_conf_are_replaced_at_start() {
    let w = world();
    installed_v1(&w).await;
    let bin = w.data.path().join("bin/yt-dlp");
    let fresh = std::fs::read_to_string(&bin).unwrap();
    std::fs::write(&bin, "#!/bin/sh\necho stale\n").unwrap();
    std::fs::write(w.data.path().join("yt-dlp.conf"), "--stale\n").unwrap();
    w.tool.start().await;
    assert_eq!(std::fs::read_to_string(&bin).unwrap(), fresh);
    assert!(std::fs::read_to_string(w.data.path().join("yt-dlp.conf")).unwrap().contains("--cache-dir"));
    assert_eq!(wrapper_version(&w), "2026.01.01");
}

#[tokio::test]
async fn nothing_is_written_at_start_when_nothing_is_installed() {
    let w = world();
    w.tool.start().await;
    assert!(!w.data.path().join("bin/yt-dlp").exists() && !w.data.path().join("yt-dlp.conf").exists());
}

#[tokio::test]
async fn reprobe_never_waits_for_a_running_job() {
    let (guard, search) = python_only_path();
    let w = world_with(vec![search.clone()], guard);
    installed_v1(&w).await;
    let _job = w.gate.clone().read_owned().await;
    write_script(&search, "node", "echo v22.3.0");
    let caps = tokio::time::timeout(Duration::from_secs(10), w.tool.reprobe()).await.expect("reprobe waited for the job");
    assert_eq!(caps.js_runtime, Some(runtime::JsRuntime::Node));
    let text = std::fs::read_to_string(w.data.path().join("bin/yt-dlp")).unwrap();
    assert!(text.contains("RUNTIME = \"node:"), "{text}");
}

#[tokio::test]
async fn a_requested_update_returns_at_once_and_the_scheduler_runs_it() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    let job = w.gate.clone().read_owned().await;
    tokio::spawn(schedule(w.tool.clone()));
    let started = std::time::Instant::now();
    w.tool.request_update(false);
    assert!(started.elapsed() < Duration::from_millis(100), "the request waited");
    assert!(w.tool.status().updating, "the page sees it at once");
    while !w.tool.status().waiting_for_jobs {
        assert!(started.elapsed() < Duration::from_secs(30), "the scheduler never ran it");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(job);
    while w.tool.status().updating {
        assert!(started.elapsed() < Duration::from_secs(30), "never finished");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(wrapper_version(&w), "2026.02.02");
}

#[tokio::test]
async fn without_python3_nothing_is_fetched_and_the_page_is_told() {
    let empty = tempfile::tempdir().unwrap();
    let w = world_with(vec![empty.path().to_path_buf()], empty);
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.01.01"));
    assert_eq!(w.tool.update(false).await, Err(UpdateError::Python3Missing));
    assert!(w.fetch.asked().is_empty());
    assert_eq!(w.tool.status().error.as_deref(), Some("python3 missing"));
    assert_eq!(w.tool.capabilities().apt_hint.as_deref(), Some("sudo apt install python3"));
    assert!(w.tool.due(), "nothing installed: the next tick tries again");
}

#[tokio::test]
async fn the_swap_waits_for_the_download_job_in_progress() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    let job = w.gate.clone().read_owned().await;
    let tool = w.tool.clone();
    let update = tokio::spawn(async move { tool.update(true).await });
    let started = std::time::Instant::now();
    while !w.tool.status().waiting_for_jobs {
        assert!(started.elapsed() < Duration::from_secs(30), "never got ready");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(wrapper_version(&w), "2026.01.01", "swapped under a running job");
    assert!(w.tool.status().updating);
    drop(job);
    assert!(matches!(update.await.unwrap(), Ok(Update::Installed(_))));
    assert_eq!(wrapper_version(&w), "2026.02.02");
    assert!(!w.tool.status().waiting_for_jobs && !w.tool.status().updating);
}

#[tokio::test]
async fn one_update_at_a_time() {
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    let job = w.gate.clone().read_owned().await;
    let tool = w.tool.clone();
    let first = tokio::spawn(async move { tool.update(true).await });
    let started = std::time::Instant::now();
    while !w.tool.status().waiting_for_jobs {
        assert!(started.elapsed() < Duration::from_secs(30), "never got ready");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let second = tokio::time::timeout(Duration::from_secs(5), w.tool.update(true)).await;
    assert_eq!(second, Ok(Err(UpdateError::Busy)), "a second update must answer at once, not wait");
    drop(job);
    assert!(first.await.unwrap().is_ok());
}

#[tokio::test]
async fn an_interrupted_swap_is_rolled_back_when_the_tool_starts() {
    let w = world();
    installed_v1(&w).await;
    let d = w.data.path();
    std::fs::rename(d.join("yt-dlp"), d.join(install::OLD_DIR)).unwrap();
    let again = Tool::with_deps(
        d,
        Deps {
            fetch: w.fetch.clone(),
            now: Arc::new(|| T0),
            search: vec![],
            arch: "arm".into(),
            gate: ToolGate::default(),
            key: public(test_key()),
            node_keys: vec![public(test_key())],
        },
    );
    assert!(again.status().installed);
    assert_still_v1(&w);
}

#[tokio::test]
async fn the_wrapper_follows_the_runtime_the_machine_offers() {
    let (guard, search) = python_only_path();
    write_script(&search, "node", "echo v22.3.0");
    let w = world_with(vec![search.clone()], guard);
    installed_v1(&w).await;
    let text = std::fs::read_to_string(w.data.path().join("bin/yt-dlp")).unwrap();
    assert!(text.contains(&format!("RUNTIME = \"node:{}\"", search.join("node").display())), "{text}");
    // node removed: the wrapper falls back on nothing.
    std::fs::remove_file(search.join("node")).unwrap();
    let caps = w.tool.reprobe().await;
    assert_eq!(caps.js_runtime, None);
    let text = std::fs::read_to_string(w.data.path().join("bin/yt-dlp")).unwrap();
    assert!(text.contains("RUNTIME = None"), "{text}");
}

#[tokio::test]
async fn deno_is_fetched_only_on_machines_it_publishes_builds_for() {
    // A machine with no runtime of the plugin's: yt-dlp installs, neither
    // runtime is asked for, and none missing is a failure.
    let w = world();
    installed_v1(&w).await;
    publish(&w.fetch, Channel::Stable, &fake_zipapp("2026.02.02"));
    w.tool.update(true).await.unwrap();
    assert!(w.fetch.asked().iter().all(|u| !u.contains("deno") && !u.contains("nodejs")), "{:?}", w.fetch.asked());
    assert_eq!(w.tool.status().runtime_error, None, "no runtime is no failure where none exists");
    // armv7 (the device): node is, deno never.
    let (guard, search) = python_only_path();
    let w = world_on("arm", vec![search], guard);
    installed_v1(&w).await;
    assert!(w.fetch.asked().iter().all(|u| !u.contains("deno")), "{:?}", w.fetch.asked());
}

// ---- the plugin's own node, on armv7 ----------------------------------------

fn node_url(name: &str) -> String {
    format!("{}{name}", runtime::NODE_BASE)
}

/// Serves `latest-v22.x` as `version`, signed by the test key, its armv7l
/// tarball's binary answering `version`.
fn publish_node(fetch: &FakeFetch, version: &str) {
    let dir = format!("node-v{version}-linux-armv7l/");
    let entry = format!("{dir}bin/node");
    let script = format!("#!/bin/sh\necho v{version}\n");
    let tarball = tar_gz(&[(&dir, TarItem::Dir), (&entry, TarItem::File(&script))]);
    let asset = format!("node-v{version}-linux-armv7l.tar.gz");
    let sums = sums_for(&[(&asset, &tarball)]);
    fetch.serve(&node_url(runtime::NODE_SIG), Ok(sign(&sums)));
    fetch.serve(&node_url(runtime::NODE_SUMS), Ok(sums));
    fetch.serve(&node_url(&asset), Ok(tarball));
}

fn node_world() -> World {
    let (guard, search) = python_only_path();
    world_on("arm", vec![search], guard)
}

fn tarball_asked(w: &World) -> bool {
    w.fetch.asked().iter().any(|u| u.ends_with("-linux-armv7l.tar.gz"))
}

#[tokio::test]
async fn on_armv7_node_is_installed_after_yt_dlp_and_is_the_wrappers_runtime() {
    let w = node_world();
    publish_node(&w.fetch, "22.5.1");
    installed_v1(&w).await;
    let caps = w.tool.capabilities();
    assert_eq!((caps.js_runtime, caps.own_node), (Some(runtime::JsRuntime::Node), true));
    let text = std::fs::read_to_string(w.data.path().join("bin/yt-dlp")).unwrap();
    let own = runtime::own_node(w.data.path());
    assert!(text.contains(&format!("RUNTIME = \"node:{}\"", own.display())), "{text}");
    assert_eq!(w.tool.status().runtime_error, None);
}

#[tokio::test]
async fn the_own_node_is_updated_only_when_the_signed_list_names_a_newer_version() {
    let w = node_world();
    publish_node(&w.fetch, "22.5.1");
    installed_v1(&w).await;
    assert_eq!(node_highest(&w).as_deref(), Some("22.5.1"), "recorded at the first install");
    // The same version listed: the list is read, the tarball is not.
    w.tool.update(true).await.unwrap();
    assert!(w.fetch.asked().contains(&node_url(runtime::NODE_SUMS)), "{:?}", w.fetch.asked());
    assert!(!tarball_asked(&w), "{:?}", w.fetch.asked());
    // A newer one: fetched, in place, and recorded.
    w.fetch.forget();
    publish_node(&w.fetch, "22.6.0");
    w.tool.update(true).await.unwrap();
    assert!(tarball_asked(&w), "{:?}", w.fetch.asked());
    assert_eq!(runtime::own_node_version(w.data.path()).await.as_deref(), Some("22.6.0"));
    assert_eq!(node_highest(&w).as_deref(), Some("22.6.0"));
    assert_eq!(w.tool.status().runtime_error, None);
}

/// The highest node ever installed, as `settings.toml` holds it on disk.
fn node_highest(w: &World) -> Option<String> {
    Settings::load(&w.data.path().join("settings.toml")).node_highest
}

/// An old list replayed, validly signed: refused and reported, the node in
/// place kept; and with `bin/node` deleted, the record alone still refuses
/// it, so deleting the binary brings no older node back.
#[tokio::test]
async fn an_older_signed_node_list_is_refused_even_with_the_node_deleted() {
    let w = node_world();
    publish_node(&w.fetch, "22.10.0");
    installed_v1(&w).await;
    w.fetch.forget();
    publish_node(&w.fetch, "22.9.5");
    w.tool.update(true).await.unwrap();
    let st = w.tool.status();
    let f = st.runtime_error_code.expect("a rollback is reported");
    assert_eq!(
        (f.code, f.params["runtime"].as_str(), f.params["installed"].as_str(), f.params["listed"].as_str()),
        ("rollback", "node", "22.10.0", "22.9.5")
    );
    assert!(!tarball_asked(&w), "{:?}", w.fetch.asked());
    assert_eq!(runtime::own_node_version(w.data.path()).await.as_deref(), Some("22.10.0"));
    assert_eq!(node_highest(&w).as_deref(), Some("22.10.0"));

    // bin/node deleted, a day later (past the retry delay), the plugin
    // restarted on the same data: still refused.
    std::fs::remove_file(runtime::own_node(w.data.path())).unwrap();
    let w = World { tool: Tool::with_deps(w.data.path(), deps_of(&w, "arm")), ..w };
    w.clock.fetch_add(DAY, Ordering::SeqCst);
    w.fetch.forget();
    w.tool.update(true).await.unwrap();
    assert_eq!(w.tool.status().runtime_error_code.map(|f| f.code), Some("rollback"));
    assert!(!tarball_asked(&w), "{:?}", w.fetch.asked());
    assert!(!runtime::own_node(w.data.path()).exists());
}

/// A device whose node was installed before the record existed learns it
/// from the node it finds current.
#[tokio::test]
async fn a_current_node_is_recorded_when_nothing_was() {
    let w = node_world();
    publish_node(&w.fetch, "22.5.1");
    installed_v1(&w).await;
    w.tool.edit_settings(|s| s.node_highest = None).unwrap();
    w.tool.update(true).await.unwrap();
    assert_eq!(node_highest(&w).as_deref(), Some("22.5.1"));
}

#[tokio::test]
async fn a_node_failure_keeps_the_node_in_place_and_is_no_yt_dlp_failure() {
    let w = node_world();
    publish_node(&w.fetch, "22.5.1");
    installed_v1(&w).await;
    // nodejs.org unreachable, then a list signed by another key: refused,
    // reported, retried only a day later, and the node in place serves on.
    for broken in [Err("connection refused".to_string()), Ok(b"not a signature".to_vec())] {
        w.fetch.serve(&node_url(runtime::NODE_SIG), broken);
        w.clock.fetch_add(DAY, Ordering::SeqCst);
        w.fetch.forget();
        assert!(matches!(w.tool.update(true).await, Ok(Update::Current(_))));
        let st = w.tool.status();
        assert!(st.error.is_none() && st.runtime_error.is_some(), "{st:?}");
        assert_eq!(st.runtime_error_code.unwrap().params["runtime"], "node");
        assert_eq!(runtime::own_node_version(w.data.path()).await.as_deref(), Some("22.5.1"));
        assert_eq!(w.tool.capabilities().runtime_arg, Some(format!("node:{}", runtime::own_node(w.data.path()).display())));
        // Not again within the day.
        w.fetch.forget();
        w.tool.update(true).await.unwrap();
        assert!(!w.fetch.asked().contains(&node_url(runtime::NODE_SUMS)), "{:?}", w.fetch.asked());
    }
    // A day later, a good list again: the failure is cleared.
    publish_node(&w.fetch, "22.5.1");
    w.clock.fetch_add(DAY, Ordering::SeqCst);
    w.tool.update(true).await.unwrap();
    assert_eq!(w.tool.status().runtime_error, None);
}

#[tokio::test]
async fn the_own_node_is_said_unmaintained_past_its_end_of_life_and_kept() {
    let w = node_world();
    publish_node(&w.fetch, "22.5.1");
    installed_v1(&w).await;
    assert!(!w.tool.capabilities().js_runtime_unmaintained, "before 2027-05-01");
    w.clock.store(runtime::NODE22_END_OF_LIFE, Ordering::SeqCst);
    let caps = w.tool.capabilities();
    assert!(caps.js_runtime_unmaintained);
    assert_eq!(caps.js_runtime, Some(runtime::JsRuntime::Node), "kept, and still the runtime");
    let page = serde_json::to_value(&caps).unwrap();
    assert_eq!(page["js_runtime_unmaintained"], true);
    // A system node is never said unmaintained by the plugin.
    let (guard, search) = python_only_path();
    write_script(&search, "node", "echo v22.3.0");
    let w = world_on("arm", vec![search], guard);
    w.clock.store(runtime::NODE22_END_OF_LIFE + DAY, Ordering::SeqCst);
    let caps = w.tool.reprobe().await;
    assert_eq!((caps.js_runtime, caps.js_runtime_unmaintained), (Some(runtime::JsRuntime::Node), false));
}

#[tokio::test]
async fn a_deno_failure_is_reported_but_yt_dlp_is_installed() {
    let (guard, search) = python_only_path();
    let data = tempfile::tempdir().unwrap();
    let fetch = Arc::new(FakeFetch::default());
    let clock = Arc::new(AtomicU64::new(T0));
    let c = clock.clone();
    let deps = Deps {
        fetch: fetch.clone(),
        now: Arc::new(move || c.load(Ordering::SeqCst)),
        search: vec![search],
        arch: "x86_64".into(),
        gate: ToolGate::default(),
        key: public(test_key()),
        node_keys: vec![public(test_key())],
    };
    let tool = Tool::with_deps(data.path(), deps);
    publish(&fetch, Channel::Stable, &fake_zipapp("2026.01.01"));
    assert!(matches!(tool.update(false).await, Ok(Update::Installed(_))));
    let st = tool.status();
    assert!(st.installed && st.error.is_none() && !st.updating);
    assert!(st.runtime_error.is_some(), "the deno download was tried and its failure kept");
    let deno_asked = |f: &FakeFetch| f.asked().iter().any(|u| u.contains("deno-x86_64"));
    assert!(deno_asked(&fetch), "{:?}", fetch.asked());

    // Not again within the day, even when the page forces yt-dlp's update.
    fetch.forget();
    assert!(matches!(tool.update(true).await, Ok(Update::Current(_))));
    assert!(!deno_asked(&fetch), "{:?}", fetch.asked());
    // A day after the failure, it is tried again.
    fetch.forget();
    clock.store(T0 + DAY, Ordering::SeqCst);
    assert!(matches!(tool.update(true).await, Ok(Update::Current(_))));
    assert!(deno_asked(&fetch), "{:?}", fetch.asked());
    drop(guard);
}

/// Manual, network: the real `latest` of both channels, through the real
/// fetcher, read as a tag.
#[tokio::test]
#[ignore = "manual: asks GitHub"]
async fn manual_latest_tag_of_both_channels() {
    for channel in [Channel::Stable, Channel::Nightly] {
        let location = fetch::Https::default().redirect_of(&channel.latest_url(), LATEST).await.unwrap();
        let tag = channel.tag_from_location(&location).unwrap();
        eprintln!("{channel:?}: {location} -> {tag}");
    }
}

/// Manual, network: deno's real release for this machine, through the real
/// fetcher, into `YTDLP_REAL_DATA`.
#[tokio::test]
#[ignore = "manual: downloads deno from GitHub"]
async fn manual_install_of_the_real_deno() {
    let data = PathBuf::from(std::env::var("YTDLP_REAL_DATA").unwrap());
    let started = std::time::Instant::now();
    runtime::install_deno(&fetch::Https::default(), &data, std::env::consts::ARCH).await.unwrap();
    eprintln!("deno installed in {:?}", started.elapsed());
    let env = runtime::Env { search: vec![], data_dir: data.clone(), arch: std::env::consts::ARCH.into() };
    let caps = runtime::detect(&env).await;
    eprintln!("{caps:?}");
    assert_eq!(caps.js_runtime, Some(runtime::JsRuntime::Deno));
}

/// The one manual end-to-end check on the real release, with the real key:
/// `YTDLP_REAL_DIR` holds `yt-dlp`, `SHA2-256SUMS` and `SHA2-256SUMS.sig` as
/// downloaded, `YTDLP_REAL_DATA` an empty directory to install into. No
/// network: the files are served from that directory.
#[tokio::test]
#[ignore = "manual: needs the real release files, see the doc comment"]
async fn manual_install_of_the_real_release() {
    let dir = PathBuf::from(std::env::var("YTDLP_REAL_DIR").unwrap());
    let data = PathBuf::from(std::env::var("YTDLP_REAL_DATA").unwrap());
    let fetch = Arc::new(FakeFetch::default());
    fetch.serve(&Channel::Stable.latest_url(), Ok(format!("{}tag/{TAG}", Channel::Stable.releases()).into_bytes()));
    for name in [ASSET, SUMS, SIG] {
        fetch.serve(&url(Channel::Stable, name), Ok(std::fs::read(dir.join(name)).unwrap()));
    }
    let search = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect();
    let deps = Deps {
        fetch,
        now: Arc::new(|| T0),
        search,
        // This machine: a deno installed beforehand by the test above is
        // then the wrapper's fallback.
        arch: std::env::consts::ARCH.into(),
        gate: ToolGate::default(),
        key: verify::ytdlp_key().unwrap(),
        node_keys: verify::node_keys().unwrap(),
    };
    let tool = Tool::with_deps(&data, deps);
    let started = std::time::Instant::now();
    let r = tool.update(false).await;
    eprintln!("update: {r:?} in {:?}", started.elapsed());
    assert!(matches!(r, Ok(Update::Installed(_))), "{r:?}");
    let started = std::time::Instant::now();
    let out = retrying(|| Command::new(data.join("bin/yt-dlp")).arg("--version").output());
    eprintln!("wrapper --version: {:?} {:?} in {:?}", out.status, String::from_utf8_lossy(&out.stdout), started.elapsed());
    assert!(out.status.success());
}
