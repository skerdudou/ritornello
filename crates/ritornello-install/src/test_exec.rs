//! Writing a script a test then runs, without "Text file busy".
//!
//! `std::fs::write` followed by a spawn races every other test of the same
//! binary: a test thread that forks while this file is still open for
//! writing hands the descriptor to its child, which holds it until its own
//! `exec`, and running the script in that window fails with `ETXTBSY`. It
//! failed this way on CI (`spawning /tmp/.tmp…/ssh`, aarch64, the
//! v0.2.0-beta.7 tag run). Measured on WSL with four threads writing and
//! running scripts while a fifth spawns `true` in a loop: 136, 175 and 185
//! failures out of 2000, all `ExecutableFileBusy`; 0 out of 2000, twice,
//! with the writing done below.
//!
//! The file is written by a child process instead: this process never holds
//! a descriptor open for writing on it, so there is none for a sibling's
//! fork to inherit, and the child has exited, closing its own, before the
//! script is run.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `content` to `path` with mode 0755, through `sh`.
pub fn write_executable(path: &Path, content: &str) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawning sh to write an executable");
    child
        .stdin
        .take()
        .expect("sh's stdin")
        .write_all(content.as_bytes())
        .expect("writing the executable's content");
    let status = child.wait().expect("waiting for sh");
    assert!(status.success(), "writing {} failed: {status}", path.display());
}
