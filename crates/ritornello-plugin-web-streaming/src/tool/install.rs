//! Putting a verified release in place without ever losing the working one.
//!
//! The zipapp is extracted (measured: run as a zip it starts in 9 s on the
//! device, because Python cannot cache bytecode inside a zip; extracted, in
//! 1.9 s) into `<data_dir>/yt-dlp.new/`, warmed up there, then swapped in:
//! `yt-dlp` -> `yt-dlp.old`, `yt-dlp.new` -> `yt-dlp`, `yt-dlp.old` removed.
//! A failure before the swap leaves the old copy untouched; a failure or a
//! crash in the middle of it is undone by [`recover`], at the next start.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::wrapper::PACKAGE_DIR;

pub const NEW_DIR: &str = "yt-dlp.new";
pub const OLD_DIR: &str = "yt-dlp.old";

/// What an archive may hold. The real zipapp: 1051 files, 10 MB unpacked.
#[derive(Clone, Copy, Debug)]
pub struct ZipLimits {
    pub max_entries: usize,
    pub max_total: u64,
}

pub const YTDLP_LIMITS: ZipLimits = ZipLimits { max_entries: 20_000, max_total: 256 << 20 };

/// The package was installed whole: its `__init__.py` is there.
pub fn intact(data_dir: &Path) -> bool {
    data_dir.join(PACKAGE_DIR).join("yt_dlp/__init__.py").is_file()
}

fn archive(bytes: &[u8]) -> Result<zip::ZipArchive<std::io::Cursor<&[u8]>>, String> {
    // The zip crate finds the central directory from the end, so the
    // zipapp's shebang line in front is no obstacle (checked on the real
    // asset, see the task report).
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a zip archive: {e}"))
}

/// Extracts every regular file and directory of `bytes` under `dest`, which
/// must not exist. Refused: a name leaving `dest` (an absolute one is read
/// relative to it), a link, more entries or more bytes than `limits`. The
/// caller removes `dest` on error.
pub fn extract(bytes: &[u8], dest: &Path, limits: ZipLimits) -> Result<(), String> {
    let mut zip = archive(bytes)?;
    if zip.len() > limits.max_entries {
        return Err(format!("{} entries, more than {}", zip.len(), limits.max_entries));
    }
    std::fs::create_dir(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().unwrap_or_default().to_string();
        let Some(relative) = file.enclosed_name() else {
            return Err(format!("unsafe name {name:?}"));
        };
        if file.is_symlink() {
            return Err(format!("{name:?} is a link"));
        }
        let path = dest.join(&relative);
        if file.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let budget = limits.max_total - total;
        let copied = std::io::copy(&mut (&mut file).take(budget + 1), &mut out).map_err(|e| format!("{name}: {e}"))?;
        if copied > budget {
            return Err(format!("more than {} bytes unpacked", limits.max_total));
        }
        total += copied;
    }
    Ok(())
}

/// The single file `name` of the zip `bytes`, at most `max` bytes.
pub fn extract_one(bytes: &[u8], name: &str, max: u64) -> Result<Vec<u8>, String> {
    let mut zip = archive(bytes)?;
    let file = zip.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut out = Vec::new();
    file.take(max + 1).read_to_end(&mut out).map_err(|e| format!("{name}: {e}"))?;
    if out.len() as u64 > max {
        return Err(format!("{name}: more than {max} bytes"));
    }
    Ok(out)
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// Puts `yt-dlp.new` in place of `yt-dlp`. On error, the old copy is back
/// where it was.
pub fn swap_in(data_dir: &Path) -> std::io::Result<()> {
    let (current, new, old) = (data_dir.join(PACKAGE_DIR), data_dir.join(NEW_DIR), data_dir.join(OLD_DIR));
    let had_one = exists(&current);
    if had_one {
        if exists(&old) {
            std::fs::remove_dir_all(&old)?;
        }
        std::fs::rename(&current, &old)?;
    }
    if let Err(e) = std::fs::rename(&new, &current) {
        if had_one {
            let _ = std::fs::rename(&old, &current);
        }
        return Err(e);
    }
    if had_one && let Err(e) = std::fs::remove_dir_all(&old) {
        // Harmless: the next swap or start removes it.
        tracing::warn!("{} not removed: {e}", old.display());
    }
    Ok(())
}

/// At start: undoes a swap that a crash interrupted (no `yt-dlp`, but an
/// `yt-dlp.old`), and removes what an interrupted install left.
pub fn recover(data_dir: &Path) {
    let (current, new, old) = (data_dir.join(PACKAGE_DIR), data_dir.join(NEW_DIR), data_dir.join(OLD_DIR));
    if !exists(&current) && exists(&old) {
        match std::fs::rename(&old, &current) {
            Ok(()) => tracing::warn!("an interrupted yt-dlp update was rolled back"),
            Err(e) => tracing::warn!("{} not restored: {e}", old.display()),
        }
    }
    for leftover in [new, old] {
        if exists(&leftover) {
            let _ = std::fs::remove_dir_all(&leftover);
        }
    }
}

/// Writes `bytes` to `path` with mode 0755, atomically: a temporary file in
/// the same directory, renamed over `path`. The temporary name is unique to
/// the process and to the call: two writers of the same file never share
/// one, so neither can publish the other's half-written file.
pub fn write_executable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp: PathBuf = dir.join(format!(".{name}.tmp.{}.{n}", std::process::id()));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o755).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        // The umask may have narrowed the creation mode.
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::testkit::*;

    fn tree(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    out.push(rel);
                }
            }
        }
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn a_zipapp_behind_its_shebang_extracts_whole() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out");
        extract(&fake_zipapp("1.0"), &dest, YTDLP_LIMITS).unwrap();
        assert_eq!(
            tree(&dest),
            vec!["__main__.py", "yt_dlp/__init__.py", "yt_dlp/extractor/__init__.py", "yt_dlp/version.py"]
        );
    }

    #[test]
    fn unsafe_names_and_oversized_archives_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in ["../escape.py", "a/../../escape.py"].iter().enumerate() {
            let dest = dir.path().join(format!("out{i}"));
            let got = extract(&zipapp(&[("ok.py", "x"), (name, "x")]), &dest, YTDLP_LIMITS);
            assert!(got.as_ref().is_err_and(|e| e.contains("unsafe")), "{name}: {got:?} {:?}", tree(&dest));
        }
        assert!(!dir.path().join("escape.py").exists());
        // An absolute name is read relative to the destination (the zip
        // crate drops the root): it lands inside, never at `/`.
        let dest = dir.path().join("abs");
        extract(&zipapp(&[("/abs-ritornello-test.py", "x")]), &dest, YTDLP_LIMITS).unwrap();
        assert_eq!(tree(&dest), vec!["abs-ritornello-test.py"]);
        assert!(!Path::new("/abs-ritornello-test.py").exists());

        let big = "x".repeat(1000);
        let err = extract(&zipapp(&[("a", &big), ("b", &big)]), &dir.path().join("big"), ZipLimits { max_entries: 10, max_total: 1500 })
            .unwrap_err();
        assert!(err.contains("bytes"), "{err}");
        let err = extract(&zipapp(&[("a", "1"), ("b", "2")]), &dir.path().join("many"), ZipLimits { max_entries: 1, max_total: 1500 })
            .unwrap_err();
        assert!(err.contains("entries"), "{err}");
        // Exactly at the limits is fine.
        extract(&zipapp(&[("a", &big[..750]), ("b", &big[..750])]), &dir.path().join("exact"), ZipLimits { max_entries: 2, max_total: 1500 })
            .unwrap();
    }

    #[test]
    fn one_file_is_taken_whole_and_no_larger_than_allowed() {
        let zip = zipapp(&[("deno", "0123456789"), ("other", "x")]);
        assert_eq!(extract_one(&zip, "deno", 10).unwrap(), b"0123456789");
        assert!(extract_one(&zip, "deno", 9).unwrap_err().contains("more than 9"));
        assert!(extract_one(&zip, "missing", 10).is_err());
    }

    #[test]
    fn a_link_in_the_archive_is_refused() {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut out);
            w.add_symlink("yt_dlp", "/etc", zip::write::SimpleFileOptions::default()).unwrap();
            w.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let err = extract(&out.into_inner(), &dir.path().join("out"), YTDLP_LIMITS).unwrap_err();
        assert!(err.contains("link"), "{err}");
    }

    #[test]
    fn the_swap_replaces_the_copy_and_keeps_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("yt-dlp/yt_dlp")).unwrap();
        std::fs::write(d.join("yt-dlp/yt_dlp/__init__.py"), "old").unwrap();
        std::fs::create_dir_all(d.join("yt-dlp.new/yt_dlp")).unwrap();
        std::fs::write(d.join("yt-dlp.new/yt_dlp/__init__.py"), "new").unwrap();
        std::fs::create_dir_all(d.join("yt-dlp.old")).unwrap(); // a leftover
        swap_in(d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("yt-dlp/yt_dlp/__init__.py")).unwrap(), "new");
        assert!(!d.join("yt-dlp.new").exists() && !d.join("yt-dlp.old").exists());
        // The first install has nothing to move aside.
        let fresh = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fresh.path().join("yt-dlp.new/yt_dlp")).unwrap();
        swap_in(fresh.path()).unwrap();
        assert!(fresh.path().join("yt-dlp/yt_dlp").is_dir());
    }

    #[test]
    fn a_swap_that_fails_puts_the_old_copy_back() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("yt-dlp/yt_dlp")).unwrap();
        std::fs::write(d.join("yt-dlp/yt_dlp/__init__.py"), "old").unwrap();
        // No yt-dlp.new: the second rename fails.
        assert!(swap_in(d).is_err());
        assert_eq!(std::fs::read_to_string(d.join("yt-dlp/yt_dlp/__init__.py")).unwrap(), "old");
    }

    #[test]
    fn an_interrupted_swap_is_rolled_back_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // Crashed between the two renames: no yt-dlp, the old copy aside.
        std::fs::create_dir_all(d.join("yt-dlp.old/yt_dlp")).unwrap();
        std::fs::write(d.join("yt-dlp.old/yt_dlp/__init__.py"), "old").unwrap();
        std::fs::create_dir_all(d.join("yt-dlp.new/yt_dlp")).unwrap();
        recover(d);
        assert!(intact(d));
        assert_eq!(std::fs::read_to_string(d.join("yt-dlp/yt_dlp/__init__.py")).unwrap(), "old");
        assert!(!d.join("yt-dlp.new").exists() && !d.join("yt-dlp.old").exists());
        // A crash after the swap: both there, the leftover goes.
        std::fs::create_dir_all(d.join("yt-dlp.old")).unwrap();
        recover(d);
        assert!(intact(d) && !d.join("yt-dlp.old").exists());
    }

    #[test]
    fn executables_are_written_whole_with_their_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin/yt-dlp");
        write_executable(&p, b"one").unwrap();
        write_executable(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_dir(dir.path().join("bin")).unwrap().count(), 1, "no temporary left");
    }

    /// Writers of the same file at once: every write succeeds, and what is
    /// in place is always one writer's whole content.
    #[test]
    fn concurrent_writers_never_share_a_temporary_nor_publish_half_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin/yt-dlp");
        write_executable(&p, &[b'0'; 64 << 10]).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (p, stop) = (p.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let b = std::fs::read(&p).unwrap();
                    assert_eq!(b.len(), 64 << 10, "a half-written file was in place");
                    assert!(b.iter().all(|x| *x == b[0]), "two writers' bytes mixed");
                }
            })
        };
        let writers: Vec<_> = (0..8u8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for _ in 0..40 {
                        write_executable(&p, &[b'a' + i; 64 << 10]).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();
        assert_eq!(std::fs::read_dir(dir.path().join("bin")).unwrap().count(), 1, "no temporary left");
    }
}
