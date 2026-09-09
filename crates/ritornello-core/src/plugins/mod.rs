// No caller yet: `append_block`, `remove_entry` and `move_entry` get their
// first caller in a later task. Until then only this module's own tests
// reach its public items, and a binary crate (unlike a library) does not
// treat `pub` as "reachable from outside" on its own.
#[allow(dead_code)]
pub mod edit;

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
///
/// `pub(crate)`: `status::plugin_status::plugin_delete` reuses it to persist
/// `edit::remove_entry`'s output, the same way this module's own
/// `set_enabled` already does.
///
/// **Known debt, recorded rather than dressed up: the four read-modify-write
/// sequences over `plugins.toml` are not serialised against each other.**
/// They are `set_enabled` just above, `plugin_move_post` and `plugin_delete`
/// in `status::plugin_status`, and `update::Worker::write_declaration`. Each
/// reads the file, transforms the text and renames a temporary onto it, with
/// no lock across the three steps. The rename makes a *torn* file impossible;
/// it does nothing about a **lost update** — the worker appends a plugin block
/// (reads V0, writes V0+block) while a move handler reads V0 and writes
/// V0+move, and whichever renames last wins. "No `.await` inside the window"
/// does not serialise them: they run on different tasks of a multi-threaded
/// runtime.
///
/// Reach is narrow — the worker only writes this file on a **fresh** install,
/// so it takes an operator moving, switching or deleting a *different* plugin
/// from a second tab during one — and the loss is visible and undoable (a lost
/// declaration shows as "Installed but not declared" and re-declares; a lost
/// move is re-done with one arrow). Contrast `update::placed::record`, whose
/// identical read-modify-write **is** safe, and for the reason this one is
/// not: it has a single serial writer.
///
/// The shape of the fix, for whoever takes it: one process-wide mutex and one
/// `edit_manifest(path, transform)` helper here that holds it across read,
/// transform and rename, returning an enum the callers match on — the three
/// handlers build their messages with `catalog.read().await`, so those awaits
/// have to move out of the locked region, which is why this is a refactor of
/// three handlers rather than four added lines.
pub(crate) fn write_atomic(path: &Path, content: &str) -> Result<()> {
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

/// Entries of `plugins_dir` that `manifest` does not declare — a binary on
/// disk with nothing declaring it. The twin of `missing_binary` (a
/// declaration with no binary behind it): this is what a hand-dropped binary
/// leaves, or what an uninstall leaves between its two halves — the
/// declaration is already gone from `plugins.toml`, and the privileged unit
/// has not yet erased the file (see `Availability::Undeclared`'s doc).
///
/// **One scan, shared by both readers that need it** — `update::Worker::installed`
/// (which turns a name here into an `Installed` row so `/api/update` can
/// report `Availability::Undeclared`) and `status::status_json` (which turns
/// one into a `PluginStatus` line so `/api/status` can set
/// `PluginStatus::undeclared_binary`) — precisely so the two payloads answer
/// the same question about the same directory rather than risking two
/// answers.
///
/// Compares **paths**, not bare file names, which carries a precondition
/// this function does not itself check: a declared `exec` must be an
/// absolute path, matching `plugins_dir` joined onto the entry's own file
/// name — `plugins.toml`'s existing convention, never enforced here or at
/// load time. **A relative `exec` is not immune under this scheme; it is the
/// opposite.** `PathBuf::from("radio")` can never equal
/// `plugins_dir.join("radio")` (`std::fs::read_dir`'s entries are always
/// absolute), so a plugin declared with a relative `exec` would have its own
/// present binary misreported as `Undeclared`. Harmless today because
/// nothing ships or hand-edits a relative `exec` — comparing bare file names
/// instead would have been robust to that case, at the cost of conflating
/// two different plugins that happened to share a binary's file name.
///
/// A `plugins_dir` that cannot be read (not created yet — a device with no
/// plugin installed at all, though `plugins.toml` itself always ships one) is
/// silently empty rather than an error: nothing here is a fault, only a
/// question with no directory to answer it from.
pub fn undeclared_binaries(plugins_dir: &Path, manifest: &PluginManifest) -> Vec<String> {
    // Canonicalised, not compared as written: a declared `exec` reached
    // through a relative path, a doubled separator or a symlink is the exact
    // same file as the one the scan lists, and comparing the raw `PathBuf`s
    // (task 18's own review) would report it undeclared regardless — a
    // display bug when this function only feeds a badge, and a way to let a
    // *declared* binary through the belt-and-braces check `plugin_binary_delete`
    // adds on top of this, once that route exists. A declared `exec` that
    // cannot be canonicalised (the plugin's `missing_binary` case: nothing is
    // there to resolve) falls back to the path as written, which is exactly
    // this function's previous, narrower behaviour for that one entry.
    let declared: std::collections::HashSet<PathBuf> = manifest
        .plugins
        .iter()
        .map(|p| {
            let raw = PathBuf::from(&p.exec);
            std::fs::canonicalize(&raw).unwrap_or(raw)
        })
        .collect();
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            let path = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
            !declared.contains(&path)
        })
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

/// Recovers the component name a release would publish this binary under,
/// from the bare file name a directory scan finds it as (`undeclared_binaries`
/// returns file names, never anything else). The release's own packaging
/// convention (see `update::archive` and `update::release::classify_asset`)
/// names a plugin's binary `ritornello-plugin-<name>`; a file that does not
/// follow it — a hand-dropped binary, never one of ours — has no name to
/// recover, and is returned unchanged.
///
/// **The one place this mapping is made.** Both `/api/status` (`PluginStatus`,
/// in `status::status_json`) and `/api/update` (`Installed`, in
/// `update::Worker::installed`) call this rather than each repeating the
/// prefix — a second, independent copy of it is exactly how a review of this
/// task found the two sides had stopped agreeing: `update::mod::resolve`
/// matches a **component** name against the release (`mpd`), so a row left
/// named by its bare file (`ritornello-plugin-mpd`) can never resolve to
/// anything the release publishes, and "Declare" failed with no message at
/// all.
pub fn component_name_from_file(file: &str) -> &str {
    file.strip_prefix("ritornello-plugin-").unwrap_or(file)
}

/// May `file` be erased as an undeclared binary? **Two independent checks**,
/// because unlike every sibling route this one deletes a file and cannot be
/// undone by writing the manifest back:
///
/// 1. `file` must be in `currently_undeclared` — a **fresh** scan
///    (`undeclared_binaries`), taken at request time, never anything the page
///    sent or a previous check cached. The same "the file is the authority"
///    doctrine `plugin_enabled_put`, `plugin_move_post` and `plugin_delete`
///    already follow.
/// 2. `file` must not equal any declared plugin's own `exec` file name —
///    belt and braces against the scan's own path comparison ever missing a
///    declared binary (`undeclared_binaries` now canonicalises, but this
///    check does not depend on that staying true forever, or on the scan
///    being the only caller ever routed here).
///
/// A pure function over the manifest and a scan result, rather than the scan
/// run twice: what the second check must catch is precisely a scan that
/// disagrees with the manifest, so it cannot trust the same scan to have
/// already ruled itself out.
pub fn binary_is_removable(file: &str, manifest: &PluginManifest, currently_undeclared: &[String]) -> bool {
    let in_current_scan = currently_undeclared.iter().any(|f| f == file);
    let matches_a_declared_exec = manifest
        .plugins
        .iter()
        .any(|p| Path::new(&p.exec).file_name().and_then(|f| f.to_str()) == Some(file));
    in_current_scan && !matches_a_declared_exec
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

    /// The first of the three cases RULING 63 asks for: a binary the
    /// manifest does not mention is reported.
    #[test]
    fn a_binary_present_and_undeclared_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("radio"), b"").unwrap();
        std::fs::write(dir.path().join("extra"), b"").unwrap();
        let manifest = PluginManifest {
            plugins: vec![PluginConfig {
                name: "radio".into(),
                exec: dir.path().join("radio").to_string_lossy().into_owned(),
                enabled: true,
            }],
        };
        assert_eq!(undeclared_binaries(dir.path(), &manifest), vec!["extra".to_string()]);
    }

    /// The second case: a binary the manifest **does** declare, by its exact
    /// `exec` path, must not come back as one of the extras — this is the
    /// same fixture as above, and it is `radio`'s absence from the result
    /// (asserted together with `extra`'s presence) that proves it.
    #[test]
    fn a_binary_present_and_declared_is_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("radio"), b"").unwrap();
        let manifest = PluginManifest {
            plugins: vec![PluginConfig {
                name: "radio".into(),
                exec: dir.path().join("radio").to_string_lossy().into_owned(),
                enabled: true,
            }],
        };
        assert!(undeclared_binaries(dir.path(), &manifest).is_empty());
    }

    /// The third case, and the one a careless implementation gets wrong: a
    /// declaration whose binary is **absent** must not be reported here
    /// either — that is `missing_binary`'s question, not this one, and the
    /// two must never both fire for the same plugin. `read_dir` never
    /// produces an entry for a file that does not exist, so a scan of a
    /// directory that genuinely lacks it answers empty on its own — this
    /// pins that rather than assuming it.
    #[test]
    fn a_declared_plugin_with_no_binary_is_reported_by_neither() {
        let dir = tempfile::tempdir().unwrap();
        // The directory exists, but `mpd`'s binary was never placed in it.
        let manifest = PluginManifest {
            plugins: vec![PluginConfig {
                name: "mpd".into(),
                exec: dir.path().join("mpd").to_string_lossy().into_owned(),
                enabled: true,
            }],
        };
        assert!(undeclared_binaries(dir.path(), &manifest).is_empty());
    }

    /// A subdirectory is not a binary: without this filter, a plugin that
    /// keeps a data directory beside its `exec` (or the `staging` directory
    /// itself, if it ever lived under the plugins directory) would be
    /// reported as an orphaned binary.
    #[test]
    fn a_subdirectory_is_never_reported_as_an_undeclared_binary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("some-data-dir")).unwrap();
        assert!(undeclared_binaries(dir.path(), &PluginManifest::default()).is_empty());
    }

    #[test]
    fn a_plugins_directory_that_does_not_exist_yet_is_reported_as_empty_not_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-there-yet");
        assert!(undeclared_binaries(&missing, &PluginManifest::default()).is_empty());
    }

    /// Task 18's re-review, Finding 2: a doubled separator (this test's
    /// original fixture) proves nothing about canonicalisation — Rust's own
    /// `Path`/`PathBuf` equality already collapses redundant separators at
    /// the component level (`PathBuf::from("/a//b") == PathBuf::from("/a/b")`
    /// is `true` with or without `canonicalize`), so that fixture passed
    /// identically whether the fix was present or not. A `..` hop is
    /// different: `Path`'s own component-wise equality does **not** resolve
    /// it lexically (`["a","b","..","c"]` and `["a","c"]` are different
    /// component sequences to `Path`), so only `canonicalize` — which
    /// actually consults the filesystem — can prove the two name the same
    /// file. Without the fix this asserts, `undeclared_binaries` wrongly
    /// returns `["radio"]` for a genuinely declared binary; confirmed by
    /// commenting out the `canonicalize` calls and re-running: it reddens.
    #[test]
    fn a_relative_hop_through_parent_in_the_declared_exec_does_not_make_the_binary_look_undeclared() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("radio"), b"").unwrap();
        // The same file as `sub.join("radio")`, reached through a `..` hop
        // back into the same directory — a shape only `canonicalize` (by
        // actually resolving it against the filesystem) collapses to the
        // scan's own path.
        let declared_exec = format!("{}/../sub/radio", sub.to_string_lossy());
        let manifest = PluginManifest {
            plugins: vec![PluginConfig { name: "radio".into(), exec: declared_exec, enabled: true }],
        };
        assert!(undeclared_binaries(&sub, &manifest).is_empty());
    }

    /// The ordinary case: a name the fresh scan reports, and no declared
    /// plugin claims. Removable.
    #[test]
    fn a_name_the_scan_reports_and_nothing_declares_is_removable() {
        let manifest = PluginManifest {
            plugins: vec![PluginConfig { name: "radio".into(), exec: "/a/radio".into(), enabled: true }],
        };
        assert!(binary_is_removable("ritornello-plugin-mpd", &manifest, &["ritornello-plugin-mpd".into()]));
    }

    /// First operand: absent from the fresh scan. Even with an empty
    /// manifest (nothing could possibly "declare" it), a name the scan does
    /// not currently report is not removable — the scan is what makes this
    /// route's decision, not the page's memory of an earlier one.
    #[test]
    fn a_name_absent_from_the_fresh_scan_is_not_removable() {
        assert!(!binary_is_removable("ritornello-plugin-mpd", &PluginManifest::default(), &[]));
    }

    /// Second operand, tested independently of the first: a name the
    /// (possibly stale or wrong) scan claims is undeclared, but that is
    /// nonetheless a declared plugin's own `exec` file name. Passing it
    /// through `currently_undeclared` directly — rather than trusting
    /// `undeclared_binaries` to already agree — is what proves this check is
    /// truly a *second*, independent reason to refuse, not a restatement of
    /// the first.
    #[test]
    fn a_name_matching_a_declared_execs_file_name_is_never_removable_even_if_the_scan_says_so() {
        let manifest = PluginManifest {
            plugins: vec![PluginConfig {
                name: "radio".into(),
                exec: "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio".into(),
                enabled: true,
            }],
        };
        assert!(!binary_is_removable(
            "ritornello-plugin-radio",
            &manifest,
            &["ritornello-plugin-radio".into()],
        ));
    }

    /// The release's naming convention, in one direction: what a scan finds
    /// on disk (`ritornello-plugin-mpd`) becomes the component name the
    /// release publishes and the operator would recognise (`mpd`).
    #[test]
    fn component_name_from_file_strips_the_release_prefix() {
        assert_eq!(component_name_from_file("ritornello-plugin-mpd"), "mpd");
    }

    /// A hand-dropped binary that never followed the convention has no name
    /// but its own: the function must not invent one, or truncate a name that
    /// only coincidentally starts with something else.
    #[test]
    fn component_name_from_file_leaves_a_foreign_name_unchanged() {
        assert_eq!(component_name_from_file("my-homebrew-daemon"), "my-homebrew-daemon");
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
