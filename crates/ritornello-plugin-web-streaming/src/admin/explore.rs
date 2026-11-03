//! Where a download may go: the shares, and the folders on them.
//!
//! Every call that touches a share is spawned and runs under a deadline (see
//! `targets`): a mute share blocks a pool thread, never the admin loop. A
//! newer request wins: the answer of an older one is dropped.

use super::probe::Fault;
use super::{Environment, Shared};
use ritornello_files_mount::roots::Roots;
use ritornello_plugin_web_streaming::targets::{self, Target, TargetError};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;

/// Reading the declared roots, a file of our own machine.
const ROOTS_DEADLINE: Duration = Duration::from_secs(5);
/// Listing or making a folder on a share.
const SHARE_DEADLINE: Duration = Duration::from_secs(10);

/// The folder dialog's state.
#[derive(Default)]
pub struct Explore {
    /// Which request is current.
    seq: u64,
    busy: bool,
    /// The last listing: the folder, and the names under it.
    listing: Option<Listing>,
    error: Option<Fault>,
}

#[derive(Serialize, Clone)]
struct Listing {
    target: Target,
    names: Vec<String>,
}

impl Explore {
    pub fn view(&self) -> serde_json::Value {
        serde_json::json!({ "busy": self.busy, "listing": self.listing, "error": self.error })
    }
}

fn fault(e: &TargetError) -> Fault {
    let (code, detail) = match e {
        TargetError::UnknownRoot => ("unknown_root", None),
        TargetError::NotWritable => ("not_writable", None),
        TargetError::NotMounted => ("not_mounted", None),
        TargetError::Escapes => ("escapes", None),
        TargetError::Unresponsive => ("unresponsive", None),
        TargetError::Io(detail) => ("io", Some(detail.clone())),
    };
    Fault { code, detail }
}

/// The roots the files plugin declared and the mount table, read together.
async fn roots_and_mounts(env: &Arc<Environment>) -> Result<(Roots, String), TargetError> {
    let env = env.clone();
    targets::blocking(ROOTS_DEADLINE, move || {
        let roots = Roots::load(&env.roots_path).map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok((roots, (env.mounts)()))
    })
    .await
}

/// The shares a download may go to right now. No roots file (the files plugin
/// never configured one) means none; a file that cannot be read, or a read
/// that did not answer in time, is `None`: the caller keeps what it knew.
pub async fn targets_now(env: &Arc<Environment>) -> Option<Vec<String>> {
    if !env.roots_path.exists() {
        return Some(Vec::new());
    }
    match roots_and_mounts(env).await {
        Ok((roots, mounts)) => Some(targets::writable_targets(&roots, &mounts)),
        Err(_) => None,
    }
}

/// Starts a request: it is the current one until a newer replaces it.
fn begin(shared: &Shared) -> u64 {
    let mut e = shared.explore.lock().unwrap_or_else(|p| p.into_inner());
    e.seq += 1;
    e.busy = true;
    e.error = None;
    e.seq
}

fn finish(shared: &Shared, seq: u64, outcome: Result<Listing, TargetError>) {
    let mut e = shared.explore.lock().unwrap_or_else(|p| p.into_inner());
    if e.seq != seq {
        return;
    }
    e.busy = false;
    match outcome {
        Ok(listing) => e.listing = Some(listing),
        Err(err) => {
            e.listing = None;
            e.error = Some(fault(&err));
        }
    }
}

async fn list(env: &Arc<Environment>, target: Target) -> Result<Listing, TargetError> {
    let (roots, mounts) = roots_and_mounts(env).await?;
    let names = targets::list_dirs(&target, &roots, &mounts, SHARE_DEADLINE).await?;
    Ok(Listing { target, names })
}

pub fn list_dirs(shared: &Arc<Shared>, env: &Arc<Environment>, target: Target) {
    let seq = begin(shared);
    let (shared, env) = (shared.clone(), env.clone());
    tokio::spawn(async move {
        let outcome = list(&env, target).await;
        finish(&shared, seq, outcome);
    });
}

/// Makes the folder, then lists the same place again so the page sees it.
pub fn create_dir(shared: &Arc<Shared>, env: &Arc<Environment>, target: Target, name: String) {
    let seq = begin(shared);
    let (shared, env) = (shared.clone(), env.clone());
    tokio::spawn(async move {
        let made = async {
            let (roots, mounts) = roots_and_mounts(&env).await?;
            targets::create_dir(&target, &name, &roots, &mounts, SHARE_DEADLINE).await
        }
        .await;
        let outcome = match made {
            Ok(()) => list(&env, target).await,
            Err(e) => Err(e),
        };
        finish(&shared, seq, outcome);
    });
}
