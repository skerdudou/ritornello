//! Where a download may go: a folder, chosen by the operator, on a share the
//! `files` plugin declares and that is really writable.
//!
//! **The roots are the files plugin's.** They are read from its
//! `media-roots.toml` through `ritornello-files-mount`'s library (never its
//! helper binary): one grammar, one reading of `/proc/mounts`. This plugin
//! runs unprivileged and may write only under `/etc/ritornello` and
//! `/mnt/ritornello`, so only an SMB root that is declared `writable` *and*
//! mounted `rw` right now is a target. A local root is never one: its path may
//! lie anywhere, outside what this service can write.
//!
//! **Everything is confined under `Root::base_dir()`**: the folder `files`
//! actually shows (the mount point, then the declared subpath), not the whole
//! share. The "mounted read-write" check, on the other hand, reads the line of
//! the *mount point* in `/proc/mounts`: that is what is mounted.
//!
//! **A path is a relative folder path**, components separated by `/`; the
//! empty path is the base itself. A component is never empty, `.`, `..`, nor
//! holds `/`, `\` or NUL. The check is textual and then real: the folder is
//! canonicalised (a symlink that points outside is followed, and refused) and
//! must lie under the canonicalised base.
//!
//! **A window remains between resolving and writing.** A target is resolved
//! when a job starts and written to minutes later: a folder swapped for a
//! symlink in between is followed. Accepted, because whoever can do that on
//! the share is the operator's own NAS writer.
//!
//! **`$` is refused in every name, and in the base.** yt-dlp runs
//! `expandvars` on the whole `-o` value (measured: `$HOME` and `${PATH}` are
//! expanded, `$$` is no escape), so a folder with a `$` would be written
//! somewhere other than the folder that was checked here. Such a folder is
//! not offered by `list_dirs` either.
//!
//! **No filesystem call on a share runs without a deadline.** A share that
//! stops answering blocks the thread that asks; each call runs in
//! `spawn_blocking` under `tokio::time::timeout`, so it blocks a pool thread
//! and never the plugin (a mute share has frozen the admin loop here once).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ritornello_files_mount::mounts::{is_mounted_in, mount_points};
use ritornello_files_mount::roots::{Root, RootKind, Roots};
use serde::{Deserialize, Serialize};

/// The `files` plugin's declared roots: a sibling of our own data directory.
pub fn files_roots_path(own_data_dir: &Path) -> PathBuf {
    own_data_dir.parent().unwrap_or(own_data_dir).join("files").join("media-roots.toml")
}

/// Whether `point` is mounted read-write, by its line in `/proc/mounts`.
///
/// The fourth column is split on commas and one option must be exactly `rw`:
/// `user=rw` or `rwx` are not it. A point listed twice is judged by its last
/// line, the one on top (a remount appends).
pub fn mounted_rw(proc_mounts: &str, point: &Path) -> bool {
    proc_mounts
        .lines()
        .rfind(|line| mount_points(line).next().as_deref() == Some(point))
        .and_then(|line| line.split_whitespace().nth(3))
        .is_some_and(|options| options.split(',').any(|o| o == "rw"))
}

/// A folder chosen on a root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub root: String,
    /// Relative to the root's `base_dir()`; empty is the base itself.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    UnknownRoot,
    /// Not declared writable, mounted read-only, or not an SMB root.
    NotWritable,
    NotMounted,
    /// A path component that is not a plain name, or a folder that resolves
    /// outside the base.
    Escapes,
    /// The share did not answer in time.
    Unresponsive,
    Io(String),
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetError::UnknownRoot => write!(f, "unknown root"),
            TargetError::NotWritable => write!(f, "the root is not writable"),
            TargetError::NotMounted => write!(f, "the root is not mounted"),
            TargetError::Escapes => write!(f, "the folder is not inside the root"),
            TargetError::Unresponsive => write!(f, "the share did not answer in time"),
            TargetError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TargetError {}

/// Whether a root can receive a download right now.
fn is_target(root: &Root, proc_mounts: &str) -> bool {
    root.kind == RootKind::Smb && root.writable && mounted_rw(proc_mounts, &root.mount_point())
}

/// The names of the roots a download may go to.
pub fn writable_targets(roots: &Roots, proc_mounts: &str) -> Vec<String> {
    roots.root.iter().filter(|r| is_target(r, proc_mounts)).map(|r| r.name.clone()).collect()
}

/// A folder name that is one plain component.
pub(crate) fn component_ok(c: &str) -> bool {
    !c.is_empty() && c != "." && c != ".." && !c.contains(['/', '\\', '\0', '$'])
}

fn components(path: &str) -> Result<Vec<&str>, TargetError> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().all(|c| component_ok(c)) {
        Ok(parts)
    } else {
        Err(TargetError::Escapes)
    }
}

/// Whether a path holds a `$` anywhere (yt-dlp would expand it).
pub(crate) fn has_dollar(p: &Path) -> bool {
    p.to_string_lossy().contains('$')
}

/// A target resolved: the canonical base it is confined to, and the canonical
/// folder under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub base: PathBuf,
    pub dir: PathBuf,
}

/// Runs a blocking filesystem call under a deadline.
pub async fn blocking<T, F>(deadline: Duration, f: F) -> Result<T, TargetError>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    match tokio::time::timeout(deadline, tokio::task::spawn_blocking(f)).await {
        Err(_) => Err(TargetError::Unresponsive),
        Ok(Err(join)) => Err(TargetError::Io(join.to_string())),
        Ok(Ok(Err(e))) => Err(TargetError::Io(e.to_string())),
        Ok(Ok(Ok(v))) => Ok(v),
    }
}

/// `base` joined with `path`, canonicalised, and checked to lie under the
/// canonical `base`.
pub(crate) async fn confine<C>(base: PathBuf, path: &str, deadline: Duration, canon: C) -> Result<Resolved, TargetError>
where
    C: Fn(&Path) -> io::Result<PathBuf> + Send + 'static,
{
    let joined = components(path)?.into_iter().fold(base.clone(), |p, c| p.join(c));
    let (base, dir) = blocking(deadline, move || Ok((canon(&base)?, canon(&joined)?))).await?;
    // Judged on the real path: a symlink inside the base may lead to a folder
    // whose real name holds a `$` (`dir` is under `base`, so this covers both).
    if !dir.starts_with(&base) || has_dollar(&dir) {
        return Err(TargetError::Escapes);
    }
    Ok(Resolved { base, dir })
}

/// The root a target names, if it can receive a download.
fn checked_root<'a>(target: &Target, roots: &'a Roots, proc_mounts: &str) -> Result<&'a Root, TargetError> {
    let root = roots.by_name(&target.root).ok_or(TargetError::UnknownRoot)?;
    if root.kind != RootKind::Smb || !root.writable {
        return Err(TargetError::NotWritable);
    }
    if has_dollar(&root.base_dir()) {
        return Err(TargetError::Escapes);
    }
    let point = root.mount_point();
    if !is_mounted_in(proc_mounts, &point) {
        return Err(TargetError::NotMounted);
    }
    if !mounted_rw(proc_mounts, &point) {
        return Err(TargetError::NotWritable);
    }
    Ok(root)
}

/// The target's folder and the base it is confined to, with `canon` standing
/// for `std::fs::canonicalize` (a test hands one that sleeps).
pub async fn resolve_parts_with<C>(
    target: &Target,
    roots: &Roots,
    proc_mounts: &str,
    deadline: Duration,
    canon: C,
) -> Result<Resolved, TargetError>
where
    C: Fn(&Path) -> io::Result<PathBuf> + Send + 'static,
{
    let root = checked_root(target, roots, proc_mounts)?;
    confine(root.base_dir(), &target.path, deadline, canon).await
}

pub async fn resolve_with<C>(
    target: &Target,
    roots: &Roots,
    proc_mounts: &str,
    deadline: Duration,
    canon: C,
) -> Result<PathBuf, TargetError>
where
    C: Fn(&Path) -> io::Result<PathBuf> + Send + 'static,
{
    Ok(resolve_parts_with(target, roots, proc_mounts, deadline, canon).await?.dir)
}

pub async fn resolve(
    target: &Target,
    roots: &Roots,
    proc_mounts: &str,
    deadline: Duration,
) -> Result<PathBuf, TargetError> {
    resolve_with(target, roots, proc_mounts, deadline, |p| std::fs::canonicalize(p)).await
}

/// Like [`resolve`], keeping the base for later confinement checks.
pub async fn resolve_parts(
    target: &Target,
    roots: &Roots,
    proc_mounts: &str,
    deadline: Duration,
) -> Result<Resolved, TargetError> {
    resolve_parts_with(target, roots, proc_mounts, deadline, |p| std::fs::canonicalize(p)).await
}

/// The folders (not files, not symlinks, nothing hidden) of `base/path`.
pub(crate) async fn list_dirs_in<C>(
    base: PathBuf,
    path: &str,
    deadline: Duration,
    canon: C,
) -> Result<Vec<String>, TargetError>
where
    C: Fn(&Path) -> io::Result<PathBuf> + Send + 'static,
{
    let dir = confine(base, path, deadline, canon).await?.dir;
    blocking(deadline, move || {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            // `file_type` does not follow a symlink: a link is never offered.
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Ok(name) = entry.file_name().into_string()
                && !name.starts_with('.')
                && !name.contains('$')
            {
                names.push(name);
            }
        }
        names.sort_by_key(|n| n.to_lowercase());
        Ok(names)
    })
    .await
}

pub async fn list_dirs(
    target: &Target,
    roots: &Roots,
    proc_mounts: &str,
    deadline: Duration,
) -> Result<Vec<String>, TargetError> {
    let root = checked_root(target, roots, proc_mounts)?;
    list_dirs_in(root.base_dir(), &target.path, deadline, |p| std::fs::canonicalize(p)).await
}

/// Makes one folder, `name`, inside `base/path`. `create_dir` and not
/// `create_dir_all`: an existing name, a symlink included, is an error and is
/// never followed.
pub(crate) async fn create_dir_in<C>(
    base: PathBuf,
    path: &str,
    name: &str,
    deadline: Duration,
    canon: C,
) -> Result<(), TargetError>
where
    C: Fn(&Path) -> io::Result<PathBuf> + Send + 'static,
{
    // A leading dot is refused as well: `list_dirs` would hide the folder.
    if !component_ok(name) || name.starts_with('.') {
        return Err(TargetError::Escapes);
    }
    let made = confine(base, path, deadline, canon).await?.dir.join(name);
    blocking(deadline, move || std::fs::create_dir(made)).await
}

pub async fn create_dir(
    target: &Target,
    name: &str,
    roots: &Roots,
    proc_mounts: &str,
    deadline: Duration,
) -> Result<(), TargetError> {
    let root = checked_root(target, roots, proc_mounts)?;
    create_dir_in(root.base_dir(), &target.path, name, deadline, |p| std::fs::canonicalize(p)).await
}

#[cfg(test)]
mod tests;
