//! What an archive contains, and what may be done with it.
//!
//! **This is where the trust in a downloaded archive stops.** The core reads
//! it entry by entry and takes exactly what it recognises: one binary, the
//! files under the two release-owned subdirectories of `etc/ritornello`, an
//! initial configuration, a `plugins.toml` block. Everything else — a systemd
//! unit, a polkit rule, a root-run helper, the operator's own configuration —
//! is listed so the page can say the release changes it, and **never
//! written**. `tar -x` would take it all and then need undoing.
//!
//! Two caps, and they are not the same cap. The download bounds the
//! **compressed** size; this module bounds the **decompressed** one, because
//! gzip expands and this runs on a device with a gigabyte of memory. Both the
//! payload (per entry, via a bounded `take`) and tar's own framing — GNU
//! long-name and PAX members, consumed before an entry is ever yielded — are
//! bounded: a `Bounded` reader wraps the raw decoder so a hostile header
//! errors instead of allocating.

use std::io::Read;

/// Where a plugin binary lives inside an archive.
pub const PLUGINS_PREFIX: &str = "usr/local/lib/ritornello/plugins/";
/// The only two places under `/etc/ritornello` a release owns. Deliberately
/// not `etc/ritornello/` at large: the operator's own files live there —
/// `plugins.toml`, `stations.toml`, `input-bindings.toml`, `media-roots.toml`,
/// the NAS credentials — and `etc_files` is written unconditionally, so a
/// wide prefix would let a release replace a station list and a plugin
/// declaration. Verified against `deploy/packaging.toml`, which ships these
/// two and nothing else under that root.
pub const ETC_PREFIXES: &[&str] = &["etc/ritornello/locales/", "etc/ritornello/input-presets/"];
/// Files that become a plugin's configuration **only if it is absent**.
pub const INITIAL_CONFIG_PREFIX: &str = "initial-config/";
/// The `[[plugin]]` block to append to `plugins.toml`.
pub const FRAGMENT_NAME: &str = "plugins.toml.fragment";
/// Where the core binary lives inside the core archive.
pub const CORE_BINARY: &str = "usr/local/bin/ritornello-core";
/// Reference files, shipped for reading and never written anywhere.
const EXAMPLES_PREFIX: &str = "examples/";

/// 96 MiB of decompressed bytes. The largest archive this repository produces
/// is the plugin bundle, a few tens of megabytes of stripped binaries; this
/// leaves room to grow and still refuses a bomb long before the OOM killer
/// takes an interest in the core.
pub const DECOMPRESSED_MAX: usize = 96 * 1024 * 1024;

/// Tar's framing on top of the payload: a 512-byte header per entry plus the
/// end-of-archive padding. Eight mebibytes covers sixteen thousand entries,
/// far more than any archive this project produces, and is still finite.
///
/// Added to `cap` rather than compared against it alone: tar pads every entry
/// to a 512-byte block and appends an end-of-archive marker, so a budget of
/// exactly `cap` would refuse an archive whose payload sits exactly at the
/// cap — the case the `+ 1` in the per-entry `take` below exists to keep
/// working. A hostile long name can still spend this budget instead of a real
/// entry's content; that is by design, since a legitimate 96 MiB archive
/// costs the same slack, and the defect this constant fixes was
/// unboundedness, not the size of the budget.
const FRAMING_SLACK: usize = 8 * 1024 * 1024;

/// An entry name longer than this is refused. Checked twice, and the two
/// checks are not a tidy duplicate of each other: the first, on
/// `entry.path_bytes()`, measures the name as it arrived, before `read` ever
/// copies it into a `String` — for a name over the `Bounded` budget this is
/// defence in depth (`Bounded` is what stops that allocation), and for a name
/// merely long but under budget it is what avoids copying a name nobody is
/// going to keep. The second check runs on the name only after it has been
/// copied AND had a leading `./` stripped, so it measures up to two bytes
/// fewer than the first did. A name of exactly `NAME_MAX + 2` bytes carrying
/// that prefix is refused by the first and would sail past the second with
/// no error at all — the first check is the only guard for that window, not
/// a redundant one.
const NAME_MAX: usize = 4096;

#[derive(Debug)]
pub enum ArchiveError {
    /// Not a gzipped tar at all: an HTML error page, a truncated download.
    Unreadable(String),
    /// More decompressed bytes than the cap allows — an entry's payload, or
    /// tar's own framing (a GNU long-name or PAX member, or plain padding).
    TooLarge(usize),
    /// An entry whose name escapes its own prefix, is absolute, or whose type
    /// is neither a plain file nor a directory.
    UnsafeEntry(String),
    /// An entry name longer than `NAME_MAX`. Its own variant because
    /// `TooLarge` names the decompressed cap, and reporting a five-kilobyte
    /// name as "decompresses to more than 96 MiB" sends the reader looking
    /// for the wrong thing.
    NameTooLong(usize),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(d) => write!(f, "the archive could not be read: {d}"),
            Self::TooLarge(cap) => write!(f, "the archive decompresses to more than {cap} bytes"),
            Self::UnsafeEntry(name) => write!(f, "the archive holds an unsafe entry name: {name:?}"),
            Self::NameTooLong(max) => write!(f, "the archive holds an entry name longer than {max} bytes"),
        }
    }
}

impl std::error::Error for ArchiveError {}

#[derive(Debug, Clone, Default)]
pub struct Contents {
    /// Every entry's path, `./` stripped, directories included and
    /// normalised to end with `/` regardless of how tar wrote them. This is
    /// what `installable_from_ui` reads, and what lets the page say a
    /// release changes a unit.
    pub entries: Vec<String>,
    /// `(file name, bytes)` of the single plugin binary, when there is one
    /// directly under `PLUGINS_PREFIX` — not nested in a subdirectory.
    pub binary: Option<(String, Vec<u8>)>,
    pub core_binary: Option<Vec<u8>>,
    /// Paths under the prefixes in `ETC_PREFIXES`, with their bytes. Written
    /// unconditionally by the core: locale catalogs and input presets belong
    /// to the release, not to the operator.
    pub etc_files: Vec<(String, Vec<u8>)>,
    /// `(bare name, bytes)`. Written **only if the target is absent**.
    pub initial_config: Vec<(String, Vec<u8>)>,
    pub fragment: Option<String>,
}

/// True when everything the archive carries is something the core can install
/// without root writing anywhere but the plugins directory.
///
/// Derived from the artefact, not from a list this repository would have to
/// keep in step with itself. Today exactly one **plugin** archive fails it —
/// the files plugin, which ships a root-run mount helper, a unit and a polkit
/// rule — and that is a fact about the archive rather than a name in a table.
///
/// **A plugin rule, and only a plugin rule.** The core's own archive fails it
/// too, and always will: it carries the core binary itself, the privileged
/// installer, three systemd units and two polkit rules. Root can form the core
/// binary's path, so the core is deliberately exempted from this check — see
/// the comment at the call site in `update::Worker::install_one`, and the test
/// `the_core_archive_could_never_pass_the_rule_that_governs_a_plugin` that
/// pins the real entry lists of both.
pub fn installable_from_ui(entries: &[String]) -> bool {
    let mut binaries = 0;
    for entry in entries {
        // A directory entry describes no content: `tar` writes them for the
        // parents of what it packs, and judging an archive on them would make
        // every archive uninstallable.
        //
        // `read` now normalises every directory it yields to end with `/`
        // (from its own entry-type check, not by inspecting this string), so
        // for a list built by `read` this branch is exactly what makes a
        // directory harmless — still reachable, not dead code. But
        // `installable_from_ui` takes a plain list of names, and a caller
        // other than `read` could hand it a directory name some other way;
        // kept for that caller too.
        if entry.ends_with('/') {
            continue;
        }
        if let Some(rest) = entry.strip_prefix(PLUGINS_PREFIX) {
            // A nested path (`plugins/sub/evil`), or the empty-then-slash
            // shape a doubled separator produces (`plugins//x` strips to
            // `/x`): `target.rs` on the privileged side only ever forms one
            // validated name directly under `plugins/`, so a rest containing
            // `/` can never become an installed path. Refusing here means
            // the page never calls a shape installable that root would go on
            // to refuse after the download.
            if rest.contains('/') {
                return false;
            }
            if !rest.is_empty() {
                binaries += 1;
            }
            continue;
        }
        let allowed = ETC_PREFIXES.iter().any(|p| entry.starts_with(p))
            || entry.starts_with(INITIAL_CONFIG_PREFIX)
            || entry.starts_with(EXAMPLES_PREFIX)
            || entry == FRAGMENT_NAME;
        if !allowed {
            return false;
        }
    }
    // Exactly one, not at least one: with two, the core would have to pick,
    // and picking silently is how a wrong binary gets installed.
    binaries == 1
}

/// True when the archive carries **its plugin binary and nothing else**.
///
/// The rule for a third-party archive, and it is strictly stronger than
/// `installable_from_ui`: that one also allows `etc/ritornello/locales/`,
/// `etc/ritornello/input-presets/`, an initial configuration, examples and a
/// `plugins.toml.fragment` — all of which the **core** writes, unprivileged,
/// with its own hands.
///
/// Only the binary, and this is where that is enforced rather than promised.
/// The privileged side cannot write outside the plugins directory anyway, but
/// the core CAN write `/etc/ritornello` — so without this check a third-party
/// archive would be handed the operator's configuration directory by the one
/// component allowed to write it. The fragment is refused for the same reason
/// and a sharper one: a `[[plugin]]` block names an `exec` path, and appending
/// one is asking the core to launch whatever path a stranger's archive chose.
///
/// A consequence worth stating rather than discovering: a third-party plugin
/// can therefore only ever be **updated** from the UI, never freshly
/// installed, because a fresh install is exactly the case that needs a
/// fragment. That costs nothing in practice — a plugin is only ever known to
/// be third-party because it ran and announced its repository, which means it
/// was already declared by hand.
pub fn only_its_own_binary(entries: &[String]) -> bool {
    let mut binaries = 0;
    for entry in entries {
        // A directory entry describes no content, exactly as in
        // `installable_from_ui`: `tar` writes them for the parents of what it
        // packs, so judging on them would refuse every archive.
        if entry.ends_with('/') {
            continue;
        }
        let Some(rest) = entry.strip_prefix(PLUGINS_PREFIX) else {
            return false;
        };
        // A nested path (`plugins/sub/evil`), or the doubled-separator shape
        // (`plugins//x` strips to `/x`): the privileged side only ever forms
        // one validated bare name directly under `plugins/`.
        if rest.is_empty() || rest.contains('/') {
            return false;
        }
        binaries += 1;
    }
    // Exactly one, not at least one: with two, the core would have to pick,
    // and picking silently is how a wrong binary gets installed.
    binaries == 1
}

/// What a **core** archive carries that `install_one` never places anywhere:
/// everything outside the core binary and the two `etc/ritornello`
/// subdirectories a release owns. In practice this is the privileged
/// installer, the systemd units and the polkit rules — root could in
/// principle place some of these, but nothing on this unprivileged side ever
/// does, so this is exactly what a core update did not touch.
///
/// `PLUGINS_PREFIX` is excluded too, even though a core archive has never
/// carried one: the rule this function states is "everything `install_one`
/// places", and that prefix is one of the places it looks, whichever archive
/// is asked about.
///
/// Directory entries are dropped: they describe no content, and the page has
/// nothing to say about a directory a release "carries".
pub fn core_not_installed(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| !e.ends_with('/'))
        .filter(|e| e.as_str() != CORE_BINARY)
        .filter(|e| !e.starts_with(PLUGINS_PREFIX))
        .filter(|e| !ETC_PREFIXES.iter().any(|p| e.starts_with(p)))
        .cloned()
        .collect()
}

/// Rejects an absolute path or any component that could climb out.
///
/// Nothing downstream extracts to a path taken from the archive, so this is
/// belt and braces — but an entry that escapes is a sign of a hostile archive,
/// and one refusal here is cheaper than reasoning about every consumer.
fn safe(name: &str) -> bool {
    !name.starts_with('/')
        && !name.split('/').any(|c| c == ".." )
}

/// A reader that FAILS once its budget is spent, rather than reporting end of
/// stream.
///
/// `Read::take` cannot do this: it reports a silent EOF, which the tar reader
/// then surfaces as a truncated archive — `Unreadable`, where the honest
/// answer is `TooLarge`. That mislabelling is why a stream-level `take` was
/// removed at one point, which left the tar crate's own header handling
/// unbounded: GNU long-name and PAX members are consumed BEFORE an entry is
/// ever yielded, so no per-entry cap can see them. A 455 KB compressed
/// archive declaring a 100 MB entry name allocated 389 MB of RSS on nothing
/// more than iterating.
///
/// The tripped flag is what lets `read` distinguish its own refusal from a
/// genuinely corrupt stream, since tar wraps the io error either way.
struct Bounded<R> {
    inner: R,
    left: usize,
    tripped: std::rc::Rc<std::cell::Cell<bool>>,
}

impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        match self.left.checked_sub(n) {
            Some(left) => {
                self.left = left;
                Ok(n)
            }
            None => {
                self.tripped.set(true);
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "decompressed bound exceeded",
                ))
            }
        }
    }
}

/// Turns an io error tar surfaced into the right `ArchiveError`: `TooLarge` if
/// `Bounded` tripped, `Unreadable` otherwise (a genuinely truncated or
/// malformed stream). Tar wraps the io error identically either way, so the
/// flag is the only way to tell the two apart.
fn unreadable(e: std::io::Error, tripped: &std::rc::Rc<std::cell::Cell<bool>>, cap: usize) -> ArchiveError {
    if tripped.get() {
        ArchiveError::TooLarge(cap)
    } else {
        ArchiveError::Unreadable(e.to_string())
    }
}

/// Reads a `.tar.gz` archive into memory, entry by entry, taking only what
/// this module recognises.
///
/// **Every decompressed byte spends the budget, wanted or not.** `total`
/// below only ever charges a *wanted* entry's content against `cap`, but
/// `Bounded` sees every byte tar reads to get there — including the full
/// content of an entry outside every known prefix, since tar must read
/// through it (this source is not seekable) to reach the next header. The
/// effective bound on the whole archive is therefore "the entire decompressed
/// stream fits in `cap + FRAMING_SLACK`", not merely "the content this module
/// keeps fits in `cap`" — stricter than the name suggests, and worth knowing
/// before debugging a refusal on an archive whose *wanted* content alone
/// looks well under the cap.
pub fn read(gz: &[u8], cap: usize) -> Result<Contents, ArchiveError> {
    let decoder = flate2::read::GzDecoder::new(gz);
    let tripped = std::rc::Rc::new(std::cell::Cell::new(false));
    let bounded = Bounded {
        inner: decoder,
        left: cap.saturating_add(FRAMING_SLACK),
        tripped: tripped.clone(),
    };
    let mut archive = tar::Archive::new(bounded);
    let entries = archive
        .entries()
        .map_err(|e| unreadable(e, &tripped, cap))?;

    let mut out = Contents::default();
    let mut total = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| unreadable(e, &tripped, cap))?;
        // Measured before the name is ever copied into a `String`:
        // `entry.path()` below allocates one, and a legitimately-yielded
        // name (under the `Bounded` budget, so never caught by it) that is
        // merely long doubles its own memory for nothing. A 477 KB archive
        // declaring a 100 MiB name cost +214 MB of RSS this way; checking
        // the byte length tar already holds, before copying it, avoids that
        // entirely.
        //
        // This is not just the cheaper of two equivalent checks: it is the
        // ONLY guard for a name of exactly `NAME_MAX + 2` bytes carrying a
        // leading `./`. The later check, after the `./` strip, measures up
        // to two bytes fewer than this one does — such a name passes that
        // one with room to spare. Removing this check would not merely delay
        // the refusal, it would remove it.
        if entry.path_bytes().len() > NAME_MAX {
            return Err(ArchiveError::NameTooLong(NAME_MAX));
        }
        let raw = entry
            .path()
            .map_err(|e| unreadable(e, &tripped, cap))?
            .to_string_lossy()
            .to_string();
        // `tar -C <dir> … .` — the form scripts/package-release.sh uses —
        // writes a first entry that is literally `./`, and `./`-prefixed
        // names for everything else. The prefix is stripped only when
        // something follows: stripping `./` itself yields the empty string,
        // which matches no allowed prefix and made EVERY real archive
        // uninstallable. No fixture built with `tar::Builder` can show that,
        // because it drops `./` at write time — this needs raw headers.
        let name = raw.strip_prefix("./").unwrap_or(&raw);
        if name.is_empty() || name == "." {
            continue;
        }
        if name.len() > NAME_MAX {
            // Reached only if the check above was somehow bypassed or wrong:
            // a second, independent guard on the same constraint rather than
            // trusting the first one alone.
            return Err(ArchiveError::NameTooLong(NAME_MAX));
        }
        if !safe(name) {
            return Err(ArchiveError::UnsafeEntry(name.to_string()));
        }

        let mut path = name.to_string();
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            // Normalised to end with `/` whatever tar wrote, so the
            // whitelist above sees a directory as a directory regardless of
            // the exact bytes of the name.
            if !path.ends_with('/') {
                path.push('/');
            }
            out.entries.push(path);
            continue;
        }
        if !entry_type.is_file() {
            // Our own archives contain only files and directories. Before
            // this check, only a trailing slash distinguished a directory,
            // so a symlink under `plugins/` counted as the one binary with
            // empty content, and a `CORE_BINARY` symlink yielded a zero-byte
            // core. A shape we never produce earns a named refusal instead.
            return Err(ArchiveError::UnsafeEntry(path));
        }
        if path.ends_with('/') {
            // A `Regular`-typed entry whose name ends in `/` is a
            // type-versus-name-shape mismatch our own archives never
            // produce. Without this, it would be silently skipped by every
            // consumer that treats a trailing slash as "this is a
            // directory" — the whitelist above and `installable_from_ui` —
            // while still being collected into `etc_files` under an allowed
            // prefix, since that collection keys off the name, not the type.
            return Err(ArchiveError::UnsafeEntry(path));
        }
        out.entries.push(path.clone());

        let want = path == CORE_BINARY
            || path.starts_with(PLUGINS_PREFIX)
            || ETC_PREFIXES.iter().any(|p| path.starts_with(p))
            || path.starts_with(INITIAL_CONFIG_PREFIX)
            || path == FRAGMENT_NAME;
        if !want {
            continue;
        }

        // Bound per entry, not on the stream: a stream-level `take` would
        // truncate the tar reader before this counter could ever exceed the
        // cap, turning a legitimate `TooLarge` into a spurious `Unreadable`.
        // `cap + 1`: reading one byte past the cap is what distinguishes
        // "exactly at the limit" from "over it", and it is what makes the
        // refusal happen before the rest of the archive is even looked at.
        // `saturating_add` rather than `cap as u64 + 1`, which overflows in
        // debug builds when `cap` is `usize::MAX`.
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take((cap as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| unreadable(e, &tripped, cap))?;
        total += bytes.len();
        if total > cap {
            return Err(ArchiveError::TooLarge(cap));
        }

        if path == CORE_BINARY {
            out.core_binary = Some(bytes);
        } else if let Some(name) = path.strip_prefix(PLUGINS_PREFIX) {
            // A second binary, a nested one, or the shape a doubled
            // separator produces replaces nothing: `installable_from_ui`
            // refuses all three by the same rule (nothing left in `name`
            // but a bare, non-empty component), and the bundle is never
            // installed component by component.
            if out.binary.is_none() && !name.is_empty() && !name.contains('/') {
                out.binary = Some((name.to_string(), bytes));
            }
        } else if ETC_PREFIXES.iter().any(|p| path.starts_with(p)) {
            out.etc_files.push((path, bytes));
        } else if let Some(name) = path.strip_prefix(INITIAL_CONFIG_PREFIX) {
            // Bare, as the field's doc says — enforced here rather than only
            // promised. A nested entry (`initial-config/sub/x.toml`) is a
            // shape nothing this repository packs produces, and the writer
            // would have to create a directory an archive named. The writer
            // refuses it too (`update::initial_config_target`): belt and
            // braces, which is this module's own stated rule, and the reader
            // is the half a later consumer of `Contents` will trust.
            if !name.is_empty() && !name.contains('/') {
                out.initial_config.push((name.to_string(), bytes));
            }
        } else {
            out.fragment = Some(String::from_utf8_lossy(&bytes).to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a real .tar.gz in memory from (path, bytes) pairs.
    ///
    /// A real archive and not a stub: the point of these tests is the shape of
    /// what our own packaging script produces, and a hand-made fixture would
    /// drift from it without saying so.
    fn targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).expect("append");
        }
        let tar = builder.into_inner().expect("finish");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).expect("compress");
        gz.finish().expect("finish gz")
    }

    /// Appends one entry via the header's raw name bytes, bypassing
    /// `tar::Header::set_path`'s write-side validation — which refuses an
    /// absolute path, a `..` component, a bare `./` root, and (via
    /// `Path::components()` collapsing repeated separators) a doubled `/`.
    /// Every fixture below that needs one of those shapes goes through this
    /// helper instead of `append_data`.
    fn raw_entry(builder: &mut tar::Builder<Vec<u8>>, name: &[u8], entry_type: tar::EntryType, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, data).expect("append");
    }

    fn gzip(tar_bytes: Vec<u8>) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_bytes).expect("compress");
        gz.finish().expect("finish gz")
    }

    /// The same relative layout `package-release.sh` produces for a plugin
    /// with a locale catalog and an example — but not the literal bytes: real
    /// GNU tar, invoked the way that script invokes it, writes a `./` root
    /// entry and a parent directory entry for every level in between, and
    /// `tar::Builder` cannot express either (it drops a leading `./`
    /// component at write time). See
    /// `a_real_dot_slash_root_entry_is_installable_and_never_reaches_entries`
    /// below, built from raw headers, for that shape.
    fn a_plugin_archive() -> Vec<u8> {
        targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
            ("./etc/ritornello/locales/radio/fr.toml", b"a = \"b\"\n"),
            ("./examples/stations.example.toml", b"# stations\n"),
            ("./plugins.toml.fragment", b"[[plugin]]\nname = \"radio\"\nexec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"\n"),
        ])
    }

    #[test]
    fn a_plugin_archive_yields_its_binary_its_etc_files_and_its_fragment() {
        let c = read(&a_plugin_archive(), DECOMPRESSED_MAX).expect("reads");
        assert_eq!(c.binary.as_ref().map(|(n, _)| n.as_str()), Some("ritornello-plugin-radio"));
        assert_eq!(c.binary.as_ref().map(|(_, b)| b.as_slice()), Some(b"ELF".as_slice()));
        assert_eq!(c.etc_files.len(), 1);
        assert_eq!(c.etc_files[0].0, "etc/ritornello/locales/radio/fr.toml");
        assert!(c.fragment.as_deref().unwrap().contains("name = \"radio\""));
        assert!(c.core_binary.is_none());
        assert!(c.initial_config.is_empty());
    }

    #[test]
    fn the_leading_dot_slash_is_stripped_because_tar_c_writes_it() {
        let c = read(&a_plugin_archive(), DECOMPRESSED_MAX).expect("reads");
        assert!(
            c.entries.iter().all(|e| !e.starts_with("./")),
            "entries still carry ./: {:?}",
            c.entries
        );
    }

    /// `tar::Builder` never writes a bare `./` root entry, or a parent
    /// directory entry like `./usr/` on its own — only real GNU tar, invoked
    /// the way `scripts/package-release.sh` invokes it
    /// (`tar -C "$stage" … -czf "$archive" .`), does. Built from raw header
    /// bytes for the same reason the escaping-entry and absolute-path
    /// fixtures are: there is no `tar::Builder` call that reaches this shape,
    /// so no fixture built the ordinary way could have shown that this exact
    /// byte shape used to make every real archive uninstallable.
    ///
    /// GNU tar types the root entry `Directory`, not `Regular`, so an
    /// unskipped `./` does not become the empty string `read` would drop —
    /// it comes back out through the directory-normalisation below as `"/"`.
    /// Only the third clause of the assertion, `e != "/"`, catches that; the
    /// first two alone let an unskipped `./` straight through.
    #[test]
    fn a_real_dot_slash_root_entry_is_installable_and_never_reaches_entries() {
        let mut builder = tar::Builder::new(Vec::new());
        for name in [
            "./",
            "./etc/",
            "./etc/ritornello/",
            "./etc/ritornello/locales/",
            "./etc/ritornello/locales/radio/",
            "./usr/",
            "./usr/local/",
            "./usr/local/lib/",
            "./usr/local/lib/ritornello/",
            "./usr/local/lib/ritornello/plugins/",
        ] {
            raw_entry(&mut builder, name.as_bytes(), tar::EntryType::Directory, b"");
        }
        raw_entry(
            &mut builder,
            b"./etc/ritornello/locales/radio/fr.toml",
            tar::EntryType::Regular,
            b"a = \"b\"\n",
        );
        raw_entry(
            &mut builder,
            b"./usr/local/lib/ritornello/plugins/ritornello-plugin-radio",
            tar::EntryType::Regular,
            b"ELF",
        );

        let tar = builder.into_inner().unwrap();
        let c = read(&gzip(tar), DECOMPRESSED_MAX).expect("reads");

        assert!(installable_from_ui(&c.entries), "{:?}", c.entries);
        assert!(
            c.entries.iter().all(|e| !e.is_empty() && e != "." && e != "/"),
            "the `./` root or a bare `.` reached entries: {:?}",
            c.entries
        );
    }

    #[test]
    fn a_core_archive_yields_the_core_binary() {
        let gz = targz(&[
            ("./usr/local/bin/ritornello-core", b"ELF"),
            ("./etc/systemd/system/ritornello.service", b"[Unit]\n"),
            ("./etc/polkit-1/rules.d/50-ritornello-power.rules", b"// js\n"),
            ("./etc/ritornello/locales/core/fr.toml", b"a = \"b\"\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert_eq!(c.core_binary.as_deref(), Some(b"ELF".as_slice()));
        // The unit and the polkit rule appear in `entries`, so the page can
        // say "this release changes a unit", but they are NOT collected as
        // files to write: nothing in this product writes them but a human.
        assert!(c.entries.iter().any(|e| e == "etc/polkit-1/rules.d/50-ritornello-power.rules"));
        assert_eq!(c.etc_files.len(), 1, "only etc/ritornello files are collected");
    }

    #[test]
    fn a_plugin_with_an_initial_configuration_yields_it_separately() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
            ("./initial-config/stations.example.toml", b"# defaults\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert_eq!(c.initial_config.len(), 1);
        assert_eq!(c.initial_config[0].0, "stations.example.toml");
        // Separate from `etc_files` on purpose: an etc file is written
        // unconditionally, an initial configuration only if absent. Merging
        // the two would overwrite the operator's stations at the next update.
        assert!(c.etc_files.is_empty());
    }

    /// "(bare name, bytes)" is what the field promises, so it is what the
    /// reader hands over: a nested entry is listed in `entries` — the page can
    /// still say the release carries it — and collected nowhere. The writer
    /// refuses it a second time when it forms the path.
    #[test]
    fn a_nested_initial_configuration_entry_is_listed_and_not_collected() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
            ("./initial-config/sub/stations.example.toml", b"# defaults\n"),
            ("./initial-config/stations.example.toml", b"# defaults\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert_eq!(
            c.initial_config.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["stations.example.toml"],
            "only the bare one is handed over"
        );
        assert!(
            c.entries.iter().any(|e| e == "initial-config/sub/stations.example.toml"),
            "still listed: the page says what the release carries, whatever is written"
        );
    }

    #[test]
    fn a_plugin_carrying_only_a_binary_and_etc_is_installable_from_the_ui() {
        let c = read(&a_plugin_archive(), DECOMPRESSED_MAX).expect("reads");
        assert!(installable_from_ui(&c.entries));
    }

    /// The one that decides the rule, and it is a real archive shape: the
    /// files plugin ships a root-run mount helper, a systemd unit and a polkit
    /// rule. Root can form none of those paths, so the UI must say "install
    /// this one by hand" rather than half-install it.
    #[test]
    fn the_files_plugin_shape_is_not_installable_from_the_ui() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-files", b"ELF"),
            ("./usr/local/lib/ritornello/ritornello-media-mount", b"ELF"),
            ("./etc/systemd/system/ritornello-media-mount.service", b"[Unit]\n"),
            ("./etc/polkit-1/rules.d/51-ritornello-media.rules", b"// js\n"),
            ("./examples/media-roots.example.toml", b"# roots\n"),
            ("./plugins.toml.fragment", b"[[plugin]]\nname = \"files\"\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(!installable_from_ui(&c.entries));
    }

    /// The three tests below each carry ONE disqualifying entry, because the
    /// whole-shape files-plugin test above cannot say which entry did it: that
    /// archive is refused three times over, so whitelisting any single path
    /// would leave it refused and the mistake would not show.
    ///
    /// This one is the root-run helper. It sits beside the plugins directory
    /// rather than inside it, and root can form no path to it — the two paths
    /// the privileged side computes are the core binary and one validated
    /// name under `plugins/`.
    #[test]
    fn a_root_run_helper_beside_the_plugins_directory_is_refused() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-files", b"ELF"),
            ("./usr/local/lib/ritornello/ritornello-media-mount", b"ELF"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(!installable_from_ui(&c.entries));
    }

    /// A unit is a promise to systemd that only root can make, and installing
    /// one from the page would mean the core deciding what runs at boot.
    #[test]
    fn a_systemd_unit_is_refused() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-files", b"ELF"),
            ("./etc/systemd/system/ritornello-media-mount.service", b"[Unit]\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(!installable_from_ui(&c.entries));
    }

    /// A polkit rule is JavaScript that polkitd evaluates as root. Installing
    /// one from an unprivileged process is the privilege escalation this whole
    /// split exists to prevent.
    #[test]
    fn a_polkit_rule_is_refused() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-files", b"ELF"),
            ("./etc/polkit-1/rules.d/51-ritornello-media.rules", b"// js\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(!installable_from_ui(&c.entries));
    }

    /// `ETC_PREFIXES` is narrower than `etc/ritornello/` at large precisely so
    /// that the operator's own files cannot be replaced by a release. Both
    /// names are files the operator writes and the core must never overwrite;
    /// they must be refused for the UI AND absent from what `read` collects
    /// to write, or a hostile release could ship one under its own name and
    /// replace the operator's stations or plugin declarations.
    #[test]
    fn operator_owned_etc_files_are_neither_installable_nor_collected() {
        for name in ["etc/ritornello/plugins.toml", "etc/ritornello/stations.toml"] {
            let gz = targz(&[
                ("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
                (name, b"# operator file\n"),
            ]);
            let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
            assert!(!installable_from_ui(&c.entries), "{name}: {:?}", c.entries);
            assert!(c.etc_files.is_empty(), "{name}: {:?}", c.etc_files);
        }
    }

    /// The regression guard for the narrowing above: a locale catalog and an
    /// input preset — the two subdirectories `ETC_PREFIXES` actually names —
    /// must stay installable and collected.
    #[test]
    fn locales_and_input_presets_files_remain_installable_and_collected() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-generic-input", b"ELF"),
            ("./etc/ritornello/locales/radio/en.json", b"{}"),
            ("./etc/ritornello/input-presets/default.toml", b"# preset\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(installable_from_ui(&c.entries), "{:?}", c.entries);
        assert_eq!(c.etc_files.len(), 2, "{:?}", c.etc_files);
        assert!(c.etc_files.iter().any(|(p, _)| p == "etc/ritornello/locales/radio/en.json"));
        assert!(c.etc_files.iter().any(|(p, _)| p == "etc/ritornello/input-presets/default.toml"));
    }

    /// The whitelist's positive members, asserted rather than assumed: an
    /// archive carrying an example, an initial configuration and its fragment
    /// alongside its binary is installable. Dropping any one of those
    /// prefixes from `allowed` would otherwise refuse a legitimate archive
    /// with no test to notice.
    #[test]
    fn examples_an_initial_configuration_and_the_fragment_are_all_allowed() {
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
            ("./etc/ritornello/locales/radio/en.json", b"{}"),
            ("./examples/stations.example.toml", b"# stations\n"),
            ("./initial-config/stations.toml", b"# stations\n"),
            ("./plugins.toml.fragment", b"[[plugin]]\nname = \"radio\"\n"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(installable_from_ui(&c.entries), "{:?}", c.entries);
    }

    #[test]
    fn an_archive_with_two_plugin_binaries_is_not_installable_from_the_ui() {
        // Not a shape we ship — the bundle carries all of them — but the rule
        // must be "exactly one binary to place", or the core would silently
        // pick the first.
        let gz = targz(&[
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
            ("./usr/local/lib/ritornello/plugins/ritornello-plugin-cd", b"ELF"),
        ]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(!installable_from_ui(&c.entries));
    }

    /// `usr/local/lib/ritornello/plugins/sub/evil` is not the flat layout
    /// `target.rs` on the privileged side ever forms a path from — it only
    /// ever validates one bare name directly under `plugins/`. Refused rather
    /// than silently accepted with `sub/evil` as its recorded name.
    #[test]
    fn a_nested_plugin_binary_is_not_installable() {
        let gz = targz(&[("./usr/local/lib/ritornello/plugins/sub/evil", b"ELF")]);
        let c = read(&gz, DECOMPRESSED_MAX).expect("reads");
        assert!(!installable_from_ui(&c.entries), "{:?}", c.entries);
        assert!(c.binary.is_none(), "{:?}", c.binary);
    }

    /// `plugins//x` strips to `/x` under `PLUGINS_PREFIX` — the same "a slash
    /// remains" shape as a nested binary, reached a different way. Built from
    /// a raw header because `Path::components()` collapses the doubled
    /// separator at write time, so no `tar::Builder` call can express it.
    #[test]
    fn a_double_slash_after_the_plugins_prefix_is_not_installable() {
        let mut builder = tar::Builder::new(Vec::new());
        raw_entry(
            &mut builder,
            b"usr/local/lib/ritornello/plugins//x",
            tar::EntryType::Regular,
            b"ELF",
        );
        let tar = builder.into_inner().unwrap();
        let c = read(&gzip(tar), DECOMPRESSED_MAX).expect("reads");
        assert!(!installable_from_ui(&c.entries), "{:?}", c.entries);
    }

    /// A *parent* directory, not the plugins directory itself: `./usr/local/lib/ritornello/plugins/`
    /// strips to exactly `PLUGINS_PREFIX`, which the `strip_prefix` branch
    /// absorbs on its own (empty remainder, no binary counted) whether or not
    /// the directory guard runs first — a test built on that path would prove
    /// nothing. `tar` writes a parent directory entry for everything it packs
    /// (here, `usr/local/lib/ritornello/`), and that parent is on no
    /// whitelist, so it is the directory guard alone that keeps it from
    /// refusing an otherwise ordinary archive.
    #[test]
    fn a_directory_entry_does_not_make_an_archive_uninstallable() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut dir = tar::Header::new_gnu();
        dir.set_size(0);
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_mode(0o755);
        dir.set_cksum();
        builder.append_data(&mut dir, "./usr/local/lib/ritornello/", &b""[..]).unwrap();
        let mut f = tar::Header::new_gnu();
        f.set_size(3);
        f.set_mode(0o755);
        f.set_cksum();
        builder.append_data(&mut f, "./usr/local/lib/ritornello/plugins/ritornello-plugin-cd", &b"ELF"[..]).unwrap();
        let tar = builder.into_inner().unwrap();
        let c = read(&gzip(tar), DECOMPRESSED_MAX).expect("reads");
        assert!(installable_from_ui(&c.entries), "{:?}", c.entries);
    }

    /// Only a trailing slash used to distinguish a directory; a directory
    /// whose header says `Directory` but whose name carries none must still
    /// be treated as one, from the header's own type rather than the name's
    /// shape.
    #[test]
    fn a_directory_typed_entry_without_a_trailing_slash_is_still_a_directory() {
        let mut builder = tar::Builder::new(Vec::new());
        raw_entry(&mut builder, b"usr/local/lib/ritornello", tar::EntryType::Directory, b"");
        raw_entry(
            &mut builder,
            b"usr/local/lib/ritornello/plugins/ritornello-plugin-cd",
            tar::EntryType::Regular,
            b"ELF",
        );
        let tar = builder.into_inner().unwrap();
        let c = read(&gzip(tar), DECOMPRESSED_MAX).expect("reads");
        assert!(c.entries.iter().any(|e| e == "usr/local/lib/ritornello/"), "{:?}", c.entries);
        assert!(installable_from_ui(&c.entries), "{:?}", c.entries);
    }

    /// A symlink under `plugins/` used to count as the one binary, with empty
    /// content (a symlink's target lives in the header, not the data blocks).
    /// Our own archives never contain one; a hostile shape earns a named
    /// refusal from `read` itself, before `installable_from_ui` ever runs.
    #[test]
    fn a_symlink_under_plugins_is_unsafe() {
        let mut builder = tar::Builder::new(Vec::new());
        raw_entry(
            &mut builder,
            b"usr/local/lib/ritornello/plugins/ritornello-plugin-radio",
            tar::EntryType::Symlink,
            b"",
        );
        let tar = builder.into_inner().unwrap();
        let err = read(&gzip(tar), DECOMPRESSED_MAX).expect_err("a symlink is refused");
        assert!(matches!(err, ArchiveError::UnsafeEntry(_)), "{err:?}");
    }

    /// The trap the download cap does not cover: it bounds the COMPRESSED
    /// size, and gzip expands. A few kilobytes can become gigabytes, on a
    /// device with 1 GiB of RAM.
    #[test]
    fn decompression_stops_at_the_cap_instead_of_filling_memory() {
        let gz = targz(&[("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", &vec![0u8; 200_000])]);
        // Well under what the archive decompresses to.
        let err = read(&gz, 1_000).expect_err("the cap is enforced");
        assert!(matches!(err, ArchiveError::TooLarge(1_000)), "{err:?}");
    }

    /// The reviewer found this unpinned: the `+ 1` in the per-entry `take`
    /// exists specifically so that a payload sitting exactly at the cap is
    /// accepted, not refused.
    #[test]
    fn exactly_at_the_cap_is_accepted() {
        let cap = 1_000;
        let gz = targz(&[("./usr/local/lib/ritornello/plugins/ritornello-plugin-radio", &vec![7u8; cap])]);
        let c = read(&gz, cap).expect("exactly at the cap is accepted");
        assert_eq!(c.binary.as_ref().map(|(_, b)| b.len()), Some(cap));
    }

    /// GNU long-name and PAX members are consumed by the tar crate BEFORE an
    /// entry is ever yielded, so a per-entry cap cannot see them — only a
    /// stream-level bound can. Declared name length is kept a few megabytes
    /// over the `Bounded` budget (`cap` plus the fixed framing slack) rather
    /// than the 100 MB the original probe used, so the test stays fast; the
    /// actual bytes are highly repetitive and compress to almost nothing.
    ///
    /// The point of this test is what happens if `Bounded` is replaced by a
    /// plain `Read::take`: the budget still exists, but `take` reports a
    /// silent EOF instead of an error, which the tar reader then surfaces as
    /// a truncated archive — `Unreadable` where the honest answer is
    /// `TooLarge`. Mutation 2 below exercises exactly that.
    #[test]
    fn a_gnu_long_name_declaring_a_huge_size_is_refused() {
        let cap = 1_000;
        let huge_name = format!(
            "usr/local/lib/ritornello/plugins/{}",
            "a".repeat(FRAMING_SLACK + 2_000_000)
        );
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o755);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append_data(&mut header, &huge_name, &b"ELF"[..]).expect("append");
        let tar = builder.into_inner().expect("finish");

        let err = read(&gzip(tar), cap).expect_err("the cap is enforced");
        assert!(matches!(err, ArchiveError::TooLarge(1_000)), "{err:?}");
    }

    /// The other half of the same hole (Important 1 in the review): a
    /// hostile archive does not need one huge entry, three hundred thousand
    /// tiny ones cost real memory too, and nothing charges their headers
    /// against the content cap. A stream-level bound is what catches this;
    /// the per-entry cap counts only what a "wanted" entry's content adds to
    /// `total`, which stays at zero here.
    ///
    /// This is also the one test that pins `Bounded` counting DEcompressed
    /// bytes rather than compressed ones: 20,000 headers compress to a few
    /// tens of kilobytes, comfortably under any download cap, so wiring
    /// `Bounded` on the compressed side instead would let this whole archive
    /// through. No other test in this file fails if the wrap direction is
    /// swapped.
    #[test]
    fn many_small_entries_past_the_budget_are_refused() {
        let cap = 1_000;
        let names: Vec<String> = (0..20_000u32).map(|i| format!("f{i}")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
        let gz = targz(&entries);
        let err = read(&gz, cap).expect_err("the cap is enforced");
        assert!(matches!(err, ArchiveError::TooLarge(1_000)), "{err:?}");
    }

    /// Pins the `unreadable(...)` call that guards the CONTENT `read_to_end`
    /// specifically — as opposed to the one guarding `entries.next()`, already
    /// pinned by the two tests above. `FRAMING_SLACK` (8 MiB) is exactly
    /// 16384 header blocks of 512 bytes: 16383 zero-size junk entries plus
    /// this final wanted one's own header consume exactly `FRAMING_SLACK`
    /// bytes on the nose, leaving exactly `cap` bytes of `Bounded` budget
    /// remaining at the moment this entry's content starts being read — so
    /// the trip happens while `read_to_end` is running, not while advancing
    /// to reach this entry. Making this specific `map_err` ignore the
    /// tripped flag reddens no other test in this file.
    #[test]
    fn the_budget_can_be_spent_while_reading_a_wanted_entrys_content() {
        let cap = 100usize;
        let junk_count = (FRAMING_SLACK / 512) - 1;
        let names: Vec<String> = (0..junk_count as u32).map(|i| format!("junk{i}")).collect();
        let mut entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
        let content = vec![7u8; 1_000];
        entries.push(("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", &content[..]));
        let gz = targz(&entries);
        let err = read(&gz, cap).expect_err("the cap is enforced while reading content");
        assert!(matches!(err, ArchiveError::TooLarge(100)), "{err:?}");
    }

    /// The reviewer's minor finding: a legitimately-yielded name that is
    /// merely long, not budget-busting, used to be copied into a `String`
    /// before its length was ever checked. This name sits comfortably under
    /// the `Bounded` budget (a real 96 MiB cap is plenty), so it must be
    /// `read`'s own `NAME_MAX` check — not the budget — that refuses it, and
    /// with its own error variant: reporting a five-kilobyte name as
    /// "decompresses to more than 96 MiB" would send the reader looking for
    /// the wrong thing.
    #[test]
    fn a_name_longer_than_name_max_is_refused_by_its_own_variant() {
        let long_name = format!("usr/local/lib/ritornello/plugins/{}", "a".repeat(5_000));
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o755);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append_data(&mut header, &long_name, &b"ELF"[..]).expect("append");
        let tar = builder.into_inner().expect("finish");

        let err = read(&gzip(tar), DECOMPRESSED_MAX).expect_err("the name is too long");
        assert!(matches!(err, ArchiveError::NameTooLong(_)), "{err:?}");
    }

    /// The narrow window where the two name-length guards genuinely differ,
    /// and the reason the byte-length pre-check is not the tidy duplicate it
    /// looks like: it measures the name as it arrived, while the check after
    /// the `./` strip measures a name up to two bytes shorter. A name of
    /// exactly `NAME_MAX + 2` bytes carrying that prefix is refused by the
    /// first and would sail past the second — with no error at all, not
    /// merely a later one.
    ///
    /// Built from raw header bytes, GNU long-name entry included by hand:
    /// `tar::Builder`'s path API drops a leading `./` component at write
    /// time even for a name long enough to need the GNU extension (tar-0.4.46
    /// `header.rs:1601`), so no ordinary `append_data` call can produce this
    /// on-the-wire shape — the same reason the escaping-entry and
    /// absolute-path fixtures need raw headers too.
    #[test]
    fn a_name_two_bytes_over_the_limit_behind_a_dot_slash_is_refused() {
        // "./" (2 bytes) + NAME_MAX 'a's = NAME_MAX + 2: the true on-the-wire
        // name before the `./` strip that the second check applies.
        let long_name = format!("./{}", "a".repeat(NAME_MAX));
        assert_eq!(long_name.len(), NAME_MAX + 2);
        let mut payload = long_name.into_bytes();
        payload.push(0); // GNU long-name entries are NUL-terminated.

        let mut builder = tar::Builder::new(Vec::new());

        let mut long_name_header = tar::Header::new_gnu();
        long_name_header.set_entry_type(tar::EntryType::GNULongName);
        long_name_header.as_gnu_mut().unwrap().name[..b"././@LongLink".len()]
            .copy_from_slice(b"././@LongLink");
        long_name_header.set_size(payload.len() as u64);
        long_name_header.set_mode(0o644);
        long_name_header.set_cksum();
        builder.append(&long_name_header, &payload[..]).unwrap();

        raw_entry(&mut builder, b"truncated", tar::EntryType::Regular, b"ELF");

        let tar = builder.into_inner().unwrap();
        let err = read(&gzip(tar), DECOMPRESSED_MAX).expect_err("the name is refused");
        assert!(matches!(err, ArchiveError::NameTooLong(_)), "{err:?}");
    }

    /// The reviewer's probe: a `Regular`-typed entry whose name ends in `/`
    /// stayed "installable" and, under an allowed prefix, was collected into
    /// `etc_files` — the whitelist and `installable_from_ui` both treat a
    /// trailing slash as "this is a directory" regardless of what the header
    /// says. Nothing outside the allowed prefixes can be written, so there is
    /// no escalation, but the type-versus-name-shape mismatch is refused
    /// outright rather than silently misread as a harmless directory.
    #[test]
    fn a_regular_file_whose_name_ends_with_a_slash_is_unsafe() {
        let mut builder = tar::Builder::new(Vec::new());
        raw_entry(
            &mut builder,
            b"usr/local/lib/ritornello/ritornello-media-mount/",
            tar::EntryType::Regular,
            b"ELF",
        );
        let tar = builder.into_inner().unwrap();
        let err = read(&gzip(tar), DECOMPRESSED_MAX)
            .expect_err("a regular file cannot masquerade as a directory");
        assert!(matches!(err, ArchiveError::UnsafeEntry(_)), "{err:?}");
    }

    #[test]
    fn a_body_that_is_not_an_archive_is_an_error_and_not_a_panic() {
        assert!(matches!(read(b"<html>404</html>", DECOMPRESSED_MAX), Err(ArchiveError::Unreadable(_))));
        assert!(matches!(read(b"", DECOMPRESSED_MAX), Err(ArchiveError::Unreadable(_))));
    }

    /// A tar can name `../../etc/passwd`. Nothing here extracts to a path
    /// taken from the archive — the binary goes to the privileged side under a
    /// name the core chooses, and an etc file's path is rebuilt from the
    /// prefix — but an entry that escapes is a sign of a hostile archive, and
    /// refusing it outright is cheaper than reasoning about every consumer.
    ///
    /// `tar::Builder::append_data` (used by `targz` above) refuses to write a
    /// `..` component itself — a hardening the crate added on the writing
    /// side — so reaching the malicious byte shape this test targets means
    /// writing the header's raw name field and calling `append`, which skips
    /// that validation. This is a fixture-building detail; `read` still sees
    /// exactly the hostile entry a real hostile archive would carry.
    #[test]
    fn an_entry_escaping_its_prefix_makes_the_whole_archive_unreadable() {
        let mut builder = tar::Builder::new(Vec::new());

        let mut good = tar::Header::new_gnu();
        good.set_size(3);
        good.set_mode(0o755);
        good.set_cksum();
        builder
            .append_data(
                &mut good,
                "usr/local/lib/ritornello/plugins/ritornello-plugin-radio",
                &b"ELF"[..],
            )
            .unwrap();

        let evil_name = b"etc/ritornello/../../root/.ssh/authorized_keys";
        let evil_data = b"ssh-rsa AAAA";
        let mut evil = tar::Header::new_gnu();
        evil.as_gnu_mut().unwrap().name[..evil_name.len()].copy_from_slice(evil_name);
        evil.set_size(evil_data.len() as u64);
        evil.set_mode(0o755);
        evil.set_cksum();
        builder.append(&evil, &evil_data[..]).unwrap();

        let tar = builder.into_inner().unwrap();
        assert!(matches!(
            read(&gzip(tar), DECOMPRESSED_MAX),
            Err(ArchiveError::UnsafeEntry(_))
        ));
    }

    /// `tar::Header::set_path` refuses an absolute path outright ("paths in
    /// archives must be relative"), the same write-side hardening the
    /// escaping-entry fixture works around — so this too needs a raw header.
    /// `safe`'s absolute-path guard was unpinned before this test: dropping
    /// `!name.starts_with('/')` reddened nothing.
    #[test]
    fn an_absolute_path_is_unsafe() {
        let mut builder = tar::Builder::new(Vec::new());
        raw_entry(
            &mut builder,
            b"/etc/ritornello/locales/radio/fr.toml",
            tar::EntryType::Regular,
            b"a = \"b\"\n",
        );
        let tar = builder.into_inner().unwrap();
        let err = read(&gzip(tar), DECOMPRESSED_MAX).expect_err("an absolute path is refused");
        assert!(matches!(err, ArchiveError::UnsafeEntry(_)), "{err:?}");
    }

    /// The real entry list of the core archive `installable_from_ui`'s own
    /// test pins (`the_core_archive_could_never_pass_the_rule_that_governs_a_plugin`
    /// in `update::mod`), so a change to either list is caught by both tests
    /// at once. What must come out: the privileged installer, the three
    /// systemd units and the two polkit rules — the core binary and the two
    /// locale files are excluded because `install_one` does place them.
    #[test]
    fn a_core_archive_names_the_installer_the_units_and_the_rules_as_not_installed() {
        let entries: Vec<String> = [
            "etc/",
            "etc/polkit-1/",
            "etc/polkit-1/rules.d/",
            "etc/polkit-1/rules.d/52-ritornello-update.rules",
            "etc/polkit-1/rules.d/50-ritornello-power.rules",
            "etc/ritornello/",
            "etc/ritornello/locales/",
            "etc/ritornello/locales/common/",
            "etc/ritornello/locales/common/fr.toml",
            "etc/ritornello/locales/core/",
            "etc/ritornello/locales/core/fr.toml",
            "etc/systemd/",
            "etc/systemd/system/",
            "etc/systemd/system/ritornello.service",
            "etc/systemd/system/ritornello-update.service",
            "etc/systemd/system/ritornello-rollback.service",
            "usr/",
            "usr/local/",
            "usr/local/lib/",
            "usr/local/lib/ritornello/",
            "usr/local/lib/ritornello/ritornello-update",
            "usr/local/bin/",
            "usr/local/bin/ritornello-core",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut not_installed = core_not_installed(&entries);
        not_installed.sort();
        let mut expected: Vec<String> = [
            "etc/polkit-1/rules.d/52-ritornello-update.rules",
            "etc/polkit-1/rules.d/50-ritornello-power.rules",
            "etc/systemd/system/ritornello.service",
            "etc/systemd/system/ritornello-update.service",
            "etc/systemd/system/ritornello-rollback.service",
            "usr/local/lib/ritornello/ritornello-update",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        expected.sort();
        assert_eq!(not_installed, expected);
    }

    /// A plugin archive carries nothing outside its own prefix (the fixture
    /// in `installable_from_ui`'s own test), so every entry that is not a
    /// directory comes back — proving the function names what is left
    /// **outside** the placed prefixes rather than an empty set by accident.
    #[test]
    fn a_plugin_archive_has_nothing_a_core_rule_would_exempt() {
        let entries: Vec<String> = [
            "etc/",
            "etc/ritornello/",
            "etc/ritornello/locales/",
            "etc/ritornello/locales/radio/",
            "etc/ritornello/locales/radio/fr.toml",
            "usr/",
            "usr/local/",
            "usr/local/lib/",
            "usr/local/lib/ritornello/",
            "usr/local/lib/ritornello/plugins/",
            "usr/local/lib/ritornello/plugins/ritornello-plugin-radio",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // Everything here is under an installed prefix (`ETC_PREFIXES` or
        // `PLUGINS_PREFIX`) or is a directory, so nothing is left over.
        assert!(core_not_installed(&entries).is_empty());
    }
}
