//! The transport: `ssh` invocations, never one piloted session.
//!
//! **Decision recorded (a deliberate departure from spec §3):** the
//! read-only survey and applying the bundle are separate invocations of
//! `ssh`, not a single session steering a remote `sh` that would take both
//! commands and bytes on the same stream — that exposes the installer to
//! the remote shell's own read-ahead and to a race on `sudo`'s password. On
//! Linux, macOS and WSL, `ControlMaster` reunites the invocations under one
//! authentication (`base_args`). On native Windows (OpenSSH for Windows has
//! no `ControlMaster`), without a key, ssh itself will ask for its password
//! once per invocation; `sudo`'s own password is asked once either way.
//!
//! `probe` runs the read-only survey. `apply` sends the bundle and runs it
//! as root, through `sudo` when the device needs one; with a sudo password
//! it first proves the password in a call of its own (`check_sudo_password`)
//! so that a wrong one never reaches the bundle's stream. Every function
//! refuses a host argument that could be misread as an `ssh` option or that
//! carries whitespace or control characters (`valid_host`), before a
//! process is ever spawned.

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use anyhow::{Context, bail, ensure};

use crate::device::Sudo;

/// A device to reach over ssh: `account@host`, or a bare host — the shape
/// is ssh's own business. `valid_host` is the only check made here, and it
/// refuses only what could be misread as an option or corrupt a log line,
/// never a host ssh itself would refuse.
///
/// `ssh_program` is the client to run: `ssh` in production (`Target::new`),
/// a fake script in tests, which spares them any mutation of `PATH`.
#[derive(Debug, Clone)]
pub struct Target {
    pub host: String,
    pub ssh_program: PathBuf,
}

impl Target {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into(), ssh_program: PathBuf::from("ssh") }
    }
}

/// Whether `host` is safe to hand to `ssh` as a bare positional argument:
/// not one `ssh` would read as an option (a leading `-`, which could
/// smuggle in `-oProxyCommand=...`), and free of whitespace or control
/// characters — either of which could be split by `ssh` into pieces the
/// caller never intended, or corrupt whatever later logs it.
pub fn valid_host(host: &str) -> bool {
    !host.is_empty() && !host.starts_with('-') && host.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Whether `password` can travel as one line: a `\n` or `\r` inside it
/// would end sudo's read early and let the rest reach `tar`.
pub fn valid_password(password: &str) -> bool {
    !password.contains(['\n', '\r'])
}

/// The private directory the `ControlMaster` socket lives in (unix only:
/// OpenSSH for Windows has no `ControlMaster`, so there is nothing to
/// shelter). Created at mode `0700` directly under `std::env::temp_dir()` —
/// so no other local account can plant or race the socket file — and, when
/// dropped, the master is told to exit (`ssh -O exit`, errors ignored) and
/// the directory removed.
///
/// The name is short on purpose: a unix socket path holds about 104 bytes
/// (macOS) to 108 (Linux) and OpenSSH refuses a longer `ControlPath` before
/// it connects, whatever the host. `%C` alone expands to 40 characters.
///
/// One instance is meant to be shared across the `probe` and the `apply`
/// invocations to one host (`main` does): sharing the directory is
/// what lets `ControlPath` resolve to the same socket for both. It is built
/// for a `Target`, whose host and client its `Drop` needs for `-O exit`.
pub struct ControlDir {
    #[cfg(unix)]
    path: std::path::PathBuf,
    #[cfg(unix)]
    host: String,
    #[cfg(unix)]
    program: PathBuf,
}

#[cfg(unix)]
impl ControlDir {
    pub fn new(target: &Target) -> anyhow::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        ensure!(valid_host(&target.host), "refusing to connect to {:?}: not a plain host argument", target.host);
        let tmp = std::env::temp_dir();
        for _ in 0..32 {
            let path = tmp.join(short_name());
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self { path, host: target.host.clone(), program: target.ssh_program.clone() }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("creating a private directory for the ssh control socket at {}", path.display())
                    });
                }
            }
        }
        bail!("could not find an unused name for the ssh control directory under {}", tmp.display())
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// `ri-` and six random characters.
#[cfg(unix)]
fn short_name() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0),
    );
    let mut n = hasher.finish();
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut name = String::from("ri-");
    for _ in 0..6 {
        name.push(ALPHABET[(n % ALPHABET.len() as u64) as usize] as char);
        n /= ALPHABET.len() as u64;
    }
    name
}

#[cfg(unix)]
fn control_path_option(dir: &std::path::Path) -> String {
    format!("ControlPath={}/%C", dir.display())
}

/// The arguments that ask the master serving `host` to exit.
#[cfg(unix)]
fn exit_args(dir: &std::path::Path, host: &str) -> Vec<String> {
    vec!["-o".to_string(), control_path_option(dir), "-O".to_string(), "exit".to_string(), host.to_string()]
}

#[cfg(not(unix))]
impl ControlDir {
    pub fn new(target: &Target) -> anyhow::Result<Self> {
        ensure!(valid_host(&target.host), "refusing to connect to {:?}: not a plain host argument", target.host);
        Ok(Self {})
    }
}

#[cfg(unix)]
impl Drop for ControlDir {
    fn drop(&mut self) {
        // The master detaches and outlives the last client for as long as
        // ControlPersist says: end it now, or it stays behind the installer
        // with its authenticated connection. It may not exist at all (no
        // call reached the host), and nothing here may fail or print.
        let _ = Command::new(&self.program)
            .args(exit_args(&self.path, &self.host))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The `ssh` options every invocation opens with. On `cfg(unix)`, the
/// `ControlMaster` trio pointed at `control_dir`'s own private directory;
/// elsewhere, none — nothing here may ever add `StrictHostKeyChecking` or
/// `UserKnownHostsFile`: host-key checking is ssh's business and stays on.
///
/// The master persists an hour idle (R50): the gap between the survey and
/// the apply holds the screens, the operator reading the plan, the sudo
/// password being typed and every download, and a master that expired in
/// it would have ssh ask for its password again at the start of apply.
/// Still bounded, for a run killed before `ControlDir`'s `Drop` — which
/// otherwise ends it explicitly.
#[cfg(unix)]
pub fn base_args(control_dir: &ControlDir) -> Vec<String> {
    vec![
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        control_path_option(control_dir.path()),
        "-o".to_string(),
        "ControlPersist=3600".to_string(),
    ]
}

#[cfg(not(unix))]
pub fn base_args(_control_dir: &ControlDir) -> Vec<String> {
    Vec::new()
}

/// The command `ssh` runs on the device to apply the bundle it is about to
/// receive on its stdin, as `sudo` when the device needs one.
///
/// `umask 077` runs before `mktemp -d` (R44): the bundle directory must be
/// private to root for the whole run. `RITORNELLO_INSTALL_ROOT=` is set
/// explicitly, empty (R43): without it, an inherited value — the login's
/// own environment, which is exactly what runs under `Sudo::NotNeeded` —
/// could redirect every write `apply.sh` makes. `tar -x -f -` names its
/// archive, so `$TAPE` is never consulted. `rm -rf "$d"` runs after
/// `apply.sh`'s own exit code is captured, on every exit path: the `;` (not
/// `&&`) before it means it runs whether `apply.sh` succeeded or not, and
/// the captured `$r` is what the whole command exits with.
///
/// With a sudo password, `sudo -k` ignores any cached credential and always
/// prompts, so the password line is always consumed by sudo and can never
/// reach `tar`.
pub fn remote_apply_command(sudo: Sudo) -> anyhow::Result<String> {
    let body = r#"sh -c 'umask 077 && d=$(mktemp -d) && tar -x -f - -C "$d" && RITORNELLO_INSTALL_ROOT= RITORNELLO_INSTALL_BUNDLE="$d" sh "$d/apply.sh"; r=$?; rm -rf "$d"; exit $r'"#;
    Ok(match sudo {
        Sudo::NotNeeded => body.to_string(),
        Sudo::NoPassword => format!("sudo -n {body}"),
        Sudo::Password => format!("sudo -k -S -p '' {body}"),
        Sudo::Absent => bail!("the account is not root and the device has no sudo: connect as root, or install sudo"),
    })
}

/// The command that proves a sudo password before anything else is sent:
/// `-k` forces a prompt whatever sudo has cached, so the answer says
/// whether *this* password is the right one.
pub fn sudo_check_command() -> &'static str {
    "sudo -k -S -p '' true"
}

/// What the check sends on stdin: the password line, and nothing else.
pub fn sudo_check_stdin(password: &str) -> Vec<u8> {
    format!("{password}\n").into_bytes()
}

/// The bytes sent on `apply`'s stdin, in the one order that keeps `sudo -S`
/// from ever handing a byte of the password on to `tar`: with
/// `Sudo::Password`, the password and its newline come first — `sudo -S`
/// reads its prompt answer up to that newline and leaves everything after
/// it, byte for byte, to the process it then execs. With `Sudo::NoPassword`
/// or `Sudo::NotNeeded`, nothing precedes the bundle at all: anything that
/// did would reach `tar` instead of a prompt that is never asked.
pub fn apply_stdin(sudo: Sudo, password: Option<&str>, bundle: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if sudo == Sudo::Password {
        if let Some(p) = password {
            out.extend_from_slice(p.as_bytes());
        }
        out.push(b'\n');
    }
    out.extend_from_slice(bundle);
    out
}

fn spawn(program: &std::path::Path, args: &[String]) -> anyhow::Result<Child> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {}", program.display()))
}

/// Reads `read` line by line, calling `on_line` with each one (its
/// terminator stripped, invalid UTF-8 replaced) as it arrives, and returns
/// the last non-empty line seen — the reason a run failed is what its last
/// diagnostic line says. Reading never stops on a byte that is not UTF-8:
/// closing the pipe early would cut the remote session's channel while
/// `apply.sh` may still be working.
fn pump(read: impl Read, mut on_line: impl FnMut(&str)) -> String {
    let mut reader = io::BufReader::new(read);
    let mut last = String::new();
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&bytes);
                let trimmed = line.trim_end_matches(['\n', '\r']);
                on_line(trimmed);
                if !trimmed.is_empty() {
                    last = trimmed.to_string();
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    last
}

/// What one ssh run left behind. The write to its stdin is kept aside: a
/// child that exits before reading everything makes it fail with a broken
/// pipe, which is a consequence and never the reason.
struct Run {
    status: ExitStatus,
    stdout: Vec<u8>,
    last_err_line: String,
    write: io::Result<()>,
}

impl Run {
    /// Fails with the child's own exit status and last stderr line when it
    /// did not succeed — and only then with the failed write.
    fn conclude(self, what: &str) -> anyhow::Result<Vec<u8>> {
        ensure!(self.status.success(), "ssh exited with {}: {}", self.status, self.last_err_line);
        self.write.with_context(|| format!("writing {what} to ssh's stdin"))?;
        Ok(self.stdout)
    }
}

/// Runs `program args`, feeding `input`. The write and the reads each run
/// on their own thread, so that neither can deadlock against ssh waiting on
/// the other. With `relay`, the remote's stdout and stderr go to the screen
/// line by line as they arrive (stdout is then not kept); otherwise stdout
/// is collected and stderr only remembered.
fn run(program: &std::path::Path, args: &[String], input: Vec<u8>, relay: bool) -> anyhow::Result<Run> {
    let mut child = spawn(program, args)?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out_thread = std::thread::spawn(move || -> io::Result<Vec<u8>> {
        if relay {
            pump(stdout, |line| println!("{line}"));
            Ok(Vec::new())
        } else {
            let mut all = Vec::new();
            stdout.read_to_end(&mut all).map(|_| all)
        }
    });
    let last_err_line = if relay { pump(stderr, |line| eprintln!("{line}")) } else { pump(stderr, |_| {}) };

    let write = writer.join().expect("the stdin writer thread does not panic");
    let out = out_thread.join().expect("the stdout thread does not panic");
    let status = child.wait().context("waiting for ssh")?;
    let stdout = out.context("reading ssh's stdout")?;
    Ok(Run { status, stdout, last_err_line, write })
}

fn ensure_plain_host(target: &Target) -> anyhow::Result<()> {
    ensure!(valid_host(&target.host), "refusing to connect to {:?}: not a plain host argument", target.host);
    Ok(())
}

/// Runs the read-only survey: `ssh <base_args> <host> sh -s`, `script` fed
/// on its stdin, its stdout returned whole. A non-zero exit is an error
/// citing the last line `ssh` wrote to its stderr.
/// The command `ssh` runs to survey the device. `RITORNELLO_INSTALL_ROOT=` is
/// set explicitly, empty, exactly as for the apply (R43): the survey script
/// honours that variable, and an inherited value from the login's own
/// environment would make it look at the wrong root.
pub fn remote_probe_command() -> &'static str {
    "RITORNELLO_INSTALL_ROOT= sh -s"
}

pub fn probe(target: &Target, control_dir: &ControlDir, script: &str) -> anyhow::Result<String> {
    ensure_plain_host(target)?;
    let mut args = base_args(control_dir);
    args.push(target.host.clone());
    args.push(remote_probe_command().to_string());
    let stdout = run(&target.ssh_program, &args, script.as_bytes().to_vec(), false)?.conclude("the probe script")?;
    String::from_utf8(stdout).context("the survey's output is not UTF-8")
}

/// Proves `password` against sudo through the shared master, in a call of
/// its own that sends the password line and nothing else. A refusal is
/// reported as a wrong sudo password; ssh's own failure (exit 255) is
/// reported as ssh's.
pub fn check_sudo_password(target: &Target, control_dir: &ControlDir, password: &str) -> anyhow::Result<()> {
    ensure_plain_host(target)?;
    ensure!(valid_password(password), "the sudo password cannot contain a line break");
    let mut args = base_args(control_dir);
    args.push(target.host.clone());
    args.push(sudo_check_command().to_string());
    let run = run(&target.ssh_program, &args, sudo_check_stdin(password), false)?;
    if run.status.success() {
        return Ok(());
    }
    if run.status.code() == Some(255) {
        bail!("ssh failed before sudo ran: {}", run.last_err_line);
    }
    bail!("wrong sudo password: sudo refused it ({})", run.last_err_line)
}

/// Sends the bundle and runs it as root: `ssh <base_args> <host>
/// <remote_apply_command>`, the stdin `apply_stdin` assembles. With
/// `Sudo::Password` the password is proved first (`check_sudo_password`),
/// and a wrong one stops here, before the bundle is sent. The remote's own
/// progress and diagnostics — `apply.sh`'s `say` on stdout, its `die` on
/// stderr — are relayed to the screen as they arrive. A non-zero exit is an
/// error citing the last line of stderr; the password is never printed,
/// since it never reaches either stream.
pub fn apply(
    target: &Target,
    control_dir: &ControlDir,
    sudo: Sudo,
    password: Option<&str>,
    bundle: &[u8],
) -> anyhow::Result<()> {
    ensure_plain_host(target)?;
    let command = remote_apply_command(sudo)?;
    if sudo == Sudo::Password {
        let password = password.context("sudo needs a password on this device, and none was given")?;
        ensure!(valid_password(password), "the sudo password cannot contain a line break");
        check_sudo_password(target, control_dir, password)?;
    }
    let mut args = base_args(control_dir);
    args.push(target.host.clone());
    args.push(command);
    run(&target.ssh_program, &args, apply_stdin(sudo, password, bundle), true)?.conclude("the bundle")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apply_command_extracts_runs_and_cleans_up_whatever_the_outcome() {
        let c = remote_apply_command(Sudo::NotNeeded).unwrap();
        assert!(c.contains("mktemp -d") && c.contains("apply.sh"));
        assert!(c.contains("tar -x -f - -C"), "the archive is named, so $TAPE is never consulted: {c}");
        assert!(c.contains("rm -rf"), "the bundle directory is removed even on failure");
        assert!(!c.contains("sudo"));
    }

    #[test]
    fn sudo_reads_its_password_from_the_stream_only_when_it_needs_one() {
        assert!(remote_apply_command(Sudo::NoPassword).unwrap().starts_with("sudo -n "));
        assert!(remote_apply_command(Sudo::Absent).is_err(), "no root, no sudo: said, not attempted");
    }

    /// R48: `-k` makes sudo prompt whatever it has cached, so the password
    /// line is always sudo's and never `tar`'s.
    #[test]
    fn the_password_variants_force_a_prompt() {
        assert!(
            remote_apply_command(Sudo::Password).unwrap().starts_with("sudo -k -S -p '' sh -c "),
            "{}",
            remote_apply_command(Sudo::Password).unwrap()
        );
        assert_eq!(sudo_check_command(), "sudo -k -S -p '' true");
    }

    #[test]
    fn the_check_sends_the_password_line_and_nothing_else() {
        assert_eq!(sudo_check_stdin("hunter2"), b"hunter2\n".to_vec());
        assert!(!sudo_check_command().contains("hunter2"));
    }

    /// R43: an inherited `RITORNELLO_INSTALL_ROOT` must never redirect
    /// root's writes — most of all under `Sudo::NotNeeded`, where the
    /// environment `apply.sh` runs under really is the login's own.
    #[test]
    fn the_remote_command_clears_the_install_root_explicitly() {
        for sudo in [Sudo::NotNeeded, Sudo::NoPassword, Sudo::Password] {
            let c = remote_apply_command(sudo).unwrap();
            assert!(
                c.contains("RITORNELLO_INSTALL_ROOT= "),
                "RITORNELLO_INSTALL_ROOT must be set to nothing, explicitly, for {sudo:?}: {c}"
            );
        }
    }

    /// I2: the survey honours `RITORNELLO_INSTALL_ROOT` too, so its command
    /// clears it the same way the apply's does.
    #[test]
    fn the_probe_command_clears_the_install_root_explicitly() {
        assert!(remote_probe_command().starts_with("RITORNELLO_INSTALL_ROOT= sh -s"), "{}", remote_probe_command());
    }

    /// R44: the bundle directory is private to root (`umask 077` runs
    /// before `mktemp -d`) and removed on every exit path.
    #[test]
    fn the_bundle_directory_is_private_and_removed_whatever_the_outcome() {
        let c = remote_apply_command(Sudo::NotNeeded).unwrap();
        let umask_at = c.find("umask 077").expect("umask 077 must run");
        let mktemp_at = c.find("mktemp -d").expect("mktemp -d must run");
        assert!(umask_at < mktemp_at, "umask 077 must run before mktemp -d: {c}");
        assert!(c.contains("rm -rf"), "the bundle directory must be removed even on failure: {c}");
    }

    /// Host-key checking is ssh's business and stays on: nothing here may
    /// weaken it.
    #[test]
    fn no_argument_weakens_host_key_checking() {
        let dir = ControlDir::new(&Target::new("pi@device")).expect("creating a private control directory");
        let args = base_args(&dir).join(" ");
        assert!(!args.contains("StrictHostKeyChecking"));
        assert!(!args.contains("UserKnownHostsFile"));
    }

    #[test]
    fn apply_stdin_is_password_then_bundle_only_when_sudo_needs_one() {
        let bundle: &[u8] = b"BUNDLEBYTES";
        assert_eq!(apply_stdin(Sudo::Password, Some("hunter2"), bundle), b"hunter2\nBUNDLEBYTES".to_vec());
        for sudo in [Sudo::NotNeeded, Sudo::NoPassword] {
            assert_eq!(
                apply_stdin(sudo, Some("hunter2"), bundle),
                bundle,
                "nothing may precede the bundle for {sudo:?} — it would otherwise reach tar"
            );
        }
        // The password only ever travels on stdin: no command string
        // carries it.
        for command in [remote_apply_command(Sudo::Password).unwrap(), sudo_check_command().to_string()] {
            assert!(!command.contains("hunter2"), "the password must never appear on the command line: {command}");
        }
    }

    #[test]
    fn a_host_argument_that_looks_like_an_option_or_carries_whitespace_is_refused() {
        for bad in ["-oProxyCommand=touch /tmp/pwned", "-", "user@host extra", "user@host\n", "user@host\t", "user@host\0"]
        {
            assert!(!valid_host(bad), "{bad:?} must be refused");
        }
        for ok in ["pi@192.168.0.57", "dietpi@raspberrypi.local", "root@host", "host-only"] {
            assert!(valid_host(ok), "{ok:?} must be accepted");
        }
    }

    #[test]
    fn a_host_that_looks_like_an_ssh_option_is_refused_before_spawning_anything() {
        let bad = Target::new("-oProxyCommand=touch /tmp/pwned");
        assert!(ControlDir::new(&bad).is_err(), "no control directory for such a host");
        // A target whose host was changed after its control directory was
        // made is refused by each function too.
        let good = ControlDir::new(&Target::new("pi@device")).unwrap();
        let err = probe(&bad, &good, "true\n").unwrap_err().to_string();
        assert!(err.contains("host"), "{err}");
        let err = apply(&bad, &good, Sudo::NotNeeded, None, b"").unwrap_err().to_string();
        assert!(err.contains("host"), "{err}");
        let err = check_sudo_password(&bad, &good, "x").unwrap_err().to_string();
        assert!(err.contains("host"), "{err}");
    }

    #[test]
    fn apply_refuses_to_run_without_a_password_when_sudo_needs_one() {
        let target = Target::new("pi@device");
        let dir = ControlDir::new(&target).unwrap();
        let err = apply(&target, &dir, Sudo::Password, None, b"").unwrap_err().to_string();
        assert!(err.contains("password"), "{err}");
    }

    /// M4: a line break inside the password would end sudo's read early and
    /// send the rest to `tar`. Refused before anything is spawned: the
    /// client here does not exist, so a spawn would say so instead.
    #[test]
    fn a_password_with_a_line_break_is_refused_before_anything_is_sent() {
        let mut target = Target::new("pi@device");
        target.ssh_program = PathBuf::from("/nonexistent/ssh");
        let dir = ControlDir::new(&target).unwrap();
        for bad in ["a\nb", "a\rb", "a\r\nb", "hunter2\n"] {
            let err = apply(&target, &dir, Sudo::Password, Some(bad), b"BUNDLE").unwrap_err().to_string();
            assert!(err.contains("line break"), "{bad:?}: {err}");
            let err = check_sudo_password(&target, &dir, bad).unwrap_err().to_string();
            assert!(err.contains("line break"), "{bad:?}: {err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_control_directory_is_private_short_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let target = Target::new("pi@device");
        let dir = ControlDir::new(&target).expect("creating a private control directory");
        let path = dir.path().to_path_buf();
        let mode = std::fs::metadata(&path).expect("stat the control directory").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the control directory must be private to this account");
        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()), "directly under the temporary directory");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("ri-") && name.len() == 9, "`ri-` and six characters, no more: {name}");
        // Drop runs the fake-less `ssh -O exit`, which fails harmlessly
        // here: the point is the directory is gone after it.
        drop(dir);
        assert!(!path.exists(), "the control directory must not outlive its ControlDir");
    }

    /// C1: OpenSSH refuses a `ControlPath` of 108 bytes or more (macOS: 104)
    /// before it connects, whatever the host. `%C` expands to 40 characters.
    #[cfg(unix)]
    #[test]
    fn the_expanded_control_path_fits_a_unix_socket() {
        let dir = ControlDir::new(&Target::new("a-very-long-account-name@a-very-long-host-name.example.org")).unwrap();
        let option = base_args(&dir)
            .into_iter()
            .find(|a| a.starts_with("ControlPath="))
            .expect("a ControlPath option");
        let template = option.strip_prefix("ControlPath=").unwrap();
        assert!(template.ends_with("/%C"), "the socket name is the hash alone: {template}");
        let expanded = template.replace("%C", &"0".repeat(40));
        assert!(expanded.len() < 104, "{} bytes, {expanded}", expanded.len());
        assert!(
            dir.path().display().to_string().len() + 40 + 1 < 104,
            "the directory alone leaves no room for %C"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_master_persists_long_enough_to_bridge_the_survey_and_the_apply() {
        let dir = ControlDir::new(&Target::new("pi@device")).unwrap();
        assert!(base_args(&dir).contains(&"ControlPersist=3600".to_string()), "{:?}", base_args(&dir));
    }

    /// I1: the exit request names the same socket the invocations use, and
    /// the host.
    #[cfg(unix)]
    #[test]
    fn the_exit_request_is_formed_for_the_same_socket_and_host() {
        let dir = ControlDir::new(&Target::new("pi@device")).unwrap();
        let exit = exit_args(dir.path(), "pi@device");
        let control_path = base_args(&dir).into_iter().find(|a| a.starts_with("ControlPath=")).unwrap();
        assert_eq!(exit, ["-o", control_path.as_str(), "-O", "exit", "pi@device"]);
    }

    /// No real `ssh` runs in these tests: a fake one, given to the `Target`
    /// by path, records every invocation (its arguments on one line of
    /// `calls`, its stdin in `stdin.<n>`) and then does what the test's own
    /// script says. Unix only.
    #[cfg(unix)]
    mod fake_ssh {
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;

        use super::*;

        const RECORD: &str = r#"d=$(dirname "$0")
n=$(cat "$d/n" 2>/dev/null || echo 0); n=$((n+1)); echo $n > "$d/n"
printf '%s\n' "$*" >> "$d/calls"
cat > "$d/stdin.$n"
"#;

        /// A fake `ssh` in `dir` running `RECORD` and then `body`; the
        /// `Target` that uses it.
        fn target_with(dir: &Path, record: bool, body: &str) -> Target {
            let path = dir.join("ssh");
            let prologue = if record { RECORD } else { "" };
            std::fs::write(&path, format!("#!/bin/sh\n{prologue}{body}\n")).expect("writing the fake ssh");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod the fake ssh");
            let mut target = Target::new("pi@device");
            target.ssh_program = path;
            target
        }

        fn calls(dir: &Path) -> Vec<String> {
            std::fs::read_to_string(dir.join("calls")).unwrap_or_default().lines().map(str::to_string).collect()
        }

        fn stdin_of(dir: &Path, n: usize) -> Vec<u8> {
            std::fs::read(dir.join(format!("stdin.{n}"))).expect("reading the captured stdin")
        }

        #[test]
        fn probe_sends_the_script_on_stdin_and_returns_stdout() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(tmp.path(), true, "printf 'probe-stdout-marker\\n'");
            let control = ControlDir::new(&target).unwrap();
            let out = probe(&target, &control, "the probe script\n").expect("the fake ssh succeeds");
            assert_eq!(out, "probe-stdout-marker\n");
            let calls = calls(tmp.path());
            assert_eq!(calls.len(), 1, "{calls:?}");
            assert!(calls[0].ends_with("pi@device RITORNELLO_INSTALL_ROOT= sh -s"), "{calls:?}");
            assert_eq!(stdin_of(tmp.path(), 1), b"the probe script\n");
        }

        #[test]
        fn apply_without_a_password_sends_the_bundle_alone() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(tmp.path(), true, "printf 'progress\\n'");
            let control = ControlDir::new(&target).unwrap();
            apply(&target, &control, Sudo::NoPassword, None, b"BUNDLE").expect("the fake ssh succeeds");
            let calls = calls(tmp.path());
            assert_eq!(calls.len(), 1, "no check without a password: {calls:?}");
            assert!(calls[0].contains("ControlMaster=auto") && calls[0].contains("pi@device sudo -n sh -c"), "{calls:?}");
            assert_eq!(stdin_of(tmp.path(), 1), b"BUNDLE");
        }

        /// R48: the password is proved first, alone; then the bundle goes
        /// under `sudo -k` with the password line in front of it.
        #[test]
        fn apply_with_a_password_proves_it_alone_and_then_applies() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(tmp.path(), true, "exit 0");
            let control = ControlDir::new(&target).unwrap();
            apply(&target, &control, Sudo::Password, Some("hunter2"), b"BUNDLE").expect("the fake ssh succeeds");
            let calls = calls(tmp.path());
            assert_eq!(calls.len(), 2, "{calls:?}");
            assert!(calls[0].ends_with("pi@device sudo -k -S -p '' true"), "{calls:?}");
            assert_eq!(stdin_of(tmp.path(), 1), b"hunter2\n", "the check sends the password line only");
            assert!(calls[1].contains("pi@device sudo -k -S -p '' sh -c "), "{calls:?}");
            assert_eq!(stdin_of(tmp.path(), 2), b"hunter2\nBUNDLE");
            assert!(calls.iter().all(|c| !c.contains("hunter2")), "the password is on no command line: {calls:?}");
        }

        /// R48: a wrong password is said to be one, and the bundle is never
        /// run (nor sent).
        #[test]
        fn a_wrong_sudo_password_stops_before_the_bundle_is_sent() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(tmp.path(), true, "printf 'Sorry, try again.\\n' >&2; exit 1");
            let control = ControlDir::new(&target).unwrap();
            let err = apply(&target, &control, Sudo::Password, Some("hunter2"), b"BUNDLE").unwrap_err().to_string();
            assert!(err.contains("wrong sudo password"), "{err}");
            assert!(!err.contains("hunter2"), "{err}");
            assert_eq!(calls(tmp.path()).len(), 1, "apply must never run: {:?}", calls(tmp.path()));
        }

        #[test]
        fn an_ssh_failure_during_the_check_is_not_called_a_wrong_password() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(tmp.path(), true, "printf 'ssh: connection refused\\n' >&2; exit 255");
            let control = ControlDir::new(&target).unwrap();
            let err = check_sudo_password(&target, &control, "hunter2").unwrap_err().to_string();
            assert!(err.contains("connection refused") && !err.contains("wrong sudo password"), "{err}");
        }

        #[test]
        fn a_non_zero_exit_surfaces_the_last_stderr_line_and_never_the_password() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(
                tmp.path(),
                true,
                "if [ \"$n\" = 2 ]; then printf 'still stopped\\n' >&2; printf 'boom\\n' >&2; exit 1; fi",
            );
            let control = ControlDir::new(&target).unwrap();
            let err = apply(&target, &control, Sudo::Password, Some("hunter2"), b"BUNDLE").unwrap_err().to_string();
            assert!(err.contains("boom"), "{err}");
            assert!(!err.contains("hunter2"), "the password must never appear in an error: {err}");
        }

        /// M5: a child that leaves before reading the bundle makes the write
        /// fail with a broken pipe; the error must be the child's reason.
        #[test]
        fn a_broken_pipe_does_not_hide_the_remote_reason() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(tmp.path(), false, "printf 'sudo: a password is required\\n' >&2; exit 1");
            let control = ControlDir::new(&target).unwrap();
            let big = vec![0u8; 8 * 1024 * 1024];
            let err = apply(&target, &control, Sudo::NoPassword, None, &big).unwrap_err().to_string();
            assert!(err.contains("a password is required"), "{err}");
            assert!(!err.contains("Broken pipe"), "{err}");
        }

        /// M6: a byte that is not UTF-8 in the remote's output must not end
        /// the relay: what follows is still read, and still cited.
        #[test]
        fn output_that_is_not_utf8_does_not_stop_the_relay() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(
                tmp.path(),
                true,
                "printf 'caf\\351 \\377\\376\\n' >&2; printf 'progress\\377\\n'; printf 'boom\\n' >&2; exit 1",
            );
            let control = ControlDir::new(&target).unwrap();
            let err = apply(&target, &control, Sudo::NoPassword, None, b"BUNDLE").unwrap_err().to_string();
            assert!(err.contains("boom"), "{err}");
        }

        /// I1: dropping the control directory asks the master to exit,
        /// naming the socket and the host, then removes the directory.
        #[test]
        fn dropping_the_control_directory_asks_the_master_to_exit() {
            let tmp = tempfile::tempdir().unwrap();
            let target = target_with(tmp.path(), true, "exit 0");
            let control = ControlDir::new(&target).unwrap();
            let path = control.path().to_path_buf();
            drop(control);
            let calls = calls(tmp.path());
            assert_eq!(calls.len(), 1, "{calls:?}");
            assert!(
                calls[0].contains(&format!("ControlPath={}/%C -O exit pi@device", path.display())),
                "{calls:?}"
            );
            assert!(!path.exists());
        }
    }

    /// M8: the remote command run through a real `sh -c`, as sshd would run
    /// it, with a real (tiny) bundle whose `apply.sh` reports what it sees.
    /// Under sudo, a fake `sudo` that reads the password line the way
    /// `sudo -S` does and then execs the rest stands in for the real one.
    #[cfg(unix)]
    mod through_a_shell {
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;
        use std::process::Output;

        use super::*;

        fn shell() -> &'static str {
            if Path::new("/bin/dash").exists() { "/bin/dash" } else { "sh" }
        }

        const APPLY_SH: &str = r#"#!/bin/sh
printf 'ROOT=[%s]\n' "$RITORNELLO_INSTALL_ROOT"
printf 'BUNDLE=%s\n' "$RITORNELLO_INSTALL_BUNDLE"
printf 'DIRMODE=%s\n' "$(ls -ld "$RITORNELLO_INSTALL_BUNDLE" | cut -c1-10)"
printf 'FILEMODE=%s\n' "$(ls -l "$0" | cut -c1-10)"
exit 3
"#;

        fn bundle() -> Vec<u8> {
            let mut b = tar::Builder::new(Vec::new());
            let mut h = tar::Header::new_gnu();
            h.set_size(APPLY_SH.len() as u64);
            h.set_mode(0o755);
            h.set_mtime(0);
            h.set_entry_type(tar::EntryType::Regular);
            b.append_data(&mut h, "apply.sh", APPLY_SH.as_bytes()).unwrap();
            b.into_inner().unwrap()
        }

        const FAKE_SUDO: &str = r#"#!/bin/sh
prompt=
forced=
while [ $# -gt 0 ]; do
  case $1 in
    -n) shift ;;
    -k) forced=1; shift ;;
    -S) prompt=1; shift ;;
    -p) shift 2 ;;
    *) break ;;
  esac
done
if [ -n "$prompt" ]; then
  [ -n "$forced" ] || { echo "fake sudo: a prompt without -k" >&2; exit 9; }
  IFS= read -r line
  [ "$line" = hunter2 ] || { echo "fake sudo: wrong password" >&2; exit 1; }
fi
exec "$@"
"#;

        /// Runs `command` under a shell, `umask 000` (so only the command's
        /// own `umask 077` can make anything private) and a login
        /// environment that carries a stale `RITORNELLO_INSTALL_ROOT`.
        fn run(command: &str, stdin: &[u8], tmpdir: &Path) -> Output {
            let mut child = Command::new(shell())
                .arg("-c")
                .arg(format!("umask 000; {command}"))
                .env("TMPDIR", tmpdir)
                .env("RITORNELLO_INSTALL_ROOT", "/inherited/elsewhere")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the shell runs");
            let mut input = child.stdin.take().unwrap();
            let bytes = stdin.to_vec();
            let writer = std::thread::spawn(move || input.write_all(&bytes));
            let out = child.wait_with_output().expect("the shell finishes");
            let _ = writer.join();
            out
        }

        fn assert_ran_as_a_private_root_run(out: &Output, tmpdir: &Path, what: &str) {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(3), "{what}: apply.sh's exit code propagates\n{stdout}\n{stderr}");
            assert!(stdout.contains("ROOT=[]\n"), "{what}: the inherited root is cleared\n{stdout}\n{stderr}");
            assert!(stdout.contains("DIRMODE=drwx------\n"), "{what}: the bundle directory is 0700\n{stdout}");
            assert!(
                std::fs::read_dir(tmpdir).unwrap().next().is_none(),
                "{what}: the bundle directory is gone afterwards"
            );
        }

        fn is_root() -> bool {
            let uid = Command::new("id").arg("-u").output().expect("id runs");
            String::from_utf8_lossy(&uid.stdout).trim() == "0"
        }

        #[test]
        fn the_command_runs_extracts_privately_clears_the_root_and_cleans_up() {
            let tmp = tempfile::tempdir().unwrap();
            let command = remote_apply_command(Sudo::NotNeeded).unwrap();
            let out = run(&command, &apply_stdin(Sudo::NotNeeded, None, &bundle()), tmp.path());
            assert_ran_as_a_private_root_run(&out, tmp.path(), "NotNeeded");
            // Extracted under `umask 077`, whatever the caller's umask.
            // Root's tar restores the archive's own modes, so it can't tell.
            if !is_root() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(stdout.contains("FILEMODE=-rwx------\n"), "{stdout}");
            }
        }

        #[test]
        fn the_sudo_variants_deliver_the_bundle_to_tar_and_the_password_line_to_sudo() {
            let tmp = tempfile::tempdir().unwrap();
            let sudo = tmp.path().join("fake-sudo");
            std::fs::write(&sudo, FAKE_SUDO).unwrap();
            std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).unwrap();
            for (variant, password) in [(Sudo::NoPassword, None), (Sudo::Password, Some("hunter2"))] {
                let scratch = tempfile::tempdir().unwrap();
                let command = remote_apply_command(variant).unwrap().replacen("sudo", sudo.to_str().unwrap(), 1);
                let out = run(&command, &apply_stdin(variant, password, &bundle()), scratch.path());
                assert_ran_as_a_private_root_run(&out, scratch.path(), &format!("{variant:?}"));
            }
        }
    }
}
