//! The roots: the directories the plugin is allowed to look into.
//!
//! A USB disk, a folder on the device and an SMB share are the same thing for
//! the rest of the plugin; the mount is merely a detail of the `Smb` kind.
//! That is what makes browsing local files nearly free.
//!
//! This module's validation is **read by a root binary**. It is therefore
//! strict, and refuses on principle anything it cannot prove harmless.

use ritornello_i18n::Catalog;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Root of the mount points. Constant, **never read from the configuration**:
/// a free mount point would be a path to validate, and root is who would use
/// it.
pub const MOUNT_ROOT: &str = "/mnt/ritornello";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Root {
    pub name: String,
    pub kind: RootKind,
    /// `Local` kind only: absolute path of the directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub share: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub domain: String,
    /// Removes `ro` from the mount options. False by default: saving a
    /// playlist onto the share is an explicit choice, not a given.
    #[serde(default)]
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    Local,
    Smb,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Roots {
    #[serde(default)]
    pub root: Vec<Root>,
}

/// Typed validation error: the user-facing text is produced at the boundary
/// via `message(&Catalog)`. `Display` provides an English version for internal
/// logs, outside the i18n scope.
#[derive(Debug, Clone, PartialEq)]
pub enum RootError {
    BadName { name: String },
    BadHost { host: String },
    BadShare { share: String },
    BadSubpath { subpath: String },
    DuplicateName { name: String },
    RelativeLocalPath { path: String },
}

/// Grammar of a root name: it becomes a **path component** and a
/// **credentials file name**. Anything outside this alphabet would open a
/// directory traversal on the privileged side.
fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A field that lands in a `mount.cifs` options line.
///
/// **The comma is the injection to fear**: `mount.cifs` options are separated
/// by commas, so a host "nas,uid=0" would add an option to the line executed
/// by root. A space breaks the parsing, `..` climbs the tree, and the null
/// byte truncates a C string.
pub fn field_on(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(',')
        && !value.chars().any(char::is_whitespace)
        && !value.contains("..")
        && !value.contains('\0')
}

/// Grammar of a **subpath** browsed under a mount point.
///
/// Distinct from `field_on`, and deliberately so. `field_on` refuses the comma
/// and the space because its values land in the `mount.cifs` options line,
/// which separates them with commas. **A subpath never gets there**:
/// `mount_command` only sets the host, the share and `mount_point()`, which
/// ignores the subpath.
///
/// Applying the same rule to both made "Ma Musique" undeclarable for a reason
/// that has nothing to do with it. The defect was rarely seen as long as the
/// subpath was typed by hand; it would become constant with a wizard that
/// offers to pick any folder of a NAS.
fn subpath_on(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('/')
        && !s.contains('\0')
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

impl Roots {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let roots: Roots = toml::from_str(&text)?;
        roots.validate()?;
        Ok(roots)
    }

    pub fn validate(&self) -> Result<(), RootError> {
        let mut seen: Vec<&str> = Vec::new();
        for r in &self.root {
            if !valid_name(&r.name) {
                return Err(RootError::BadName { name: r.name.clone() });
            }
            if seen.contains(&r.name.as_str()) {
                return Err(RootError::DuplicateName { name: r.name.clone() });
            }
            seen.push(&r.name);
            match r.kind {
                RootKind::Local => {
                    let p = r.path.clone().unwrap_or_default();
                    if !Path::new(&p).is_absolute() {
                        return Err(RootError::RelativeLocalPath { path: p });
                    }
                }
                RootKind::Smb => {
                    if !field_on(&r.host) {
                        return Err(RootError::BadHost { host: r.host.clone() });
                    }
                    if !field_on(&r.share) {
                        return Err(RootError::BadShare { share: r.share.clone() });
                    }
                    if let Some(s) = &r.subpath
                        && !subpath_on(s)
                    {
                        return Err(RootError::BadSubpath { subpath: s.clone() });
                    }
                }
            }
        }
        Ok(())
    }

    pub fn by_name(&self, name: &str) -> Option<&Root> {
        self.root.iter().find(|r| r.name == name)
    }
}

impl Root {
    /// Directory actually browsed. For a share, the **imposed** mount point,
    /// possibly followed by the declared subpath.
    pub fn base_dir(&self) -> PathBuf {
        match self.kind {
            RootKind::Local => PathBuf::from(self.path.clone().unwrap_or_default()),
            RootKind::Smb => {
                let mut p = PathBuf::from(MOUNT_ROOT).join(&self.name);
                if let Some(s) = &self.subpath {
                    p = p.join(s);
                }
                p
            }
        }
    }

    /// Mount point, **without** the subpath: the whole share is what gets
    /// mounted, the subpath being merely a place to look inside it.
    pub fn mount_point(&self) -> PathBuf {
        PathBuf::from(MOUNT_ROOT).join(&self.name)
    }

    /// Credentials file consumed by `mount.cifs`.
    pub fn credentials_path(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.cred", self.name))
    }
}

impl RootError {
    /// Localized message surfaced to the user (body of the admin-side refusal).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            RootError::BadName { name } => catalog.get("bad_root_name").replace("{name}", name),
            RootError::BadHost { host } => catalog.get("bad_host").replace("{host}", host),
            RootError::BadShare { share } => catalog.get("bad_share").replace("{share}", share),
            RootError::BadSubpath { subpath } => {
                catalog.get("bad_subpath").replace("{path}", subpath)
            }
            RootError::DuplicateName { name } => {
                catalog.get("duplicate_root").replace("{name}", name)
            }
            RootError::RelativeLocalPath { path } => {
                catalog.get("relative_local_path").replace("{path}", path)
            }
        }
    }
}

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RootError::BadName { name } => write!(f, "invalid root name: {name}"),
            RootError::BadHost { host } => write!(f, "invalid host: {host}"),
            RootError::BadShare { share } => write!(f, "invalid share: {share}"),
            RootError::BadSubpath { subpath } => write!(f, "invalid subpath: {subpath}"),
            RootError::DuplicateName { name } => write!(f, "duplicate root name: {name}"),
            RootError::RelativeLocalPath { path } => write!(f, "local path not absolute: {path}"),
        }
    }
}

impl std::error::Error for RootError {}

/// Folds an arbitrary label into a root name conforming to `valid_name`.
///
/// The user no longer types this name: the wizards derive it from the share
/// name or from the last segment of the chosen path. Since it becomes **a
/// component of the mount path and a credentials file name**, the derivation
/// must produce something valid by construction — a refusal after derivation
/// would be a defect that nothing in the UI would allow to fix.
///
/// `taken` carries the names already in use: without deduplication, a second
/// source would overwrite the first one's credentials file and fight over its
/// mount point.
pub fn derive_name(hint: &str, taken: &[&str]) -> String {
    let base = fold(hint);
    if !taken.contains(&base.as_str()) {
        return base;
    }
    for n in 2..1000 {
        let suffix = format!("-{n}");
        // Truncate **before** concatenating: adding the suffix to an already
        // long name would produce a refused name, hence a source impossible
        // to declare a second time.
        let head: String = base.chars().take(32 - suffix.len()).collect();
        let candidate = format!("{}{suffix}", head.trim_end_matches('-'));
        if !taken.contains(&candidate.as_str()) {
            return candidate;
        }
    }
    base
}

/// The folding itself: ASCII lowercase, a dash for everything else.
///
/// The first character is alphanumeric **by construction** — we never push a
/// dash onto an empty string — which satisfies the first rule of `valid_name`
/// without having to check it afterwards.
fn fold(hint: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in hint.chars() {
        let c = without_accents(c);
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    let truncated: String = out.chars().take(32).collect();
    let clean = truncated.trim_end_matches('-').to_string();
    // A hint that is entirely non-ASCII leaves nothing: better a generic name
    // than a source impossible to declare.
    if clean.is_empty() {
        "source".to_string()
    } else {
        clean
    }
}

/// Folds the common Latin accents.
///
/// A table rather than a Unicode normalization crate: fifteen lines cover
/// French, Spanish and German, and everything else falls onto the dash anyway.
/// "Été" turning into "t" would be an exact name but an unreadable one in the
/// logs and under `/mnt/ritornello`.
fn without_accents(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'A',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
        'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => 'O',
        'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
        'Ç' => 'C',
        'Ñ' => 'N',
        'Ý' => 'Y',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smb_root() -> Root {
        Root {
            name: "nas".into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: "musique".into(),
            subpath: Some("Albums".into()),
            user: "steven".into(),
            domain: String::new(),
            writable: false,
        }
    }

    fn local_root() -> Root {
        Root {
            name: "usb".into(),
            kind: RootKind::Local,
            path: Some("/media/usb".into()),
            host: String::new(),
            share: String::new(),
            subpath: None,
            user: String::new(),
            domain: String::new(),
            writable: false,
        }
    }

    fn roots_with(root: Root) -> Roots {
        Roots { root: vec![root] }
    }

    #[test]
    fn a_root_name_outside_the_grammar_is_refused() {
        // The name becomes a path component (/mnt/ritornello/<name>) AND a
        // credentials file name. Anything that is not [a-z0-9-] would open a
        // directory traversal on the privileged side.
        for bad in ["../evasion", "Nas", "nas/musique", "", "nas musique", "-nas", "nas.."] {
            let r = roots_with(Root { name: bad.into(), ..smb_root() });
            assert!(
                matches!(r.validate(), Err(RootError::BadName { .. })),
                "wrongly accepted: {bad:?}"
            );
        }
    }

    #[test]
    fn a_comma_in_the_host_or_the_share_is_refused() {
        // THE hole not to miss: mount.cifs options are separated by commas. A
        // host "nas,uid=0" would inject an option into the mount line executed
        // by root.
        let r = roots_with(Root { host: "nas,uid=0".into(), ..smb_root() });
        assert!(matches!(r.validate(), Err(RootError::BadHost { .. })));
        let r = roots_with(Root { share: "musique,rw".into(), ..smb_root() });
        assert!(matches!(r.validate(), Err(RootError::BadShare { .. })));
    }

    #[test]
    fn a_subpath_that_climbs_or_is_absolute_is_refused() {
        let r = roots_with(Root { subpath: Some("../../etc".into()), ..smb_root() });
        assert!(matches!(r.validate(), Err(RootError::BadSubpath { .. })));
        let r = roots_with(Root { subpath: Some("/etc".into()), ..smb_root() });
        assert!(matches!(r.validate(), Err(RootError::BadSubpath { .. })));
    }

    #[test]
    fn a_subpath_with_spaces_is_accepted() {
        // The defect that was fixed. `field_on` refuses the space because its
        // values land in the mount.cifs options line, which is comma-separated.
        // A subpath NEVER gets there: `mount_command` only sets the host, the
        // share and `mount_point()`, which ignores it. Applying the same rule
        // to it made "Ma Musique" undeclarable for a reason that has nothing
        // to do with it — and the wizard now offers any folder.
        let r = roots_with(Root { subpath: Some("Ma Musique/Jazz, live".into()), ..smb_root() });
        assert!(r.validate().is_ok(), "{:?}", r.validate());
    }

    #[test]
    fn a_subpath_that_climbs_stays_refused() {
        for bad in ["../../etc", "/etc", "a/../../b", "a//b", "a/./b", "a\0b", ""] {
            let r = roots_with(Root { subpath: Some(bad.into()), ..smb_root() });
            assert!(
                matches!(r.validate(), Err(RootError::BadSubpath { .. })),
                "wrongly accepted: {bad:?}"
            );
        }
    }

    #[test]
    fn a_derived_name_is_always_accepted_by_the_grammar() {
        // The invariant that matters: this name becomes a component of the
        // mount path AND a credentials file name. The derivation must produce
        // something valid by construction, never by luck — the user no longer
        // sees this name and would have no way to fix a refusal.
        let hostile = [
            "../etc", "Ma Musique", "Éric's Jazz!", "///", "", "$$$", "3615",
            "CamelCase", "a b c d e f g h i j k l m n o p q r s t u v w x y z 0 1 2 3",
            "日本語", "-début-tiret-", "fin-tiret---",
        ];
        for h in hostile {
            let n = derive_name(h, &[]);
            assert!(valid_name(&n), "hint {h:?} produced a refused name: {n:?}");
        }
    }

    #[test]
    fn two_identical_hints_yield_two_distinct_names() {
        // Without deduplication, the second source would overwrite the first
        // one's credentials file and fight over its mount point.
        let a = derive_name("Musique", &[]);
        let b = derive_name("Musique", &[a.as_str()]);
        let c = derive_name("Musique", &[a.as_str(), b.as_str()]);
        assert_eq!(a, "musique");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert!(valid_name(&b) && valid_name(&c));
    }

    #[test]
    fn a_very_long_hint_remains_deduplicable() {
        // The suffix must fit within the 32 characters: concatenating it
        // without truncating first would produce a refused name, hence a
        // source impossible to declare a second time.
        let long = "a".repeat(60);
        let a = derive_name(&long, &[]);
        let b = derive_name(&long, &[a.as_str()]);
        assert!(valid_name(&a) && valid_name(&b), "{a:?} / {b:?}");
        assert_ne!(a, b);
    }

    #[test]
    fn accents_fold_instead_of_disappearing() {
        // "Été" turning into "t" would be an exact name but an unreadable one
        // in the logs and in /mnt/ritornello.
        assert_eq!(derive_name("Été à Nîmes", &[]), "ete-a-nimes");
    }

    #[test]
    fn two_roots_with_the_same_name_are_refused() {
        // They would fight over the same mount point and the same credentials
        // file.
        let r = Roots { root: vec![smb_root(), smb_root()] };
        assert!(matches!(r.validate(), Err(RootError::DuplicateName { .. })));
    }

    #[test]
    fn a_local_root_wants_an_absolute_path() {
        let r = roots_with(Root { path: Some("media/usb".into()), ..local_root() });
        assert!(matches!(r.validate(), Err(RootError::RelativeLocalPath { .. })));
        assert!(roots_with(local_root()).validate().is_ok());
    }

    #[test]
    fn a_valid_root_passes_and_its_directories_are_imposed() {
        let r = roots_with(smb_root());
        assert!(r.validate().is_ok());
        // The mount point is NEVER read from the configuration, and the
        // subpath does not enter it: the whole share is what gets mounted.
        assert_eq!(r.root[0].mount_point(), PathBuf::from("/mnt/ritornello/nas"));
        assert_eq!(r.root[0].base_dir(), PathBuf::from("/mnt/ritornello/nas/Albums"));
    }

    #[test]
    fn every_refusal_resolves_against_the_embedded_catalog() {
        // `Catalog::get` returns the key when it cannot find it: without this
        // test, a typo would display "bad_share" on screen without anything
        // complaining. So we resolve against the catalog actually embedded,
        // and refuse a message reduced to its own key.
        let catalog =
            Catalog::load("files", "en", Path::new("/inexistant"), crate::FILES_EN);
        let messages = [
            RootError::BadName { name: "x/y".into() }.message(&catalog),
            RootError::BadHost { host: "a,b".into() }.message(&catalog),
            RootError::BadShare { share: "a,b".into() }.message(&catalog),
            RootError::BadSubpath { subpath: "..".into() }.message(&catalog),
            RootError::DuplicateName { name: "nas".into() }.message(&catalog),
            RootError::RelativeLocalPath { path: "media/usb".into() }.message(&catalog),
        ];
        for m in &messages {
            assert!(m.contains(' '), "message reduced to a raw key: {m:?}");
        }
        // And the interpolation goes through: no placeholder left as is.
        let host_message = RootError::BadHost { host: "nas,uid=0".into() }.message(&catalog);
        assert!(host_message.contains("nas,uid=0"), "the refusal must name what is wrong: {host_message:?}");
        assert!(!host_message.contains("{host}"), "placeholder left as is: {host_message:?}");
    }

    #[test]
    fn a_table_reads_back_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("media-roots.toml");
        std::fs::write(
            &f,
            r#"
[[root]]
name = "nas"
kind = "smb"
host = "192.168.1.20"
share = "musique"
subpath = "Albums"
user = "steven"

[[root]]
name = "usb"
kind = "local"
path = "/media/usb"
"#,
        )
        .unwrap();
        let roots = Roots::load(&f).unwrap();
        assert_eq!(roots.root.len(), 2);
        assert_eq!(roots.by_name("nas").unwrap().kind, RootKind::Smb);
        assert_eq!(roots.by_name("usb").unwrap().base_dir(), PathBuf::from("/media/usb"));
        // The `writable` default matters: a share is not writable unless asked
        // for.
        assert!(!roots.by_name("nas").unwrap().writable);
    }
}
