//! The manifest block a plugin binary carries, read by the core as data.
//!
//! A switched-off plugin is never executed, so it cannot announce its
//! version. Instead the SDK embeds a small block in the binary
//! (`declare_runtime!` expands [`encode`] into a `static`), and the core
//! finds it by scanning the file as bytes ([`scan`]).
//!
//! Format 1, at any offset in the file:
//!
//! | Bytes | Content |
//! |---|---|
//! | 24 | magic, ASCII `RITORNELLO-PLUGIN-BLOCK:` |
//! | 1 | format, `0x01` |
//! | 2 | payload length `L`, little-endian `u16`, `L <= 512` |
//! | `L` | payload: UTF-8 lines `key=value\n` |
//!
//! Payload rules: every non-empty line contains `=`; `version` is required
//! exactly once (1 to 64 bytes of `[0-9A-Za-z.+-]`); `repository` is optional,
//! at most once (1 to 400 bytes, no control character); `kinds` is optional,
//! at most once (1 to 64 bytes: comma-separated non-empty items of `[a-z_]`,
//! the kinds the plugin's `Cargo.toml` declares, so a switched-off plugin's
//! row can name its kind); any other key is ignored, so a later field is a
//! compatible addition — `kinds` was one, and a core that predates it reads
//! such a block as before. Bytes after the payload (the encoder pads with
//! zeros) are ignored.
//!
//! A file with zero or two markers is "unknown", never "the first one wins",
//! so a plugin binary must carry the magic exactly once — in its block — and
//! the core, which links this crate to scan, must carry it nowhere. Two
//! things hold that, and neither alone is enough:
//!
//! - The magic is never a literal in this crate. It is stored masked (each
//!   byte XOR `0x5A`): [`encode`] unmasks it at const-evaluation time, so only
//!   the block holds it, and the scanner unmasks it at run time from the
//!   [`MAGIC_MASKED`] `static` through volatile reads ([`magic`]), which the
//!   optimiser may not replace with the values it knows. Unmasked with plain
//!   reads, an optimised build folded the magic back into a constant: it
//!   then lay in the binary beside the block, kept from forming a second
//!   marker only by how the linker happened to lay out 16-byte constants.
//! - `scripts/package-release.sh` counts the markers in every release binary
//!   it packages — exactly one in each plugin, none in the core — and
//!   refuses to package otherwise, so a compiler, linker or LTO change that
//!   brings the magic back is caught at release time instead of making
//!   every switched-off plugin read "unknown" on a device.

use std::io::{Read, Seek, SeekFrom};

pub const MAGIC_LEN: usize = 24;
const MASK: u8 = 0x5A;
/// `b"RITORNELLO-PLUGIN-BLOCK:"`, each byte XOR [`MASK`]. A `const` for
/// [`encode`], which runs at const-evaluation time.
const MAGIC_MASKED_CONST: [u8; MAGIC_LEN] = [
    0x08, 0x13, 0x0E, 0x15, 0x08, 0x14, 0x1F, 0x16, 0x16, 0x15, 0x77, 0x0A, 0x16, 0x0F, 0x1D, 0x13,
    0x14, 0x77, 0x18, 0x16, 0x15, 0x19, 0x11, 0x60,
];
/// The same masked bytes, as a `static` so that they have one address the
/// run-time unmasking ([`magic`]) reads through, volatilely.
#[doc(hidden)]
pub static MAGIC_MASKED: [u8; MAGIC_LEN] = MAGIC_MASKED_CONST;
pub const FORMAT: u8 = 1;
pub const PAYLOAD_MAX: usize = 512;
pub const BLOCK_LEN: usize = MAGIC_LEN + 3 + PAYLOAD_MAX;

const VERSION_MAX: usize = 64;
const REPOSITORY_MAX: usize = 400;
const KINDS_MAX: usize = 64;

/// What a block says about the plugin that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embedded {
    pub version: String,
    pub repository: Option<String>,
    /// The kinds the plugin's `Cargo.toml` declares, in its order. Empty for
    /// a block without them: a binary built before the field, or a plugin
    /// that declares none.
    pub kinds: Vec<String>,
}

/// The magic, unmasked at run time.
///
/// Each masked byte is read with `read_volatile`, which the optimiser must
/// perform as written: it cannot substitute the value it knows the `static`
/// holds, so it cannot fold this function into the magic as a constant —
/// which is what plain reads produced in release builds (see the module
/// documentation). `black_box` would only be a hint; this is a guarantee.
pub fn magic() -> [u8; MAGIC_LEN] {
    let mut out = [0u8; MAGIC_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        // SAFETY: an aligned, in-bounds read of an initialised `u8` in an
        // immutable `static`; nothing writes to it.
        let masked = unsafe { std::ptr::read_volatile(&MAGIC_MASKED[i]) };
        *byte = masked ^ MASK;
    }
    out
}

const fn version_byte_ok(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'+' || b == b'-'
}

const fn put(out: &mut [u8; BLOCK_LEN], at: &mut usize, bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        out[*at] = bytes[i];
        *at += 1;
        i += 1;
    }
}

const fn kind_byte_ok(b: u8) -> bool {
    b.is_ascii_lowercase() || b == b'_'
}

/// The kinds a plugin's `Cargo.toml` declares, as the block carries them:
/// the `kinds = [...]` line of its `[package.metadata.ritornello]` table
/// (the declaration `plugin_catalogue_declaration.rs` in the core already
/// holds against the announcement), written `source` or `display,input`.
///
/// Read at const-evaluation time from the text `declare_runtime!` includes,
/// so it is a hand-rolled scan rather than a TOML parser. It understands the
/// one shape every plugin of this repository writes — the table header on a
/// line of its own, then `kinds = ["a", "b"]` on one line — and answers an
/// empty slice for anything else (no table, no `kinds`, a list spread over
/// several lines, a byte that is not `[a-z_]`, a quote, a comma or a blank):
/// the block then carries no `kinds`, and the core shows the switched-off
/// plugin's kind as unknown, exactly as before. Never a compile error: a
/// third-party plugin is not required to declare its kinds.
pub const fn manifest_kinds(cargo_toml: &str) -> &[u8] {
    const TABLE: &[u8] = b"[package.metadata.ritornello]";
    let t = cargo_toml.as_bytes();
    let mut in_table = false;
    let mut line_start = 0;
    while line_start < t.len() {
        let mut line_end = line_start;
        while line_end < t.len() && t[line_end] != b'\n' {
            line_end += 1;
        }
        // The line, without its leading blanks and its trailing `\r`/blanks.
        let mut a = line_start;
        while a < line_end && (t[a] == b' ' || t[a] == b'\t') {
            a += 1;
        }
        let mut b = line_end;
        while b > a && (t[b - 1] == b'\r' || t[b - 1] == b' ' || t[b - 1] == b'\t') {
            b -= 1;
        }
        if a < b && t[a] == b'[' {
            in_table = starts_with(t, a, b, TABLE) && b - a == TABLE.len();
        } else if in_table && starts_with(t, a, b, b"kinds") {
            let mut i = a + 5;
            while i < b && (t[i] == b' ' || t[i] == b'\t') {
                i += 1;
            }
            if i >= b || t[i] != b'=' {
                // `kindsfoo = …`: another key.
                line_start = line_end + 1;
                continue;
            }
            i += 1;
            while i < b && (t[i] == b' ' || t[i] == b'\t') {
                i += 1;
            }
            if i >= b || t[i] != b'[' || t[b - 1] != b']' {
                return &[];
            }
            let (_, rest) = t.split_at(i + 1);
            let (inner, _) = rest.split_at(b - 1 - (i + 1));
            let mut j = 0;
            while j < inner.len() {
                let c = inner[j];
                if !(kind_byte_ok(c) || c == b'"' || c == b',' || c == b' ' || c == b'\t') {
                    return &[];
                }
                j += 1;
            }
            return inner;
        }
        line_start = line_end + 1;
    }
    &[]
}

const fn starts_with(t: &[u8], a: usize, b: usize, prefix: &[u8]) -> bool {
    if b - a < prefix.len() {
        return false;
    }
    let mut k = 0;
    while k < prefix.len() {
        if t[a + k] != prefix[k] {
            return false;
        }
        k += 1;
    }
    true
}

/// Builds a block. Usable in a `static` initializer; panics (a compile-time
/// error there) on exactly what [`decode_payload`] refuses.
pub const fn encode(version: &str, repository: Option<&str>) -> [u8; BLOCK_LEN] {
    encode_with_kinds(version, repository, &[])
}

/// [`encode`], plus the plugin's kinds as [`manifest_kinds`] found them (an
/// empty slice: no `kinds` line). Quotes and blanks are dropped, so
/// `"display", "input"` is written `kinds=display,input`; empty items are
/// dropped too.
pub const fn encode_with_kinds(version: &str, repository: Option<&str>, kinds: &[u8]) -> [u8; BLOCK_LEN] {
    let v = version.as_bytes();
    assert!(!v.is_empty(), "CARGO_PKG_VERSION is empty");
    assert!(
        v.len() <= VERSION_MAX,
        "CARGO_PKG_VERSION longer than 64 bytes"
    );
    let mut i = 0;
    while i < v.len() {
        assert!(
            version_byte_ok(v[i]),
            "CARGO_PKG_VERSION has a character outside [0-9A-Za-z.+-]"
        );
        i += 1;
    }
    if let Some(repo) = repository {
        let r = repo.as_bytes();
        assert!(!r.is_empty(), "CARGO_PKG_REPOSITORY is empty");
        assert!(
            r.len() <= REPOSITORY_MAX,
            "CARGO_PKG_REPOSITORY longer than 400 bytes"
        );
        let mut j = 0;
        while j < r.len() {
            assert!(
                r[j] >= 0x20 && r[j] != 0x7f,
                "CARGO_PKG_REPOSITORY has a control character"
            );
            j += 1;
        }
    }

    let mut out = [0u8; BLOCK_LEN];
    let mut at = 0;
    let mut k = 0;
    while k < MAGIC_LEN {
        out[at] = MAGIC_MASKED_CONST[k] ^ MASK;
        at += 1;
        k += 1;
    }
    out[at] = FORMAT;
    at += 1;
    let len_at = at;
    at += 2;
    let start = at;
    put(&mut out, &mut at, b"version=");
    put(&mut out, &mut at, v);
    put(&mut out, &mut at, b"\n");
    if let Some(repo) = repository {
        put(&mut out, &mut at, b"repository=");
        put(&mut out, &mut at, repo.as_bytes());
        put(&mut out, &mut at, b"\n");
    }
    // `kinds=a,b`: the items of the list, quotes and blanks dropped, empty
    // items skipped. Written only when at least one item is there.
    let line_at = at;
    put(&mut out, &mut at, b"kinds=");
    let value_at = at;
    let mut item_open = false;
    let mut k = 0;
    while k < kinds.len() {
        let c = kinds[k];
        if kind_byte_ok(c) {
            if !item_open && at > value_at {
                out[at] = b',';
                at += 1;
            }
            item_open = true;
            out[at] = c;
            at += 1;
        } else if c == b',' {
            item_open = false;
        } else {
            assert!(c == b'"' || c == b' ' || c == b'\t', "a kind has a character outside [a-z_]");
        }
        k += 1;
    }
    if at == value_at {
        // No item: the line is taken back, the bytes stay zero.
        while at > line_at {
            at -= 1;
            out[at] = 0;
        }
    } else {
        assert!(at - value_at <= KINDS_MAX, "kinds longer than 64 bytes");
        put(&mut out, &mut at, b"\n");
    }
    let len = at - start;
    assert!(len <= PAYLOAD_MAX, "payload longer than 512 bytes");
    out[len_at] = (len & 0xff) as u8;
    out[len_at + 1] = (len >> 8) as u8;
    out
}

/// Decodes a payload under the rules in the module documentation.
pub fn decode_payload(payload: &[u8]) -> Option<Embedded> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut version: Option<&str> = None;
    let mut repository: Option<&str> = None;
    let mut kinds: Option<&str> = None;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        match key {
            "version" => {
                if version.is_some()
                    || value.is_empty()
                    || value.len() > VERSION_MAX
                    || !value.bytes().all(version_byte_ok)
                {
                    return None;
                }
                version = Some(value);
            }
            "repository" => {
                if repository.is_some()
                    || value.is_empty()
                    || value.len() > REPOSITORY_MAX
                    || value.bytes().any(|b| b < 0x20 || b == 0x7f)
                {
                    return None;
                }
                repository = Some(value);
            }
            "kinds" => {
                if kinds.is_some()
                    || value.is_empty()
                    || value.len() > KINDS_MAX
                    || value.split(',').any(|k| k.is_empty() || !k.bytes().all(kind_byte_ok))
                {
                    return None;
                }
                kinds = Some(value);
            }
            _ => {}
        }
    }
    Some(Embedded {
        version: version?.to_string(),
        repository: repository.map(str::to_string),
        kinds: kinds.map(|k| k.split(',').map(str::to_string).collect()).unwrap_or_default(),
    })
}

/// Decodes a block starting at the magic.
pub fn decode(block: &[u8]) -> Option<Embedded> {
    let header = block.get(..MAGIC_LEN + 3)?;
    if header[..MAGIC_LEN] != magic() || header[MAGIC_LEN] != FORMAT {
        return None;
    }
    let len = u16::from_le_bytes([header[MAGIC_LEN + 1], header[MAGIC_LEN + 2]]) as usize;
    if len > PAYLOAD_MAX {
        return None;
    }
    decode_payload(block.get(MAGIC_LEN + 3..MAGIC_LEN + 3 + len)?)
}

/// Finds the one block in `reader` (at most `limit` bytes) and decodes it.
/// No marker, two markers, a file past the limit, a malformed block or any
/// I/O error: `None`.
pub fn scan<R: Read + Seek>(reader: R, limit: u64) -> Option<Embedded> {
    scan_chunked(reader, limit, 64 * 1024)
}

fn scan_chunked<R: Read + Seek>(mut reader: R, limit: u64, chunk: usize) -> Option<Embedded> {
    let magic = magic();
    let mut buf: Vec<u8> = Vec::with_capacity(chunk + MAGIC_LEN);
    let mut read_buf = vec![0u8; chunk.max(1)];
    // Absolute offset of buf[0].
    let mut base: u64 = 0;
    let mut total: u64 = 0;
    let mut found: Option<u64> = None;
    loop {
        let n = reader.read(&mut read_buf).ok()?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return None;
        }
        buf.extend_from_slice(&read_buf[..n]);
        // The magic cannot overlap itself, and the carry is shorter than the
        // magic, so every occurrence in `buf` is new.
        for (i, w) in buf.windows(MAGIC_LEN).enumerate() {
            if w == magic {
                if found.is_some() {
                    return None;
                }
                found = Some(base + i as u64);
            }
        }
        let keep = (MAGIC_LEN - 1).min(buf.len());
        let drop = buf.len() - keep;
        buf.drain(..drop);
        base += drop as u64;
    }
    let at = found?;
    reader.seek(SeekFrom::Start(at + MAGIC_LEN as u64)).ok()?;
    let mut header = [0u8; 3];
    reader.read_exact(&mut header).ok()?;
    if header[0] != FORMAT {
        return None;
    }
    let len = u16::from_le_bytes([header[1], header[2]]) as usize;
    if len > PAYLOAD_MAX {
        return None;
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).ok()?;
    decode_payload(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const LITERAL: &[u8] = b"RITORNELLO-PLUGIN-BLOCK:";

    fn file_with(blocks: &[&[u8]], junk: usize) -> Vec<u8> {
        let mut v = vec![0xAB; junk];
        for b in blocks {
            v.extend_from_slice(b);
        }
        v.extend(std::iter::repeat_n(0xAB, junk));
        v
    }

    fn version_of(e: Option<Embedded>) -> Option<String> {
        e.map(|e| e.version)
    }

    #[test]
    fn encode_then_decode_gives_back_what_was_given() {
        let block = encode("9.9.9", Some("https://github.com/someone/their-plugin"));
        assert_eq!(
            decode(&block),
            Some(Embedded {
                version: "9.9.9".into(),
                repository: Some("https://github.com/someone/their-plugin".into()),
                kinds: vec![],
            })
        );
        assert_eq!(
            decode(&encode("1.0.0", None)),
            Some(Embedded {
                version: "1.0.0".into(),
                repository: None,
                kinds: vec![]
            })
        );
    }

    #[test]
    fn the_encoded_block_starts_with_the_magic_and_contains_it_once() {
        let block = encode("0.2.0", Some("https://example.org/x"));
        assert_eq!(&block[..MAGIC_LEN], LITERAL);
        assert_eq!(
            block.windows(MAGIC_LEN).filter(|w| *w == LITERAL).count(),
            1
        );
    }

    #[test]
    fn the_masked_bytes_do_not_contain_the_magic() {
        assert_eq!(MAGIC_MASKED, MAGIC_MASKED_CONST);
        assert!(!MAGIC_MASKED.windows(MAGIC_LEN).any(|w| w == magic()));
        assert_eq!(magic(), *LITERAL);
    }

    #[test]
    fn the_magic_cannot_overlap_itself() {
        let m = magic();
        for k in 1..MAGIC_LEN {
            assert_ne!(m[..k], m[MAGIC_LEN - k..], "prefix of {k} equals a suffix");
        }
    }

    #[test]
    fn a_block_amid_junk_is_found() {
        let file = file_with(&[&encode("0.2.0", None)], 1000);
        assert_eq!(
            version_of(scan_chunked(Cursor::new(file), u64::MAX, 64)),
            Some("0.2.0".into())
        );
    }

    #[test]
    fn a_marker_across_a_chunk_boundary_is_found() {
        for junk in 0..80 {
            let file = file_with(&[&encode("0.2.0", None)], junk);
            assert_eq!(
                version_of(scan_chunked(Cursor::new(file), u64::MAX, 32)),
                Some("0.2.0".into()),
                "junk {junk}"
            );
        }
    }

    #[test]
    fn no_marker_is_unknown() {
        let file = vec![0xAB; 10_000];
        assert_eq!(scan_chunked(Cursor::new(file), u64::MAX, 64), None);
    }

    #[test]
    fn two_markers_are_unknown() {
        let a = encode("0.2.0", None);
        let b = encode("0.3.0", None);
        let same = file_with(&[&a, &a], 100);
        assert_eq!(scan_chunked(Cursor::new(same), u64::MAX, 64), None);
        let different = file_with(&[&a, &b], 100);
        assert_eq!(scan_chunked(Cursor::new(different), u64::MAX, 64), None);
    }

    #[test]
    fn a_truncated_block_is_unknown() {
        let mut file = magic().to_vec();
        file.push(FORMAT);
        file.extend_from_slice(&40u16.to_le_bytes());
        file.extend_from_slice(b"version=10");
        assert_eq!(scan_chunked(Cursor::new(file), u64::MAX, 64), None);
    }

    #[test]
    fn an_oversized_length_is_unknown() {
        let mut file = magic().to_vec();
        file.push(FORMAT);
        file.extend_from_slice(&((PAYLOAD_MAX + 1) as u16).to_le_bytes());
        let mut payload = b"version=1.0.0\n".to_vec();
        payload.resize(600, b'\n');
        file.extend_from_slice(&payload);
        assert_eq!(scan_chunked(Cursor::new(file), u64::MAX, 64), None);
    }

    #[test]
    fn another_format_is_unknown() {
        let mut block = encode("0.2.0", None);
        block[MAGIC_LEN] = 2;
        let file = file_with(&[&block], 10);
        assert_eq!(scan_chunked(Cursor::new(file), u64::MAX, 64), None);
        assert_eq!(decode(&block), None);
    }

    #[test]
    fn a_file_longer_than_the_limit_is_unknown() {
        let block = encode("0.2.0", None);
        let mut file = vec![0xAB; 5000 - block.len()];
        file.extend_from_slice(&block);
        assert_eq!(file.len(), 5000);
        assert_eq!(scan_chunked(Cursor::new(file.clone()), 4096, 64), None);
        assert!(scan_chunked(Cursor::new(file), 5000, 64).is_some());
    }

    #[test]
    fn decode_payload_rules() {
        assert_eq!(decode_payload(b"repository=x\n"), None, "no version");
        assert_eq!(
            decode_payload(b"version=1.0.0\nversion=1.0.1\n"),
            None,
            "twice"
        );
        assert_eq!(
            decode_payload(b"version=1.0.0\r\n"),
            None,
            "carriage return"
        );
        assert_eq!(decode_payload(b"version=\n"), None, "empty version");
        assert_eq!(decode_payload(&[0xff, 0xfe]), None, "not UTF-8");
        assert_eq!(
            decode_payload(b"version=1.0.0\njunk\n"),
            None,
            "line without ="
        );
        assert_eq!(
            decode_payload(b"version=1.0.0\nfuture=whatever\n"),
            Some(Embedded {
                version: "1.0.0".into(),
                repository: None,
                kinds: vec![]
            }),
            "unknown key ignored"
        );
        assert_eq!(
            decode_payload(b"version=1.0.0\nrepository=a\x01b\n"),
            None,
            "control character"
        );
        let long = format!("version={}\n", "1".repeat(65));
        assert_eq!(decode_payload(long.as_bytes()), None, "version too long");
        let kinds = |p: &[u8]| decode_payload(p).map(|e| e.kinds);
        assert_eq!(kinds(b"version=1.0.0\nkinds=source\n"), Some(vec!["source".to_string()]));
        assert_eq!(
            kinds(b"version=1.0.0\nkinds=display,input\n"),
            Some(vec!["display".to_string(), "input".to_string()])
        );
        assert_eq!(kinds(b"version=1.0.0\nkinds=\n"), None, "empty kinds");
        assert_eq!(kinds(b"version=1.0.0\nkinds=source,\n"), None, "empty item");
        assert_eq!(kinds(b"version=1.0.0\nkinds=Source\n"), None, "outside [a-z_]");
        assert_eq!(kinds(b"version=1.0.0\nkinds=a\nkinds=b\n"), None, "kinds twice");
        let long = format!("version=1.0.0\nkinds={}\n", "a".repeat(65));
        assert_eq!(kinds(long.as_bytes()), None, "kinds too long");
    }

    /// The `[package.metadata.ritornello]` table of the CD plugin, as its
    /// `Cargo.toml` writes it, among the tables around it.
    const CD_MANIFEST: &str = "[package]\nname = \"ritornello-plugin-cd\"\n\n\
        [dependencies]\nkinds = [\"not\", \"this\"]\n\n\
        [package.metadata.ritornello]\nkinds = [\"source\"]\ndescription = \"Plays audio CDs.\"\n\
        admin = true\n\n[[bin]]\nname = \"ritornello-plugin-cd\"\n";

    #[test]
    fn the_kinds_are_read_from_the_metadata_table_only() {
        assert_eq!(manifest_kinds(CD_MANIFEST), b"\"source\"");
        assert_eq!(manifest_kinds(&CD_MANIFEST.replace('\n', "\r\n")), b"\"source\"", "CRLF");
        let two = "[package.metadata.ritornello]\nkinds = [\"display\", \"input\"]\n";
        assert_eq!(manifest_kinds(two), b"\"display\", \"input\"");
        // A `kinds` outside the table, a table without one, none at all.
        assert_eq!(manifest_kinds("[dependencies]\nkinds = [\"source\"]\n"), b"");
        assert_eq!(manifest_kinds("[package.metadata.ritornello]\ndescription = \"x\"\n"), b"");
        assert_eq!(manifest_kinds(""), b"");
        // A longer header is another table.
        assert_eq!(manifest_kinds("[package.metadata.ritornello.extra]\nkinds = [\"source\"]\n"), b"");
        // A shape the scan does not understand is no kinds, never an error.
        assert_eq!(manifest_kinds("[package.metadata.ritornello]\nkinds = [\n  \"source\",\n]\n"), b"");
        assert_eq!(manifest_kinds("[package.metadata.ritornello]\nkinds = [\"Source\"]\n"), b"");
        assert_eq!(manifest_kinds("[package.metadata.ritornello]\nkindsx = [\"source\"]\n"), b"");
    }

    #[test]
    fn a_block_with_kinds_gives_them_back() {
        let block = encode_with_kinds("1.0.0", None, manifest_kinds(CD_MANIFEST));
        assert_eq!(decode(&block).unwrap().kinds, vec!["source".to_string()]);
        let block = encode_with_kinds("1.0.0", Some("https://example.org/x"), b"\"display\", \"input\"");
        let e = decode(&block).unwrap();
        assert_eq!(e.kinds, vec!["display".to_string(), "input".to_string()]);
        assert_eq!(e.repository.as_deref(), Some("https://example.org/x"));
        // No kinds: no line at all, byte for byte the block `encode` makes.
        assert_eq!(encode_with_kinds("1.0.0", None, b""), encode("1.0.0", None));
        assert_eq!(encode_with_kinds("1.0.0", None, b"\"\""), encode("1.0.0", None));
    }

    /// What `declare_runtime!` embeds in each plugin of this workspace: the
    /// kinds its own `Cargo.toml` declares, read the way the macro reads them.
    /// Every shipped plugin declares at least one, so a scan that silently
    /// found none would leave every switched-off row without its kind.
    #[test]
    fn every_plugin_of_the_workspace_embeds_its_declared_kinds() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut seen = 0;
        for entry in std::fs::read_dir(&crates).unwrap() {
            let dir = entry.unwrap().path();
            let name = dir.file_name().unwrap().to_string_lossy().to_string();
            if !name.starts_with("ritornello-plugin-") || name == "ritornello-plugin-sdk" {
                continue;
            }
            let text = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
            let declared: toml::Value = toml::from_str(&text).unwrap();
            let want: Vec<String> = declared["package"]["metadata"]["ritornello"]["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| k.as_str().unwrap().to_string())
                .collect();
            assert!(!want.is_empty(), "{name} declares no kind");
            let block = encode_with_kinds("1.0.0", None, manifest_kinds(&text));
            assert_eq!(decode(&block).unwrap().kinds, want, "{name}");
            seen += 1;
        }
        assert!(seen >= 12, "only {seen} plugins found under {}", crates.display());
    }

    #[test]
    fn the_encoder_refuses_what_the_decoder_refuses() {
        assert!(std::panic::catch_unwind(|| encode("", None)).is_err());
        assert!(std::panic::catch_unwind(|| encode("1.0\n0", None)).is_err());
        assert!(std::panic::catch_unwind(|| encode("1.0.0", Some("a\nb"))).is_err());
    }
}
