//! What an archive contains, and what may be done with it.
//!
//! **This is where the trust in a downloaded archive stops.** The core reads
//! it entry by entry and takes exactly what it recognises: one binary, the
//! files under `etc/ritornello`, an initial configuration, a `plugins.toml`
//! block. Everything else — a systemd unit, a polkit rule, a root-run helper —
//! is listed so the page can say the release changes it, and **never written**.
//! `tar -x` would take it all and then need undoing.
//!
//! Two caps, and they are not the same cap. The download bounds the
//! **compressed** size; this module bounds the **decompressed** one, because
//! gzip expands and this runs on a device with a gigabyte of memory. That cap
//! is enforced per tar entry rather than on the whole stream: a single
//! oversized entry must be refused on its own, and a stream-level `take`
//! truncates the tar reader before the byte counter ever gets a chance to
//! exceed the cap — it turns `TooLarge` into a spurious `Unreadable`.

use std::io::Read;

/// Where a plugin binary lives inside an archive.
pub const PLUGINS_PREFIX: &str = "usr/local/lib/ritornello/plugins/";
/// Configuration the service itself may write.
pub const ETC_PREFIX: &str = "etc/ritornello/";
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

#[derive(Debug)]
pub enum ArchiveError {
    /// Not a gzipped tar at all: an HTML error page, a truncated download.
    Unreadable(String),
    /// More decompressed bytes than the cap allows.
    TooLarge(usize),
    /// An entry whose name escapes its own prefix, or is absolute.
    UnsafeEntry(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(d) => write!(f, "the archive could not be read: {d}"),
            Self::TooLarge(cap) => write!(f, "the archive decompresses to more than {cap} bytes"),
            Self::UnsafeEntry(name) => write!(f, "the archive holds an unsafe entry name: {name:?}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

#[derive(Debug, Clone, Default)]
pub struct Contents {
    /// Every entry's path, `./` stripped, directories included. This is what
    /// `installable_from_ui` reads, and what lets the page say a release
    /// changes a unit.
    pub entries: Vec<String>,
    /// `(file name, bytes)` of the single plugin binary, when there is one.
    pub binary: Option<(String, Vec<u8>)>,
    pub core_binary: Option<Vec<u8>>,
    /// Paths under `etc/ritornello/`, with their bytes. Written
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
/// keep in step with itself. Today exactly one archive fails it — the files
/// plugin, which ships a root-run mount helper, a unit and a polkit rule — and
/// that is a fact about the archive rather than a name in a table.
pub fn installable_from_ui(entries: &[String]) -> bool {
    let mut binaries = 0;
    for entry in entries {
        // A directory entry describes no content: `tar` writes them for the
        // parents of what it packs, and judging an archive on them would make
        // every archive uninstallable.
        if entry.ends_with('/') {
            continue;
        }
        if let Some(rest) = entry.strip_prefix(PLUGINS_PREFIX) {
            if !rest.is_empty() {
                binaries += 1;
            }
            continue;
        }
        let allowed = entry.starts_with(ETC_PREFIX)
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

/// Rejects an absolute path or any component that could climb out.
///
/// Nothing downstream extracts to a path taken from the archive, so this is
/// belt and braces — but an entry that escapes is a sign of a hostile archive,
/// and one refusal here is cheaper than reasoning about every consumer.
fn safe(name: &str) -> bool {
    !name.starts_with('/')
        && !name.split('/').any(|c| c == ".." )
}

pub fn read(gz: &[u8], cap: usize) -> Result<Contents, ArchiveError> {
    let decoder = flate2::read::GzDecoder::new(gz);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;

    let mut out = Contents::default();
    let mut total = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| ArchiveError::Unreadable(e.to_string()))?;
        let raw = entry
            .path()
            .map_err(|e| ArchiveError::Unreadable(e.to_string()))?
            .to_string_lossy()
            .to_string();
        // `tar -C <dir> .` writes every path as `./x`. Stripping here rather
        // than at every comparison below.
        let path = raw.strip_prefix("./").unwrap_or(&raw).to_string();
        if !safe(&path) {
            return Err(ArchiveError::UnsafeEntry(path));
        }
        out.entries.push(path.clone());
        if path.ends_with('/') {
            continue;
        }

        let want = path == CORE_BINARY
            || path.starts_with(PLUGINS_PREFIX)
            || path.starts_with(ETC_PREFIX)
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
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| ArchiveError::Unreadable(e.to_string()))?;
        total += bytes.len();
        if total > cap {
            return Err(ArchiveError::TooLarge(cap));
        }

        if path == CORE_BINARY {
            out.core_binary = Some(bytes);
        } else if let Some(name) = path.strip_prefix(PLUGINS_PREFIX) {
            // A second binary replaces nothing: `installable_from_ui` has
            // already refused such an archive, and the bundle is never
            // installed component by component.
            if out.binary.is_none() {
                out.binary = Some((name.to_string(), bytes));
            }
        } else if path.starts_with(ETC_PREFIX) {
            out.etc_files.push((path, bytes));
        } else if let Some(name) = path.strip_prefix(INITIAL_CONFIG_PREFIX) {
            out.initial_config.push((name.to_string(), bytes));
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

    /// The exact shape `package-release.sh` produces for a plugin with a
    /// locale catalog and an example.
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

    #[test]
    fn a_directory_entry_does_not_make_an_archive_uninstallable() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut dir = tar::Header::new_gnu();
        dir.set_size(0);
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_mode(0o755);
        dir.set_cksum();
        builder.append_data(&mut dir, "./usr/local/lib/ritornello/plugins/", &b""[..]).unwrap();
        let mut f = tar::Header::new_gnu();
        f.set_size(3);
        f.set_mode(0o755);
        f.set_cksum();
        builder.append_data(&mut f, "./usr/local/lib/ritornello/plugins/ritornello-plugin-cd", &b"ELF"[..]).unwrap();
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        let c = read(&gz.finish().unwrap(), DECOMPRESSED_MAX).expect("reads");
        assert!(installable_from_ui(&c.entries), "{:?}", c.entries);
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
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        assert!(matches!(
            read(&gz.finish().unwrap(), DECOMPRESSED_MAX),
            Err(ArchiveError::UnsafeEntry(_))
        ));
    }
}
