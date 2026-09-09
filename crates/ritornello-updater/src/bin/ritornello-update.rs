//! Two verbs, and no third.
//!
//! `apply` reads the request the core staged and places what it names.
//! `rollback` is reached only by `OnFailure=` on `ritornello.service`, and
//! puts back what the last fresh install replaced.
//!
//! Thin on purpose: everything worth testing lives in the library, driven
//! against a temporary root. What is here is argument parsing, the clock, and
//! two `systemctl` calls — the same split as `system.rs` in the core, whose
//! pure parsers carry the tests and whose I/O wrappers do not.

use anyhow::{bail, Context, Result};
use ritornello_updater::{apply, marker, request::Request, rollback};
use std::path::Path;

/// Where the core stages what it downloaded, and its request.
///
/// Inside the service's own state directory, so the unprivileged side can
/// write it. Read here and never trusted: see `apply`.
const STAGING: &str = "/var/lib/ritornello/staging";
const REQUEST: &str = "/var/lib/ritornello/staging/request.json";

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        // A clock before the epoch is not a case worth a branch: zero makes
        // every marker read as fresh, which `marker::is_fresh` already
        // documents as the safe answer.
        .unwrap_or(0)
}

fn main() -> Result<()> {
    let verb = std::env::args().nth(1).unwrap_or_default();
    match verb.as_str() {
        "apply" => do_apply(),
        "rollback" => do_rollback(),
        other => bail!("unknown verb {other:?}: expected `apply` or `rollback`"),
    }
}

fn do_apply() -> Result<()> {
    let text = std::fs::read_to_string(REQUEST)
        .with_context(|| format!("reading {REQUEST}"))?;
    let request: Request = serde_json::from_str(&text)
        .with_context(|| format!("parsing {REQUEST}"))?;
    let applied = apply::apply(Path::new("/"), Path::new(STAGING), &request)
        .context("applying the update")?;
    // Arms the rollback net only if the core itself was replaced, and clears
    // it otherwise — see `marker::arm`. A plugin gesture that armed it would
    // have any unrelated core crash loop inside the window undo that gesture.
    marker::arm(Path::new("/"), &applied, now_unix_s())
        .context("arming the pending marker")?;
    // Named individually rather than counted: this line is what an operator
    // reads in the journal to know what actually moved.
    println!(
        "placed: {:?}; removed: {:?}; core replaced: {}",
        applied.placed, applied.removed, applied.core_replaced
    );
    Ok(())
}

fn do_rollback() -> Result<()> {
    match rollback::rollback(Path::new("/"), now_unix_s()).context("rolling back")? {
        None => {
            // The ordinary case for a crash loop unrelated to an update. Exit
            // 0: this unit succeeded at deciding there was nothing to do, and
            // systemd is left to give up on the service as it would have.
            println!("no fresh update marker: nothing to roll back");
            Ok(())
        }
        Some(report) if report.restored.is_empty() => {
            println!("a fresh marker was found but nothing could be restored: {:?}", report.failed);
            Ok(())
        }
        Some(report) => {
            println!("restored: {:?}; failed: {:?}", report.restored, report.failed);
            // The service is in `failed` state with its start limit exhausted,
            // so it must be both cleared and started. Done here rather than
            // with `ExecStartPost=` so the decision and its condition live in
            // one place: restarting after a rollback that restored nothing
            // would fight systemd's give-up instead of respecting it.
            //
            // No polkit involved: this process is root. The core never reaches
            // this path.
            systemctl(&["reset-failed", "ritornello.service"]);
            systemctl(&["start", "--no-block", "ritornello.service"]);
            Ok(())
        }
    }
}

/// Best-effort, and reported rather than propagated: the rollback itself has
/// already succeeded on disk, and failing the unit here would hide that.
fn systemctl(args: &[&str]) {
    match std::process::Command::new("systemctl").args(args).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "systemctl {args:?} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("systemctl {args:?} could not be launched: {e}"),
    }
}
