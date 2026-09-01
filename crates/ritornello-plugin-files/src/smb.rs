//! Talking to `smbclient`: enumerating a host's shares and listing a folder,
//! **without mounting anything**.
//!
//! This is what makes browsing *before* declaring possible. Mounting
//! temporarily just to preview would have required a privilege for a mere
//! glance, left orphan mounts behind if the tab closes, and above all could
//! not have enumerated the shares — `mount.cifs` already requires knowing the
//! share name, which is precisely the question we are asking the machine.
//!
//! Parsing the outputs is pure and is tested without a NAS. The formats are
//! those of samba 4.19.5.

use ritornello_i18n::Catalog;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmbError {
    NotInstalled,
    BadCredentials,
    AccessDenied,
    Unreachable,
    /// The targeted share or folder does not exist.
    ///
    /// Distinct from `Unreachable` **because measurement forced it**: the NAS
    /// returns `NT_STATUS_OBJECT_NAME_NOT_FOUND` in this case, and filing them
    /// together would show "the machine did not answer" in front of a stale
    /// path — the user would go check their network instead of their tree.
    NotFound,
    Timeout,
    /// Non-empty output that no rule could read.
    ///
    /// Kept distinct from an empty folder **on purpose**: if a samba version
    /// changes its format, a full folder would show as empty and the user
    /// would conclude that their NAS lost its music. A refusal that names the
    /// problem and hands back the raw output can be diagnosed; an empty folder
    /// cannot.
    UnreadableOutput(String),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SmbEntry {
    pub name: String,
    pub dir: bool,
}

impl SmbError {
    pub fn message(&self, catalog: &Catalog, host: &str) -> String {
        match self {
            SmbError::NotInstalled => catalog.get("smb_not_installed").to_string(),
            SmbError::BadCredentials => catalog.get("smb_bad_credentials").to_string(),
            SmbError::AccessDenied => catalog.get("smb_access_denied").to_string(),
            SmbError::Unreachable => catalog.get("smb_unreachable").replace("{host}", host),
            SmbError::NotFound => catalog.get("smb_not_found").to_string(),
            SmbError::Timeout => catalog.get("smb_timeout").replace("{host}", host),
            SmbError::UnreadableOutput(raw) => {
                catalog.get("smb_unreadable_output").replace("{detail}", raw)
            }
            // Verbatim: an unknown NT_STATUS code is the only information
            // available, and a home-made sentence would lose it.
            SmbError::Other(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for SmbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmbError::NotInstalled => write!(f, "smbclient is not installed"),
            SmbError::BadCredentials => write!(f, "smb credentials refused"),
            SmbError::AccessDenied => write!(f, "smb access denied"),
            SmbError::Unreachable => write!(f, "smb host unreachable"),
            SmbError::NotFound => write!(f, "smb share or folder not found"),
            SmbError::Timeout => write!(f, "smb host timed out"),
            SmbError::UnreadableOutput(b) => write!(f, "unreadable smbclient output: {b}"),
            SmbError::Other(m) => write!(f, "smbclient: {m}"),
        }
    }
}

impl std::error::Error for SmbError {}

/// Classifies `smbclient` outputs.
///
/// **Takes stdout and stderr combined, and that is not a detail**: measured on
/// samba 4.19.5, `do_connect: … failed (Error NT_STATUS_…)` goes to stderr,
/// but `session setup failed: NT_STATUS_ACCESS_DENIED` goes to **stdout**.
/// Classifying on stderr alone would miss the authentication failure, that is,
/// the most frequent case for the user.
pub fn classify(outputs: &str) -> SmbError {
    let s = outputs.to_ascii_uppercase();
    if s.contains("NT_STATUS_LOGON_FAILURE")
        || s.contains("NT_STATUS_WRONG_PASSWORD")
        || s.contains("NT_STATUS_NO_SUCH_USER")
        || s.contains("NT_STATUS_ACCOUNT_DISABLED")
    {
        return SmbError::BadCredentials;
    }
    if s.contains("NT_STATUS_ACCESS_DENIED") {
        return SmbError::AccessDenied;
    }
    // `OBJECT_NAME_NOT_FOUND` is tested **before** the connection failures and
    // is definitely not one of them: the NAS returns it for a missing folder.
    if s.contains("NT_STATUS_OBJECT_NAME_NOT_FOUND")
        || s.contains("NT_STATUS_BAD_NETWORK_NAME")
        || s.contains("NT_STATUS_OBJECT_PATH_NOT_FOUND")
    {
        return SmbError::NotFound;
    }
    if s.contains("NT_STATUS_CONNECTION_REFUSED")
        || s.contains("NT_STATUS_IO_TIMEOUT")
        || s.contains("NT_STATUS_HOST_UNREACHABLE")
        || s.contains("NT_STATUS_NETWORK_UNREACHABLE")
        || s.contains("FAILED TO CONNECT")
    {
        return SmbError::Unreachable;
    }
    let t = outputs.trim();
    if t.is_empty() {
        SmbError::Other("smbclient failed without a message".to_string())
    } else {
        SmbError::Other(t.to_string())
    }
}

/// Parses the output of `smbclient -L //host -g`.
///
/// The machine format (`Type|name|comment`) rather than the human table: the
/// latter changes column widths between versions, and parsing it would have
/// been a defect that only shows up on someone else's machine.
///
/// Administrative shares (`IPC$`, `print$`, any name ending in `$`) are
/// dropped: they hold no music and their presence would make the user doubt
/// which share is the right one.
pub fn parse_shares(stdout: &str) -> Vec<String> {
    let mut out: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("Disk|"))
        .map(|rest| rest.split('|').next().unwrap_or("").trim().to_string())
        .filter(|n| !n.is_empty() && !n.ends_with('$'))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Parses the output of `smbclient -c 'ls'`.
///
/// The format is positional and is read **from the right**: the date takes the
/// last five words, the size the sixth, the attributes the seventh; the name
/// is everything that remains, including its spaces. Reading from the left
/// would break on the first album name containing a space, that is, almost
/// all of them.
///
/// A line that no rule can read is counted: if the output was not empty and
/// nothing was recognized, it is an `UnreadableOutput` and not an empty folder
/// (see the variant).
pub fn parse_ls(stdout: &str) -> Result<Vec<SmbEntry>, SmbError> {
    let mut entries = Vec::new();
    let mut read_lines = 0usize;
    let mut ignored = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // The output footer: "9876543 blocks of size 1024. …".
        if line.contains("blocks of size") {
            read_lines += 1;
            continue;
        }
        match split(line) {
            Some((name, attrs)) => {
                read_lines += 1;
                // `.` and `..` would make the tree loop on itself; hidden
                // entries are dropped as `scan::list_dir` already does, so
                // that the two trees look alike.
                if name == "." || name == ".." || name.starts_with('.') {
                    continue;
                }
                entries.push(SmbEntry { name: name.to_string(), dir: attrs.contains('D') });
            }
            None => ignored.push(line.trim()),
        }
    }

    if read_lines == 0 && !ignored.is_empty() {
        return Err(SmbError::UnreadableOutput(ignored.join(" / ")));
    }
    entries.sort_by(|a, b| (b.dir, &a.name).cmp(&(a.dir, &b.name)));
    Ok(entries)
}

/// Splits an `ls` line from the right: returns `(name, attributes)`.
///
/// The attributes may be **absent** on some versions; in that case the column
/// found is not made only of attribute letters, and we give it back to the
/// name rather than truncating it.
fn split(line: &str) -> Option<(&str, &str)> {
    const ATTRS: &str = "DAHNRSE";
    let mut rest = line.trim_end();
    // Five date words: "Mon Aug 11 20:12:33 2025".
    for _ in 0..5 {
        rest = rest[..rest.rfind(char::is_whitespace)?].trim_end();
    }
    // The size, which must be a number — this is what tells a real entry line
    // from a diagnostic sentence of five words or more.
    let before_size = rest[..rest.rfind(char::is_whitespace)?].trim_end();
    let size = rest[before_size.len()..].trim();
    if size.is_empty() || !size.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // The attributes, if they really form an attribute column.
    let before_attrs = match before_size.rfind(char::is_whitespace) {
        Some(i) => before_size[..i].trim_end(),
        None => return Some((before_size.trim(), "")),
    };
    let attrs = before_size[before_attrs.len()..].trim();
    if !attrs.is_empty() && attrs.chars().all(|c| ATTRS.contains(c)) {
        let name = before_attrs.trim();
        (!name.is_empty()).then_some((name, attrs))
    } else {
        // No attributes: everything before the size is the name.
        let name = before_size.trim();
        (!name.is_empty()).then_some((name, ""))
    }
}

/// Binary name. A named constant so that the end-to-end journey can put a fake
/// `smbclient` in the test server's `PATH`.
const SMBCLIENT: &str = "smbclient";

pub struct Credentials {
    pub user: String,
    pub password: String,
    pub domain: String,
}

/// True if the value can be put as is on a command line.
///
/// An argument starting with `-` would be read by `smbclient` as a flag: the
/// form could then rewrite the command line. `field_on` already covers the
/// comma, the space, `..` and the NUL byte.
fn argument_on(v: &str) -> bool {
    crate::roots::field_on(v) && !v.starts_with('-')
}

/// Temporary authentication file, erased on drop.
///
/// The passphrase **never** goes through `argv`: it would be readable there in
/// `ps` by any user of the machine. Permissions are set **at creation** —
/// creating then restricting would leave a window during which the secret
/// would be readable by everyone.
struct AuthFile(PathBuf);

impl AuthFile {
    /// Writes the file in `dir`, or failing that in the temporary directory.
    ///
    /// The fallback is not a convenience, it is the fix for a defect actually
    /// met: this file landed in the directory of the **persisted** credentials
    /// (`/etc/ritornello/media-credentials`), which does not exist in
    /// development and which an ordinary user cannot create. The symptom was
    /// as misleading as it gets — "smbclient: Permission denied (os error 13)"
    /// — and sent one looking for a mount or SMB rights problem where there
    /// was none.
    ///
    /// Safety does not come from the directory but from the **mode 0600 set at
    /// creation**: a file opened that way in `/tmp` is no more readable than
    /// anywhere else. And it disappears on drop.
    fn create(dir: &Path, creds: &Credentials) -> std::io::Result<Self> {
        match Self::create_in(dir, creds) {
            Ok(f) => Ok(f),
            Err(e) => {
                tracing::debug!("{} is not writable ({e}): falling back to the temp dir", dir.display());
                Self::create_in(&std::env::temp_dir(), creds)
            }
        }
    }

    fn create_in(dir: &Path, creds: &Credentials) -> std::io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(dir)?;
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(".explore-{}-{n}.auth", std::process::id()));
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&path)?;
        use std::io::Write;
        writeln!(f, "username={}", creds.user)?;
        writeln!(f, "password={}", creds.password)?;
        if !creds.domain.is_empty() {
            writeln!(f, "domain={}", creds.domain)?;
        }
        f.sync_all()?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for AuthFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// True if `smbclient` is present and executable.
///
/// A probe rather than an assumption: its absence must grey out the wizard,
/// not make an action fail at the worst moment (see `can_browse_smb`).
pub async fn available() -> bool {
    tokio::process::Command::new(SMBCLIENT)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Runs `smbclient` and returns `(stdout, combined outputs, success)`.
///
/// The timeout is held here, by `tokio`, and not by `smbclient`'s `-t`: its
/// presence and semantics vary between versions, whereas killing the process
/// holds everywhere. Without this cap, a powered-off NAS would hold the task
/// well beyond what the page waits for.
async fn run(args: &[String], timeout: Duration) -> Result<(String, String, bool), SmbError> {
    let child = tokio::process::Command::new(SMBCLIENT)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SmbError::NotInstalled
            } else {
                SmbError::Other(e.to_string())
            }
        })?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(r) => r.map_err(|e| SmbError::Other(e.to_string()))?,
        Err(_) => return Err(SmbError::Timeout),
    };
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    let err = String::from_utf8_lossy(&output.stderr).to_string();
    // Both streams combined for classification: measured on samba 4.19.5, the
    // authentication refusal goes to stdout and the connection refusal to
    // stderr.
    let combined = format!("{out}\n{err}");
    Ok((out, combined, output.status.success()))
}

/// Authentication arguments: file, or guest attempt.
///
/// Empty user → `-N`. Many home NAS expose a public share, and requiring an
/// account would make them inaccessible.
fn args_auth(auth: &Option<AuthFile>) -> Vec<String> {
    match auth {
        Some(f) => vec!["-A".to_string(), f.path().display().to_string()],
        None => vec!["-N".to_string()],
    }
}

fn prepare_auth(creds: Option<&Credentials>, dir: &Path) -> Result<Option<AuthFile>, SmbError> {
    match creds {
        Some(c) if !c.user.is_empty() => {
            Some(AuthFile::create(dir, c).map_err(|e| SmbError::Other(e.to_string()))).transpose()
        }
        _ => Ok(None),
    }
}

/// Enumerates a host's shares.
pub async fn list_shares(
    host: &str,
    creds: Option<&Credentials>,
    work_dir: &Path,
    timeout: Duration,
) -> Result<Vec<String>, SmbError> {
    if !argument_on(host) {
        return Err(SmbError::Other(format!("invalid host: {host}")));
    }
    let auth = prepare_auth(creds, work_dir)?;
    let mut args = vec!["-L".to_string(), format!("//{host}"), "-g".to_string()];
    args.extend(args_auth(&auth));
    let (out, combined, ok) = run(&args, timeout).await?;
    if !ok {
        return Err(classify(&combined));
    }
    Ok(parse_shares(&out))
}

/// Lists a folder of a share.
///
/// The starting directory goes through `-D` rather than through a `cd "…"`
/// slipped into the `-c` string: a name containing a quote would break the
/// parsing `smbclient` does of its own command.
pub async fn list_dir(
    host: &str,
    share: &str,
    path: &str,
    creds: Option<&Credentials>,
    work_dir: &Path,
    timeout: Duration,
) -> Result<Vec<SmbEntry>, SmbError> {
    if !argument_on(host) {
        return Err(SmbError::Other(format!("invalid host: {host}")));
    }
    if !argument_on(share) {
        return Err(SmbError::Other(format!("invalid share: {share}")));
    }
    let start_dir = if path.is_empty() { "/".to_string() } else { format!("/{path}") };
    if start_dir.starts_with('-') || start_dir.contains('\0') || start_dir.contains("..") {
        return Err(SmbError::Other(format!("invalid path: {path}")));
    }
    let auth = prepare_auth(creds, work_dir)?;
    let mut args =
        vec![format!("//{host}/{share}"), "-D".to_string(), start_dir, "-c".to_string(), "ls".to_string()];
    args.extend(args_auth(&auth));
    let (out, combined, ok) = run(&args, timeout).await?;
    if !ok {
        return Err(classify(&combined));
    }
    parse_ls(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_i18n::Catalog;
    use std::path::Path;

    /// Output **captured** from `smbclient -L //192.168.1.15 -g` against a real
    /// Synology NAS.
    ///
    /// Two details one would not have invented: the administrative share
    /// carries the type `IPC|` and not `Disk|`, and a noise line ends the
    /// output without preventing a zero exit code.
    const SHARES: &str = "\
Disk|book|eBooks
Disk|downloads|
Disk|music|System default shared folder
Disk|photo|System default shared folder
Disk|home|Home directory of ritornello
IPC|IPC$|IPC Service ()
SMB1 disabled -- no workgroup available
";

    /// Output **captured** from `smbclient //host/music -D / -c ls`.
    ///
    /// Columns are aligned with spaces: the name can therefore contain some,
    /// and parsing must be done **from the right**. Attributes are one or two
    /// letters. The last entry is the case that justifies everything: spaces,
    /// apostrophe, accents and dash in a single name.
    const LS: &str = "\
  .                                  DA        0  Fri Apr 17 14:46:30 2026
  ..                                  D        0  Sun Aug 16 16:23:48 2026
  Within Temptation                   D        0  Tue Mar 27 20:20:11 2018
  Eagles Of Death Metal               D        0  Fri Feb  7 16:19:36 2020
  Yann Tiersen                       DA        0  Tue Jul 17 23:07:00 2018
  .cache                             DH        0  Sat Jan  4 11:02:10 2025
  cover.jpg                           A   123456  Sat Jan  4 11:02:10 2025
  piste.mp3                           A  9876543  Sat Jan  4 11:02:10 2025
  Le fabuleux Destin d'Amélie Poulain - BO      D        0  Fri Dec 29 19:49:47 2023

\t\t102400 blocks of size 1024. 102380 blocks available
";

    fn sources_catalog() -> Catalog {
        Catalog::load("files", "en", Path::new("/inexistant"), crate::FILES_EN)
    }

    #[test]
    fn administrative_shares_are_dropped() {
        // The NAS announces `IPC$` with the type `IPC|`, not `Disk|`: the
        // prefix therefore already drops it. The `$` filter remains the belt
        // to the braces, for a server without that nicety.
        assert_eq!(parse_shares(SHARES), vec!["book", "downloads", "home", "music", "photo"]);
    }

    #[test]
    fn the_trailing_noise_line_is_not_a_share() {
        // "SMB1 disabled -- no workgroup available" ends the output of a
        // modern NAS without preventing a zero exit code.
        assert!(!parse_shares(SHARES).iter().any(|s| s.contains("SMB1")));
    }

    #[test]
    fn an_empty_shares_output_does_not_panic() {
        assert!(parse_shares("").is_empty());
        assert!(parse_shares("SMB1 disabled -- no workgroup available\n").is_empty());
    }

    #[test]
    fn a_name_with_spaces_survives_parsing() {
        // THE trap of the `ls` format: columns are aligned with spaces.
        // Reading from the left would break on almost all album names —
        // these ones are real.
        let e = parse_ls(LS).unwrap();
        let names: Vec<&str> = e.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"Within Temptation"), "{names:?}");
        assert!(names.contains(&"Eagles Of Death Metal"), "{names:?}");
        // Spaces, apostrophe, accents and dash in a single name: the case that
        // condemns left-to-right parsing as much as the `cd "…"` mentioned.
        assert!(names.contains(&"Le fabuleux Destin d'Amélie Poulain - BO"), "{names:?}");
    }

    #[test]
    fn a_single_digit_day_of_month_does_not_shift_the_split() {
        // "Fri Feb  7" carries two spaces where "Fri Feb 17" has only one.
        // Counting words without re-trimming would shift the name by a column.
        let e = parse_ls(LS).unwrap();
        assert!(e.iter().any(|x| x.name == "Eagles Of Death Metal"));
    }

    #[test]
    fn folders_are_told_from_files() {
        // Attributes are one or two letters: `D` as well as `DA`.
        let e = parse_ls(LS).unwrap();
        assert!(e.iter().find(|x| x.name == "Within Temptation").unwrap().dir);
        assert!(e.iter().find(|x| x.name == "Yann Tiersen").unwrap().dir, "DA attributes");
        assert!(!e.iter().find(|x| x.name == "cover.jpg").unwrap().dir);
        assert!(!e.iter().find(|x| x.name == "piste.mp3").unwrap().dir);
    }

    #[test]
    fn special_and_hidden_entries_are_dropped() {
        // `.` and `..` would make the tree loop on itself; hidden entries are
        // dropped as `scan::list_dir` already does, so that the two trees
        // look alike.
        let names: Vec<String> = parse_ls(LS).unwrap().into_iter().map(|x| x.name).collect();
        assert!(!names.iter().any(|n| n == "." || n == ".." || n == ".cache"), "{names:?}");
    }

    #[test]
    fn the_output_footer_is_not_an_entry() {
        let names: Vec<String> = parse_ls(LS).unwrap().into_iter().map(|x| x.name).collect();
        assert!(!names.iter().any(|n| n.contains("blocks")), "{names:?}");
    }

    #[test]
    fn an_empty_folder_returns_an_empty_list_without_error() {
        // A truly empty folder contains only `.` and `..`: it must be told
        // from an unparseable output.
        let empty = "  .    D    0  Mon Aug 11 20:12:33 2025\n  ..   D    0  Mon Aug 11 20:12:33 2025\n";
        assert_eq!(parse_ls(empty).unwrap(), vec![]);
        assert_eq!(parse_ls("").unwrap(), vec![]);
    }

    #[test]
    fn a_non_empty_but_unparseable_output_is_an_error_and_not_an_empty_folder() {
        // The decision that matters. If a future version changes the format, a
        // full folder would show as empty and the user would conclude that
        // their NAS lost its music. Better a refusal that names the problem.
        let err = parse_ls("something unexpected\non two lines\n").unwrap_err();
        assert!(matches!(err, SmbError::UnreadableOutput(_)), "{err:?}");
    }

    #[test]
    fn a_refused_password_is_recognized() {
        // Measured on samba 4.19.5: this message goes to **stdout**, not to
        // stderr. Classifying on stderr alone would miss the most frequent case.
        assert_eq!(classify("session setup failed: NT_STATUS_LOGON_FAILURE"), SmbError::BadCredentials);
        assert_eq!(classify("session setup failed: NT_STATUS_ACCESS_DENIED"), SmbError::AccessDenied);
    }

    #[test]
    fn an_unreachable_host_is_recognized() {
        // Outputs captured as is on samba 4.19.5.
        assert_eq!(
            classify("do_connect: Connection to 127.0.0.1 failed (Error NT_STATUS_CONNECTION_REFUSED)"),
            SmbError::Unreachable
        );
        assert_eq!(
            classify("do_connect: Connection to 192.0.2.1 failed (Error NT_STATUS_IO_TIMEOUT)"),
            SmbError::Unreachable
        );
    }

    #[test]
    fn a_missing_folder_is_not_an_unreachable_host() {
        // The trap that measurement exposed. The NAS returns
        // NT_STATUS_OBJECT_NAME_NOT_FOUND for a folder that does not exist;
        // filing it with the connection failures would show "the machine did
        // not answer" in front of a merely stale path — and the user would go
        // check their network instead of their tree.
        assert_eq!(
            classify("cd \\NExistePas\\: NT_STATUS_OBJECT_NAME_NOT_FOUND"),
            SmbError::NotFound
        );
    }

    #[test]
    fn an_unknown_error_goes_through_verbatim() {
        // Inventing a generic sentence would lose the only information
        // available to diagnose.
        let e = classify("NT_STATUS_SOMETHING_NEW: the future");
        assert_eq!(e, SmbError::Other("NT_STATUS_SOMETHING_NEW: the future".into()));
    }

    #[test]
    fn a_silent_failure_remains_a_non_empty_message() {
        assert!(matches!(classify("   \n"), SmbError::Other(_)));
    }

    #[test]
    fn every_refusal_resolves_against_the_embedded_catalog() {
        // `Catalog::get` returns the key when it does not find it: without this
        // test, a typo would show "smb_bad_credentials" on screen.
        let c = sources_catalog();
        for e in [
            SmbError::NotInstalled,
            SmbError::BadCredentials,
            SmbError::AccessDenied,
            SmbError::Unreachable,
            SmbError::NotFound,
            SmbError::Timeout,
            SmbError::UnreadableOutput("raw".into()),
        ] {
            let m = e.message(&c, "nas");
            assert!(m.contains(' '), "raw key sent to the screen: {m:?}");
            assert!(!m.contains('{'), "token left as is: {m:?}");
        }
        assert!(SmbError::Unreachable.message(&c, "nas").contains("nas"));
    }

    #[test]
    fn an_argument_that_looks_like_an_option_is_refused() {
        // `smbclient` would read "-L" as a flag. A host named "-L" makes no
        // sense, but it comes from the browser: the command line must not be
        // rewritable from the form.
        assert!(!argument_on("-L"));
        assert!(!argument_on("--user=root"));
        assert!(!argument_on(""));
        assert!(argument_on("192.168.1.20"));
        assert!(argument_on("nas.local"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_auth_file_is_mode_0600_and_disappears() {
        // The passphrase never goes through argv — it would be readable there
        // in `ps` by any user of the machine. Permissions are set at creation,
        // not afterwards: creating then restricting leaves a window.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let creds = Credentials {
            user: "steven".into(),
            password: "secret-du-nas".into(),
            domain: String::new(),
        };
        let path = {
            let f = AuthFile::create(dir.path(), &creds).unwrap();
            let meta = std::fs::metadata(f.path()).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
            let contents = std::fs::read_to_string(f.path()).unwrap();
            assert!(contents.contains("password=secret-du-nas"), "{contents}");
            f.path().to_path_buf()
        };
        // The file erases itself on drop: a passphrase must not outlive the
        // call that requested it.
        assert!(!path.exists(), "the auth file survived");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_writable_work_dir_does_not_block_the_wizard() {
        // The defect actually met, now pinned. This file landed in the
        // directory of the *persisted* credentials, under /etc: nonexistent in
        // development and impossible to create without privilege. The wizard
        // then failed with "smbclient: Permission denied (os error 13)", a
        // message that sent one looking for a mount or SMB rights problem
        // where there was none.
        //
        // And the fallback gives nothing away: mode 0600 is set at creation, a
        // file opened that way in the temporary directory is no more readable
        // than anywhere else.
        use std::os::unix::fs::PermissionsExt;
        let creds = Credentials {
            user: "steven".into(),
            password: "secret-du-nas".into(),
            domain: String::new(),
        };
        let f = AuthFile::create(Path::new("/proc/impossible/a/create"), &creds)
            .expect("the fallback must allow writing regardless");
        assert_eq!(std::fs::metadata(f.path()).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(std::fs::read_to_string(f.path()).unwrap().contains("password=secret-du-nas"));
    }

    #[tokio::test]
    async fn a_refused_host_spawns_no_process() {
        let dir = tempfile::tempdir().unwrap();
        let e = list_shares("-L", None, dir.path(), std::time::Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(e, SmbError::Other(_)), "{e:?}");
    }
}
