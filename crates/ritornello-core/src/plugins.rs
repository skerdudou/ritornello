use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A plugin without a mention is **enabled**: no `plugins.toml` in service
/// changes meaning when this key appears, and "no key = on" stays true on both
/// sides — `set_enabled` removes the key instead of writing `true`.
fn enabled_by_default() -> bool {
    true
}

/// An entry of `plugins.toml`: what to launch, under which name. Nothing else.
///
/// Neither the kind nor the admin page is declared there: they are properties
/// of the **binary**, which announces them itself on the core's registration
/// socket. The operator no longer has to know them, and forgetting them can no
/// longer produce a silent degraded mode.
///
/// **The file order remains meaningful**: it is what arbitrates between two
/// plugins announcing the `metadata` kind (see `crate::register`).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub exec: String,
    /// Plugin launched at startup and wired, or left off. Toggled from the
    /// admin UI (`PUT /api/plugins/:name/enabled`), persisted here.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginConfig>,
}

impl PluginManifest {
    /// A **missing** file gives an empty manifest: the core starts without
    /// plugins rather than failing (consistent with the treatment already in
    /// place for `stations.toml`). Any other I/O error is propagated, like an
    /// invalid TOML: a `plugins.toml` that is present but unreadable
    /// (permissions) and would give "no source available" would send the
    /// diagnosis in the wrong direction.
    ///
    /// A duplicated `name` is neither rejected nor deduplicated here, only
    /// reported (see `duplicate_names`): it was the workaround used before this
    /// work to make a single binary serve two kinds, and a device in service
    /// may still carry it.
    pub fn load(path: &Path) -> Result<Self> {
        let manifest: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        for name in duplicate_names(&manifest.plugins) {
            tracing::warn!(
                "plugin name '{name}' appears more than once in plugins.toml: a single \
                 announcement satisfies both entries, and the second connection is wired \
                 twice, left hanging in a backlog nobody accepts"
            );
        }
        Ok(manifest)
    }
}

/// Toggles the `enabled` key of plugin `name` in the file, in place.
///
/// Disabling sets `enabled = false`; re-enabling **removes the key** rather
/// than writing `true`, so that an all-on file carries none and "no mention =
/// on" stays true on both sides.
///
/// An undeclared name is an error and **rewrites nothing**: this is what lets
/// the HTTP layer refuse before acting.
pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut =
        text.parse().with_context(|| format!("parsing {}", path.display()))?;
    let blocks = doc
        .get_mut("plugin")
        .and_then(|item| item.as_array_of_tables_mut())
        .ok_or_else(|| anyhow::anyhow!("no [[plugin]] entry in {}", path.display()))?;
    let block = blocks
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .ok_or_else(|| anyhow::anyhow!("plugin '{name}' is not declared in {}", path.display()))?;
    if enabled {
        block.remove("enabled");
    } else {
        block["enabled"] = toml_edit::value(false);
    }
    write_atomic(path, &doc.to_string())
}

/// Writes through a neighboring temporary file then `rename` — atomic on a
/// single filesystem, and the idiom already used for the configuration files
/// written by the `files` plugin.
///
/// A `plugins.toml` truncated by a power cut — a device one unplugs — would
/// let nothing launch at the next startup.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // The error worth propagating is the `rename` one (immutable target,
        // filesystem hostile to renaming): a cleanup that failed in turn must
        // not mask it, so its result is ignored. Best-effort: do not leave the
        // temporary file lying around rather than failing on something other
        // than what we report.
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming onto {}", path.display()));
    }
    Ok(())
}

/// `plugin.name` names appearing more than once in `plugins`, each only once,
/// in the order of their first duplication.
///
/// Pure function so as to be testable: `PluginManifest::load` uses it to name
/// each duplicate in a `tracing::warn!`, without rejecting or deduplicating
/// anything — a duplicated declaration remains silently wired twice today, the
/// second connection hanging in a backlog nobody accepts.
fn duplicate_names(plugins: &[PluginConfig]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut duplicates = Vec::new();
    for p in plugins {
        if !seen.insert(p.name.as_str()) && !duplicates.iter().any(|d| d == &p.name) {
            duplicates.push(p.name.clone());
        }
    }
    duplicates
}

/// Wipes and recreates `{runtime_dir}/sockets`, and returns its path.
///
/// A fresh directory at every startup makes stale files **impossible** instead
/// of relying on case-by-case pre-deletion: a socket left by a previous run is
/// connectable, and the core would talk to a zombie or wait for a retried
/// `ECONNREFUSED`. A single instance of the core per `runtime_dir` — guaranteed
/// by systemd's `RuntimeDirectory=` in service, by a distinct variable in
/// development.
pub fn prepare_sockets_dir(runtime_dir: &Path) -> Result<PathBuf> {
    let dir = runtime_dir.join("sockets");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("clearing {}", dir.display()));
        }
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Launches a plugin, telling it where to announce itself, under which name,
/// and with which socket prefix.
///
/// No file pre-deletion here: `prepare_sockets_dir` wiped the whole directory
/// before the first launch.
///
/// `locale` passes the current language via `RITORNELLO_LOCALE`, applied **at
/// launch** only (unchanged).
pub fn spawn(
    exec: &str,
    register: &Path,
    name: &str,
    prefix: &Path,
    locale: Option<&str>,
) -> Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(exec);
    cmd.arg("--register").arg(register);
    cmd.arg("--name").arg(name);
    cmd.arg("--socket-prefix").arg(prefix);
    if let Some(locale) = locale {
        cmd.env("RITORNELLO_LOCALE", locale);
    }
    // The path is named in the error: "No such file or directory" alone leaves
    // one guessing **which** of the `plugins.toml` paths is at fault, and the
    // most common confusion is precisely there — a deployment `exec`
    // (`/usr/local/lib/...`) copied into a development configuration, where the
    // binaries live under `target/debug/`.
    cmd.kill_on_drop(true).spawn().with_context(|| format!("executable {exec}"))
}

/// Time given to a plugin between `SIGTERM` and `SIGKILL`.
///
/// Two seconds: no plugin has any cleanup to do today, and the toggle comes
/// from a web page waiting for the answer.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Terminates a plugin: `SIGTERM`, then `SIGKILL` if it lingers beyond
/// `grace`.
///
/// `SIGTERM` first, as for mpv (`system.rs`): it is the signal a plugin may
/// one day intercept to hand back a console or turn off a screen. None does,
/// and Rust's default terminates it at once — but killing outright would
/// forbid that courtesy forever.
///
/// Returns the exit status, never an endless wait: that is the whole point of
/// the fallback to `SIGKILL`, which no process can mask.
pub async fn terminate(
    child: &mut tokio::process::Child,
    grace: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = child.id() {
        // SAFETY: the `Child` is still alive here, so the process has not been
        // reaped and its pid could not have been reassigned to another.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            tracing::warn!("plugin ignored SIGTERM, sending SIGKILL");
            child.kill().await?;
            child.wait().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_toml_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"

[[plugin]]
name = "console"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-console"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 2);
        assert_eq!(m.plugins[0].name, "radio");
        assert_eq!(m.plugins[1].name, "console");
    }

    #[test]
    fn a_manifest_without_kind_loads() {
        // The kind is now announced by the binary: the file no longer carries
        // it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 1);
        assert_eq!(m.plugins[0].name, "radio");
    }

    #[test]
    fn the_sockets_directory_is_fresh_at_every_startup() {
        // A stale file from a previous run is connectable and would make the
        // core talk to a zombie: the directory is therefore wiped, not cleaned
        // case by case.
        let dir = tempfile::tempdir().unwrap();
        let sockets = dir.path().join("sockets");
        std::fs::create_dir_all(&sockets).unwrap();
        let stale = sockets.join("radio-source.sock");
        std::fs::write(&stale, "").unwrap();

        let result = prepare_sockets_dir(dir.path()).unwrap();
        assert_eq!(result, sockets);
        assert!(result.is_dir(), "the directory must exist after the call");
        assert!(!stale.exists(), "the stale file must have disappeared");
    }

    #[test]
    fn a_missing_manifest_is_empty_but_an_unreadable_one_is_an_error() {
        // Missing = installation without plugins, normal case. Unreadable
        // (here: the parent "directory" is actually a file) = a problem to
        // name — "no source available" would send the diagnosis in the wrong
        // direction.
        let dir = tempfile::tempdir().unwrap();
        let absent = PluginManifest::load(&dir.path().join("plugins.toml")).unwrap();
        assert!(absent.plugins.is_empty());
        let stub = dir.path().join("not-a-directory");
        std::fs::write(&stub, "").unwrap();
        assert!(PluginManifest::load(&stub.join("plugins.toml")).is_err());
    }

    #[test]
    fn a_launch_error_always_names_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let e = spawn(
            "/path/that/does/not/exist/ritornello-plugin-dummy",
            &dir.path().join("register.sock"),
            "dummy",
            &dir.path().join("dummy"),
            None,
        )
        .expect_err("a missing executable must fail");
        let message = format!("{e:#}");
        assert!(
            message.contains("/path/that/does/not/exist/ritornello-plugin-dummy"),
            "the error must name the executable looked for: {message}"
        );
    }

    #[test]
    fn missing_manifest_gives_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let m = PluginManifest::load(&dir.path().join("absent.toml")).unwrap();
        assert!(m.plugins.is_empty());
    }

    #[test]
    fn detects_a_duplicated_name_without_rejecting_or_deduplicating_it() {
        // The duplicated name was the workaround before this work to make a
        // single binary serve two kinds: a manifest carrying it must load as is
        // (both entries), not fail.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "mpd"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mpd"

[[plugin]]
name = "mpd"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mpd"

[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 3, "the duplicate is not deduplicated at load time");
        assert_eq!(duplicate_names(&m.plugins), vec!["mpd".to_string()]);
    }

    #[test]
    fn no_duplicated_name_reports_nothing() {
        let plugins = vec![
            PluginConfig { name: "radio".into(), exec: "radio".into(), enabled: true },
            PluginConfig { name: "files".into(), exec: "files".into(), enabled: true },
        ];
        assert!(duplicate_names(&plugins).is_empty());
    }

    #[test]
    fn enabled_absent_means_enabled_and_false_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            "[[plugin]]\nname = \"radio\"\nexec = \"/bin/true\"\n\n\
             [[plugin]]\nname = \"cd\"\nexec = \"/bin/true\"\nenabled = false\n",
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        // A `plugins.toml` in service does not carry the key: it must keep
        // launching everything.
        assert!(m.plugins[0].enabled, "without a mention, a plugin is enabled");
        assert!(!m.plugins[1].enabled);
    }

    /// A commented manifest like the deployment one: this is what the rewrite
    /// must leave intact.
    fn commented_manifest() -> &'static str {
        "# The web tuner.\n\
         [[plugin]]\n\
         name = \"radio\"\n\
         exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"\n\
         \n\
         # Metadata: the order of this file arbitrates.\n\
         [[plugin]]\n\
         name = \"musicbrainz\"\n\
         exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-musicbrainz\"\n"
    }

    #[test]
    fn disabling_sets_the_key_without_touching_the_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, commented_manifest()).unwrap();

        set_enabled(&path, "radio", false).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# The web tuner."), "header comment lost");
        assert!(
            after.contains("# Metadata: the order of this file arbitrates."),
            "second block's comment lost"
        );
        let m = PluginManifest::load(&path).unwrap();
        assert!(!m.plugins[0].enabled);
        assert!(m.plugins[1].enabled, "the neighbor did not move");
        // The file order arbitrates the `metadata` plugins: rewriting it must
        // not permute it.
        assert_eq!(m.plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["radio", "musicbrainz"]);
    }

    #[test]
    fn re_enabling_removes_the_key_instead_of_writing_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, commented_manifest()).unwrap();

        set_enabled(&path, "radio", false).unwrap();
        set_enabled(&path, "radio", true).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        // "No mention = on" must stay true on both sides: an all-on file
        // carries no key.
        assert!(!after.contains("enabled"), "the key should have disappeared: {after}");
        assert!(PluginManifest::load(&path).unwrap().plugins[0].enabled);
    }

    #[test]
    fn an_undeclared_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, commented_manifest()).unwrap();

        let before = std::fs::read_to_string(&path).unwrap();
        assert!(set_enabled(&path, "inconnu", false).is_err());
        // Refusal **without side effect**: the file is not rewritten.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn no_temporary_file_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, commented_manifest()).unwrap();

        set_enabled(&path, "radio", false).unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["plugins.toml"], "a temporary file remained");
    }

    #[test]
    fn turned_off_then_back_on_the_plugin_regains_its_place_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, commented_manifest()).unwrap();

        set_enabled(&path, "musicbrainz", false).unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert!(m.plugins[0].enabled, "the neighbor stays on");
        assert!(!m.plugins[1].enabled);

        set_enabled(&path, "musicbrainz", true).unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert!(m.plugins.iter().all(|p| p.enabled), "everything is back on");
        // The file order arbitrates the `metadata` plugins: a plugin turned
        // back on must regain its original priority, not the tail of the list.
        assert_eq!(
            m.plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["radio", "musicbrainz"]
        );
        // And the file is back to its original shape, comments included.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), commented_manifest());
    }

    #[tokio::test]
    async fn terminate_stops_a_sleeping_process() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let status = terminate(&mut child, SHUTDOWN_GRACE).await.unwrap();
        // Terminated by signal: no zero exit code.
        assert!(!status.success(), "the process should have been terminated: {status:?}");
    }

    #[tokio::test]
    async fn terminate_insists_when_sigterm_is_ignored() {
        // A plugin that masks SIGTERM must not be able to hold up the shutdown.
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        // Short grace: the test measures the **fallback** to SIGKILL, not a timeout.
        let status = terminate(&mut child, std::time::Duration::from_millis(200)).await.unwrap();
        assert!(!status.success(), "SIGKILL should have gotten the better of it: {status:?}");
    }
}
