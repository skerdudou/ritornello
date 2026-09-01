mod audio_output;
mod admin;
mod core;
mod cover;
mod i18n;
mod metadata;
mod placeholder;
mod player;
mod plugins;
mod register;
mod health;
mod state;
mod status;
mod system;
mod theme;
mod types;
mod web;

use crate::core::MetadataWiring;
use crate::metadata::PlayerState;
use crate::plugins::PluginManifest;
use crate::status::{AppState, LogBuffer, LogBufferWriter, PluginStatus, StatusState};
use crate::types::Event;
use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
// `PluginKind` comes from the shared protocol, not from the core: it is the
// plugin binary that announces it, and `plugins.rs` no longer needs to know it.
use ritornello_proto::{
    Announcement, SourcesCatalog, Enrichment, InputMessage, Known, NowPlaying, PluginKind,
};
use ritornello_plugin_sdk::{run_input_client, run_metadata_client, DisplayClient, SourceClient, SourceUpdate};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, RwLock};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[async_trait::async_trait]
impl core::Source for SourceClient {
    async fn request(&self, req: ritornello_proto::SourceReq) -> Result<ritornello_proto::SourceAction> {
        SourceClient::request(self, req).await
    }
}

/// Relays the state to **one** display, in its own task.
///
/// One task per display, not one task looping over N clients: that is what
/// keeps a slow display — busy console, screen blocked in I/O — from delaying
/// the others. Backpressure stays confined per socket, which was the argument
/// retained for not merging the per-kind sockets.
///
/// One function, not two copies: startup and hot wiring serve a display the
/// same way, and a display that arrives late must not be served by a slightly
/// different relay.
///
/// The current state is sent **first**, before any waiting: a hot-wired
/// display must show what is playing without waiting for the next state
/// change. Relying only on `changed()` worked by accident — `main`'s
/// `state_rx` is never advanced, so the clone inherited a stale version and
/// yielded immediately. A `borrow_and_update()` added one day in `main` would
/// have left a late display **black** until the next change, hence
/// indefinitely in standby where no tick is armed, and `publish_state` would
/// have fixed nothing since it is deduplicated.
///
/// A failed send **exits the loop**. On a socket whose peer is dead the error
/// is permanent (EPIPE): without the exit, the task outlived the plugin and
/// logged on every frame — one per second during playback, per zombie
/// display. Two manual restarts were enough to overwrite, in under four
/// minutes, the 500-line buffer that feeds the UI's error popup, and to drown
/// the real diagnosis in it. A display client whose write fails is unusable:
/// name it once, and leave.
///
/// **Two** receivers, and two kinds of frame: the player state, which changes
/// up to once per second, and the sources catalog, structural and rare. Two
/// separate channels rather than a widened payload: widening would republish
/// the state on every catalog change and vice versa, which deduplication by
/// equality could not catch — the two values would change together by
/// construction.
///
/// Both current values go out right away, before any waiting, for the same
/// reason as above: a hot-wired display must know the sources catalog without
/// waiting for it to change, and the catalog almost never changes.
/// **Covers only go to the displays that asked for them** in their
/// announcement (`Announcement::covers`), and **only when the cover
/// changes**: neither on every state frame — there is one per second during
/// playback — nor to the twenty-column display, which would receive megabytes
/// only to throw them away. The change is detected on the state's
/// `cover_href`, which is precisely the identity of the image (the cache key)
/// and not a timestamp: a cover that stays on screen therefore never goes out
/// again.
///
/// Materializing the bytes — the only moment the whole image exists in memory
/// inside the core — is **behind** this filter: a display that does not want
/// covers does not pay for the file read either.
/// How a relay reports that its peer no longer responds, and under which
/// number.
///
/// The two **always** travel together — a notice without its wiring number
/// would not be interpretable by the loop — and keeping them together keeps
/// the relay's signature readable.
#[derive(Clone)]
struct UnreachableNotice {
    /// Number of the wiring that launched this relay. See `wirings` in the
    /// main loop: it is what distinguishes the closing of a current socket
    /// from that of an incarnation already replaced.
    wiring: u64,
    tx: mpsc::Sender<(String, u64)>,
}

fn display_relay(
    name: String,
    client: Arc<DisplayClient>,
    wants_covers: bool,
    covers: Arc<cover::CoverCache>,
    mut state_rx: watch::Receiver<PlayerState>,
    mut catalog_rx: watch::Receiver<SourcesCatalog>,
    notice: UnreachableNotice,
) {
    tokio::spawn(async move {
        /// Number of attempts granted to a same `cover_href` before giving
        /// up on it for good.
        ///
        /// **The exact compromise between two symmetric defects.** Marking the
        /// attempt as done before actually doing it sacrificed the cover for
        /// **the whole track** on a single exceeded timeout: a sleeping SMB
        /// share takes a handful of seconds to answer the first access and
        /// then answers, so the only attempt ever granted was precisely the
        /// one that could not succeed. Conversely, retrying without a bound
        /// would re-read the file **once per second** — the cadence of state
        /// frames during playback — for an image whose absence may be
        /// permanent (a 404 from the Cover Art Archive, a file over the cap).
        /// Three attempts cover the wake-up of a share without installing a
        /// re-read loop.
        const COVER_ATTEMPTS: u8 = 3;

        /// What the relay remembers of its cover attempts.
        ///
        /// Two fields, not one, because "pushed" and "attempted without
        /// success" are two different facts: conflating them is what lost the
        /// cover of a whole track on a single failure.
        #[derive(Default)]
        struct CoverTracking {
            /// The `cover_href` of the last cover **actually written** to the
            /// socket. A state frame repeating this href triggers nothing:
            /// this guard is what avoids pushing megabytes every second of
            /// playback.
            pushed: Option<String>,
            /// The failing `cover_href` and the number of attempts already
            /// consumed. Reset as soon as another href appears: the budget is
            /// per cover, not per relay.
            failures: Option<(String, u8)>,
        }

        /// Pushes the cover that `href` designates, if it changed since the
        /// last **successful** send. Returns `Err` like a state send, so that
        /// the loop's error handling is the same: a dead socket must exit the
        /// loop, whatever the kind of frame that discovered it.
        ///
        /// A cover that is missing, unreadable or too big is **not** a send
        /// error: nothing goes out, the loop continues, and the failure is
        /// counted separately from success (see `CoverTracking` and
        /// `COVER_ATTEMPTS`). A transient failure is thus retried on the next
        /// state frame, until the budget runs out — a permanent failure only
        /// costs three reads per track, not one per second.
        ///
        /// **Encodes nothing itself**: `covers.line` builds the frame and
        /// returns it behind an `Arc`; this relay only writes that buffer
        /// (`DisplayClient::send_cover_line`), with no copy or re-encoding.
        async fn push_cover(
            client: &DisplayClient,
            covers: &cover::CoverCache,
            tracking: &mut CoverTracking,
            href: Option<&str>,
        ) -> anyhow::Result<()> {
            // `None` (nothing is playing anymore, or the cover was removed)
            // emits no frame: the display learns it from the missing
            // `cover_href` in the state it just received. Inventing an empty
            // cover frame would make a zero-byte image exist in the protocol.
            // Both memories are cleared: the next cover, even identical to
            // the previous one, describes a new track.
            let Some(href) = href else {
                tracking.pushed = None;
                tracking.failures = None;
                return Ok(());
            };
            if tracking.pushed.as_deref() == Some(href) {
                return Ok(());
            }
            // Budget consumed for *this* href: attempt nothing more. A
            // different href wipes the slate, which the `match` below does.
            let attempts = match &tracking.failures {
                Some((h, n)) if h == href => {
                    if *n >= COVER_ATTEMPTS {
                        return Ok(());
                    }
                    *n
                }
                _ => 0,
            };
            let Some(key) = href.strip_prefix(cover::HREF_PREFIX) else {
                // A href without our prefix will never become valid: consume
                // the whole budget at once rather than retrying three times a
                // string that cannot change.
                tracing::debug!("cover href {href} has no key, nothing pushed");
                tracking.failures = Some((href.to_owned(), COVER_ATTEMPTS));
                return Ok(());
            };
            let Some(line) = covers.line(key, href).await else {
                // Already logged by `bytes` with its reason. Counted as a
                // failure, hence retried on the next frame: this is where the
                // sleeping share scenario plays out.
                tracking.failures = Some((href.to_owned(), attempts + 1));
                return Ok(());
            };
            client.send_cover_line(&line).await?;
            tracking.pushed = Some(href.to_owned());
            tracking.failures = None;
            Ok(())
        }

        // **Two loop exits that must not be confused.** A failed send means
        // the display is no longer reachable, and that is what must become
        // visible on the status page. A closed `watch::Receiver` means *the
        // core* is shutting down — its senders are gone — which says nothing
        // about the plugin and must therefore report nothing: marking every
        // display disconnected during core shutdown would paint a failure
        // over a normal stop.
        //
        // Hence the labeled block: the four "peer unreachable" paths return
        // `true`, the loop's natural end returns `false`, and the notice
        // leaves from a single place — below the block — instead of being
        // copied four times.
        let unreachable_detected = 'alive: {
            let state = state_rx.borrow_and_update().clone();
            let cat = catalog_rx.borrow_and_update().clone();
            if let Err(e) = client.send(&state).await {
                tracing::warn!("display plugin {name} relay stopped: {e}");
                break 'alive true;
            }
            if let Err(e) = client.send_catalog(&cat).await {
                tracing::warn!("display plugin {name} relay stopped: {e}");
                break 'alive true;
            }
            // The current cover goes out right away, like the state and the
            // sources catalog and for the same reason: a hot-wired display
            // must show what is playing without waiting for the next track
            // change.
            let mut cover_tracking = CoverTracking::default();
            if wants_covers
                && let Err(e) = push_cover(
                    &client,
                    &covers,
                    &mut cover_tracking,
                    state.track.cover_href.as_deref(),
                )
                .await
            {
                tracing::warn!("display plugin {name} relay stopped: {e}");
                break 'alive true;
            }
            loop {
                let send_result = tokio::select! {
                    r = state_rx.changed() => match r {
                        Ok(()) => {
                            let e = state_rx.borrow_and_update().clone();
                            let send_result = client.send(&e).await;
                            // State first, cover second: this way the display
                            // knows the `cover_href` before receiving the
                            // bytes that claim it.
                            match (send_result, wants_covers) {
                                (Ok(()), true) => {
                                    push_cover(
                                        &client,
                                        &covers,
                                        &mut cover_tracking,
                                        e.track.cover_href.as_deref(),
                                    )
                                    .await
                                }
                                (other, _) => other,
                            }
                        }
                        // The core is stopping, not the plugin: exit without
                        // reporting anything.
                        Err(_) => break,
                    },
                    r = catalog_rx.changed() => match r {
                        Ok(()) => {
                            let c = catalog_rx.borrow_and_update().clone();
                            client.send_catalog(&c).await
                        }
                        Err(_) => break,
                    },
                };
                if let Err(e) = send_result {
                    tracing::warn!("display plugin {name} relay stopped: {e}");
                    break 'alive true;
                }
            }
            false
        };
        if unreachable_detected {
            // `let _`: the core loop may have vanished in the meantime, and
            // its departure is not an incident to log here.
            let _ = notice.tx.send((name, notice.wiring)).await;
        }
    });
}

/// What a supervision future returns: name, generation, exit status, and
/// whether the death had been requested.
///
/// Boxed, hence **named**: startup and re-enabling both push into the same
/// `FuturesUnordered`, and two functions each returning an `impl Future`
/// return two distinct opaque types, which no collection accepts together.
/// One allocation per plugin launch, eight at startup.
type PluginExit =
    futures::future::BoxFuture<'static, (String, u64, std::io::Result<std::process::ExitStatus>, bool)>;

/// Watches a plugin until its death, whether suffered or requested.
///
/// A function, not an `async move` copied at the two places that launch a
/// plugin (startup and re-enabling): it is the only place that knows that
/// `kill_rx` means "kill it".
///
/// The `select!` does nothing but **choose** — none of its arms touches
/// `child` — so that the mutable borrow of the futures is released before the
/// `terminate` that follows. Calling `wait()` again afterwards is safe: tokio
/// remembers the status of an already reaped process.
///
/// Returns `(name, generation, status, requested)`. The generation is what
/// lets the main loop ignore the death of a previous incarnation, arriving
/// after the next one was re-enabled.
fn supervise(
    name: String,
    generation: u64,
    child: tokio::process::Child,
    kill_rx: tokio::sync::oneshot::Receiver<()>,
) -> PluginExit {
    use futures::FutureExt;
    async move {
        let mut child = child;
        // `r.is_ok()` and not `_`: only an actual send means "requested". A
        // `kill_rx` whose sender was dropped also returns `Err`, which
        // happens when two `plugins.toml` entries share the same `name` —
        // tolerated on purpose by the manifest loader — and the second
        // `kill_triggers.insert` overwrites the first one's `kill_tx`:
        // without this check, the first one's natural death would be taken
        // for a requested shutdown, `terminate` would send `SIGTERM` to a
        // healthy process, and `mark_plugin_disconnected` would never be
        // called.
        let requested = tokio::select! {
            r = kill_rx => r.is_ok(),
            _ = child.wait() => false,
        };
        let status = if requested {
            plugins::terminate(&mut child, plugins::SHUTDOWN_GRACE).await
        } else {
            child.wait().await
        };
        (name, generation, status, requested)
    }
    .boxed()
}

/// The children that hot wiring must hold to replay, after startup, what the
/// initial wiring loop does with its local variables.
/// How long a freshly launched plugin keeps the benefit of the doubt before
/// being reported "stalled".
///
/// Strictly longer than `register::READ_TIMEOUT` (5 s), and this is not a
/// comfort margin: an already accepted connection that is **in the middle
/// of** writing its announcement line has those five seconds, and reporting
/// it stalled during that time would be contradicting ourselves. Ten seconds
/// therefore cover loading the binary from an SD card, binding its sockets
/// and writing its announcement, with room to spare.
///
/// Beyond that, "stalled" becomes the right word again: the plugin is
/// launched, alive, and silent — a diagnosis, not a wait.
const STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The startup deadline has passed: should this plugin be downgraded to
/// "stalled"?
///
/// **Only if its line still says "starting".** Between the launch and the
/// deadline, the plugin may have announced itself (its line then describes
/// its kinds), died (it says "disconnected"), or been disabled from the UI
/// (it says "disabled"). In all three cases, overwriting would replace true
/// information with false — and the false one would be the most misleading of
/// the four, since it accuses a plugin that is doing fine.
///
/// Re-read the state rather than keep a registry to purge on every
/// transition: that is the lesson of `kill_triggers`, whose three purge sites
/// were already one too many.
fn should_downgrade(statuses: &StatusState, name: &str) -> bool {
    statuses.plugins.iter().any(|l| l.name == name && l.starting)
}

struct HotPlugChildren {
    sockets_dir: PathBuf,
    /// Manifest names in file order: the authority on accepted names, and
    /// the arbitration priority of the `metadata` plugins.
    manifest_order: Vec<String>,
    source_update_tx: mpsc::Sender<(String, SourceUpdate)>,
    cmd_tx: mpsc::Sender<InputMessage>,
    enrich_tx: mpsc::Sender<(String, Enrichment)>,
    now_playing_rx: watch::Receiver<NowPlaying>,
    state_rx: watch::Receiver<PlayerState>,
    /// The second receiver of `display_relay`: a hot-wired display must be
    /// served by a relay identical to the startup one.
    catalog_rx: watch::Receiver<SourcesCatalog>,
    /// **The same** `Arc` as the core's and the HTTP `AppState`'s (see
    /// `assemble_covers_and_core`): a hot-wired display must read the covers
    /// the core has already fetched, not a fresh, empty cache.
    covers: Arc<cover::CoverCache>,
    status_state: Arc<RwLock<StatusState>>,
    admin_backends: admin::AdminBackends,
    /// **The same** `Arc` as the HTTP `AppState`'s, for the same reason as
    /// `covers`: purging a fresh, empty cache would invalidate nothing of
    /// what the routes actually serve.
    admin_assets: Arc<admin::AssetCache>,
    /// How a closing socket makes itself known to the main loop.
    ///
    /// **This is the only path through which the death of an unsupervised
    /// plugin becomes visible.** A manually restarted plugin escapes
    /// `plugin_waits` — the core is not its parent, it will never see its
    /// exit code — but its sockets are indeed ours: their closing is a fact
    /// the core already observes, and used to merely log. The page therefore
    /// kept showing it connected, indefinitely.
    ///
    /// Carries `(name, wiring generation)`: see `wirings` in the loop, which
    /// says why the number is indispensable.
    unreachable_tx: mpsc::Sender<(String, u64)>,
}

/// Wires a plugin that announces itself **after** the startup rendezvous.
///
/// Each kind follows the shape of the initial wiring. Two differences,
/// imposed by the fact that the core is already running: the source goes
/// through `Core::add_source`, and the `metadata` arbitration order is
/// **recomputed in full** from the manifest instead of being appended to.
///
/// A re-announcement of an already wired plugin follows the same path: we
/// rewire. `add_source` replaces the client, and the previous relays exit on
/// their own at their first failed send, their socket having disappeared —
/// that is what `display_relay`'s loop exit guarantees, without which they
/// would pile up on every restart, logging on every frame.
async fn hotplug<P: player::Player>(
    announcement: Announcement,
    children: &HotPlugChildren,
    core: &mut core::Core<P>,
    gathered: &mut register::Gathered,
    kill_triggers: &HashMap<String, tokio::sync::oneshot::Sender<()>>,
    non_supervised: &mut HashSet<String>,
    // Number of this particular wiring, assigned by the loop. Copied into
    // every socket task launched here, so that the closing of a socket from a
    // previous incarnation is recognized as such and ignored. A `///` is
    // rejected on a parameter, hence the ordinary comment.
    wiring: u64,
) {
    let name = announcement.name.clone();
    // The manifest is the authority on names, hot as at the rendezvous: an
    // announcement carrying another one is named then discarded, never wired.
    if !children.manifest_order.contains(&name) {
        tracing::warn!("late announcement from unknown plugin {name}, ignored");
        return;
    }
    tracing::info!(
        "{name} announced late {:?} (admin: {}), wiring it now",
        announcement.kinds,
        announcement.admin
    );
    // The core does not hold this plugin's `child`: `plugin_waits` will see
    // neither its next exit code nor its `mark_plugin_disconnected`. The
    // `connected: true` we are about to set will be true the instant we set
    // it, and will never again correct itself on its own.
    //
    // The condition used to be `gathered.dead.contains(&name)`, twice too
    // narrow: `dead` is only filled by the startup rendezvous, so it missed
    // the deaths observed by the main loop **and** the processes the core
    // never launched. `kill_triggers` answers exactly the question asked,
    // since it only means "launched by us and not yet reaped" — and an
    // announcement proves its sender is alive.
    //
    // The name is **retained**, and that is the whole gain: the `retain`
    // calls just above erased `dead` right after the `warn`, so the only
    // trace disappeared the instant of the rewiring. A defect the program was
    // aware of and whose evidence it destroyed.
    if liveness(&name, kill_triggers, non_supervised) != Liveness::Supervised {
        // This warning used to say "its next exit will go unnoticed, and it
        // can no longer be enabled or disabled from the UI until the core
        // restarts". **Both halves have become false**: the closing of its
        // sockets is now observed, which makes its death visible on the page
        // *and* takes it out of `unsupervised`, hence manageable again. What
        // remains true — and what this `warn!` now says — is narrower: while
        // it lives, the core cannot stop it, for lack of holding its `child`.
        tracing::warn!(
            "wiring {name}, which is alive but not supervised by the core: it cannot be stopped from the admin UI while it lives, though the core will notice when its sockets close"
        );
        non_supervised.insert(name.clone());
    }

    // The gathering and the arbitration order are updated **before**
    // launching anything. The order first because the `metadata` client
    // launched below can send an enrichment from its very first frame, and
    // the core rejects an enrichment "from an undeclared metadata plugin":
    // today the main loop cannot drain `enrich_rx` during this arm, but
    // relying on that would make correctness depend on an implicit
    // serialization that a refactor — this wiring moved into a task — would
    // silently break.
    //
    // The list is recomputed **in full** from the manifest, never appended
    // to: the priority is that of `plugins.toml`, and a late `metadata`
    // plugin takes its file position in it. The ordering logic stays in
    // `register::metadata_order`, a single place.
    //
    // The two `retain` calls keep `Gathered` consistent: a stalled plugin
    // that just spoke is no longer stalled, a dead one that comes back is no
    // longer dead. Nothing reads these two lists after startup — the status
    // page comes from `status_state` — but the structure is the memory of
    // what the core knows about the plugins, and a name belongs to only one
    // of the three collections. Two lines so it does not lie to the next
    // reader.
    gathered.stalled.retain(|n| n != &name);
    gathered.dead.retain(|n| n != &name);
    gathered.announcements.insert(name.clone(), announcement.clone());
    core.set_metadata_order(register::metadata_order(&children.manifest_order, gathered));

    let prefix = children.sockets_dir.join(&name);
    // The status lines are composed separately then **substituted** as a
    // block: see `status::replace_plugin_lines`.
    let mut lines: Vec<PluginStatus> = Vec::new();

    for kind in &announcement.kinds {
        let socket = ritornello_plugin_sdk::socket_kind(&prefix, *kind);
        match kind {
            PluginKind::Source => {
                // `connect_with_close` and not `connect`: the SDK's read task
                // used to end on EOF with a log line, without telling anyone.
                // A `oneshot` relayed to the loop, because the SDK must know
                // nothing of the core's bookkeeping.
                let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
                let unreachable = children.unreachable_tx.clone();
                let closed_name = name.clone();
                tokio::spawn(async move {
                    // `Err` = the client was dropped without the socket
                    // closing, which only happens when the client is
                    // replaced: nothing to report then, the replacement
                    // speaks for itself.
                    if closed_rx.await.is_ok() {
                        let _ = unreachable.send((closed_name, wiring)).await;
                    }
                });
                match SourceClient::connect_with_close(
                    &socket,
                    name.clone(),
                    children.source_update_tx.clone(),
                    Some(closed_tx),
                )
                    .await
                {
                    Ok(client) => {
                        // Cloned before `hotplug_source` takes it: the
                        // catalog request below addresses the same client.
                        let catalog_client = client.clone();
                        // `hotplug_source` does the three things that
                        // `add_source` alone does not: the current locale
                        // (otherwise a manually restarted `cd` on a device in
                        // French comes back displaying `NO DISC`), the wake-up
                        // if it is the core's **first** source (otherwise it
                        // is active and silent), and publishing the state.
                        //
                        // First wiring or rewiring: that is precisely the
                        // event sought by whoever is debugging a flapping
                        // plugin, and the boolean knows it.
                        match core.hotplug_source(name.clone(), client).await {
                            Ok(true) => {
                                tracing::info!("{name} source client replaced (plugin rewired)")
                            }
                            Ok(false) => tracing::info!("{name} source wired for the first time"),
                            // The source **is** wired: only its wake-up
                            // failed (mpv, or the source itself). The status
                            // line therefore says `connected: true`, and a
                            // remote-control command will go through the same
                            // path again.
                            Err(e) => tracing::warn!("{name} source wired, but waking it failed: {e:#}"),
                        }
                        // Its catalog, as at startup and for the same reason:
                        // a detached task, the correlated reply (`Noop`)
                        // teaching nothing — the presets arrive through the
                        // update channel. Without this, a source announced
                        // late entered the catalog with a **permanently
                        // empty** list, nobody ever asking again; and a
                        // plugin rewired after its configuration changed
                        // while it was dead left the core on the old list.
                        //
                        // Detached, then: this arm runs in the main loop, and
                        // awaiting it would add the source protocol's 5 s to
                        // it — the loop would stop handling a remote-control
                        // key during that time.
                        let catalog_name = name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = catalog_client
                                .request(ritornello_proto::SourceReq::ListPresets)
                                .await
                            {
                                tracing::debug!("list_presets for {catalog_name}: {e}");
                            }
                        });
                        lines.push(PluginStatus::kind(&name, "source", true, announcement.admin));
                    }
                    Err(e) => {
                        tracing::warn!("plugin {name} source unavailable: {e}");
                        lines.push(PluginStatus::kind(&name, "source", false, announcement.admin));
                    }
                }
            }
            PluginKind::Display => match DisplayClient::connect(&socket).await {
                Ok(client) => {
                    display_relay(
                        name.clone(),
                        client,
                        // The flag **from this plugin's own announcement**, never
                        // a default value: it is the binary that said whether it
                        // wanted the bytes (see `Announcement::covers`).
                        announcement.covers,
                        children.covers.clone(),
                        children.state_rx.clone(),
                        children.catalog_rx.clone(),
                        UnreachableNotice { wiring, tx: children.unreachable_tx.clone() },
                    );
                    lines.push(PluginStatus::kind(&name, "display", true, announcement.admin));
                }
                Err(e) => {
                    tracing::warn!("display plugin {name} unavailable: {e}");
                    lines.push(PluginStatus::kind(&name, "display", false, announcement.admin));
                }
            },
            PluginKind::Input => {
                let tx = children.cmd_tx.clone();
                let socket_for_task = socket.clone();
                let task_name = name.clone();
                let unreachable = children.unreachable_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = run_input_client(&socket_for_task, tx).await {
                        tracing::warn!("input plugin {task_name} disconnected: {e}");
                    }
                    // `run_input_client` **never** returns `Ok`: it exits with an
                    // error both on EOF and on the core's channel closing. The
                    // second case reports into the void — the loop is gone, its
                    // receiver with it — so it does not need to be distinguished.
                    let _ = unreachable.send((task_name, wiring)).await;
                });
                lines.push(PluginStatus::kind(&name, "input", true, announcement.admin));
            }
            PluginKind::Metadata => {
                let tx = children.enrich_tx.clone();
                let np_rx = children.now_playing_rx.clone();
                let socket_for_task = socket.clone();
                let task_name = name.clone();
                let unreachable = children.unreachable_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        run_metadata_client(&socket_for_task, task_name.clone(), tx, np_rx).await
                    {
                        tracing::warn!("metadata plugin {task_name} disconnected: {e}");
                    }
                    let _ = unreachable.send((task_name, wiring)).await;
                });
                lines.push(PluginStatus::kind(&name, "metadata", true, announcement.admin));
            }
        }
    }

    // The old backend is removed **before** the connection attempt, whatever
    // the plugin announces. A backend surviving a re-announcement would point
    // at a vanished socket: `/api/admin/<name>` would render an error only
    // after the request's timeout budget, where a plain 404 says right away
    // that there is nothing at this address.
    // Assets go with the backend: a re-announcement is the end of one process
    // followed by the start of another, and the new one may carry a rebuilt
    // `ui.js`. Keeping them served the old one until the core restarted.
    admin::forget_page(&children.admin_backends, &children.admin_assets, &name).await;
    let mut admin_connected = false;
    if announcement.admin {
        let path = ritornello_plugin_sdk::admin_socket(&prefix);
        match ritornello_plugin_sdk::AdminClient::connect(&path).await {
            Ok(client) => {
                children.admin_backends.write().await.insert(name.clone(), client);
                admin_connected = true;
            }
            Err(e) => tracing::warn!("admin plugin {name} unreachable: {e}"),
        }
    }
    // Same rule as at startup: the flag follows what was actually
    // **connected**, not what the plugin announced — an announcement with
    // `admin: true` whose `connect` fails must not leave the UI pointing at a
    // page that answers 404. Reasserted on every line rather than fixed only
    // in the failure case: one truth, written once.
    for line in lines.iter_mut() {
        line.admin = admin_connected;
    }

    // **Replace, never append**: a plugin that re-announces itself would
    // otherwise accumulate one more line at every restart. Replacing with a
    // fresh list keeps the plugin visible with an unknown kind, see
    // `status::replace_plugin_lines`: an announcement with `kinds: []` must
    // report a badly built plugin, not make it disappear from the page.
    {
        let mut statuses = children.status_state.write().await;
        status::replace_plugin_lines(&mut statuses, &name, lines, admin_connected);
    }
}

/// Builds the `Core` and the HTTP `AppState` with **the same**
/// `Arc<CoverCache>` handed to both: this function is what builds that cache,
/// never `main` directly, and it is this function — not a re-reading of
/// `main`'s code — that a test calls to check the sharing via `Arc::ptr_eq`
/// (see `core::tests::the_core_and_the_appstate_really_share_the_same_arc`).
/// A regression where `main` rebuilt a second cache for one of the two would
/// break that equality on the very first call, not only when reviewing the
/// diff.
///
/// `skeleton.covers` is ignored: it exists only so the caller does not have
/// to build the `AppState` in two pieces — all its other fields pass through
/// unchanged.
pub(crate) fn assemble_covers_and_core<P: player::Player>(
    player: P,
    wiring: core::Wiring,
    cover_tx: mpsc::Sender<(String, bool)>,
    extraction_tx: mpsc::Sender<(String, Option<ritornello_proto::CoverRef>)>,
    skeleton: AppState,
) -> (AppState, core::Core<P>) {
    let covers = Arc::new(cover::CoverCache::new());
    let core_engine = core::Core::new(player, wiring, covers.clone(), cover_tx, extraction_tx);
    let app_state = AppState { covers, ..skeleton };
    (app_state, core_engine)
}

/// Turns off a plugin: we request its death, then remove **everything** the
/// core held of it.
///
/// The unwiring happens here, not on hearing back about its death: the page
/// is waiting for a reply, and it must describe an already-true state. The
/// process itself dies at its own pace — at worst two seconds later,
/// `SIGKILL` in hand — and its exit will only produce a log line from then on.
///
/// Displays and inputs have nothing explicit to remove: their relays exit the
/// loop on the first failed send or on EOF, which the socket's death causes.
///
/// For a display, this holds for its **two** channels: `display_relay` holds
/// a state receiver and a sources-catalog receiver, and both arms of its
/// `select!` funnel their send result into the same error handling — whichever
/// wakes up first after the socket dies, the task exits.
///
/// The sources-catalog channel adds a chance to notice it earlier, **but**
/// what the core knows of a plugin, once its **two** registries are crossed.
///
/// `kill_triggers` only means "launched by us and not yet reaped" — its own
/// comment says so. Yet the toggle's two guards used it as the oracle of
/// liveness, and therefore missed exactly the case they were written for: a
/// **living** plugin that the core does not supervise. Turning it on relaunched
/// a second process that stole the first one's socket prefix; turning it off
/// unwired everything, set `disabled` and returned `true`, so the UI showed
/// "inactive" while the process kept running with its sockets.
///
/// `announcements` could not serve as the oracle either, and that is
/// counterintuitive: the main loop's death branch **does not purge it**
/// (only `hot_unplug` does). A crashed plugin keeps its announcement there,
/// so relying on it would have made the shutdown of a crashed plugin get
/// *refused* — the most common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// No known process: neither launched by the core, nor announced outside
    /// its supervision.
    Off,
    /// Launched by the core and not yet reaped: it holds what it takes to
    /// stop it.
    Supervised,
    /// Alive and **out of reach**: it has spoken, the core does not hold its
    /// `child`. Manually relaunched, a system supervisor, or `child.wait()`
    /// already consumed by the startup rendezvous.
    ///
    /// **This state is no longer permanent.** The core will never see such a
    /// process's exit code, but it does see its sockets close, and infers
    /// from that it is no longer reachable: the name then leaves
    /// `non_supervised` and becomes `Off` again, hence turn-on-able. See the
    /// `unreachable_rx` arm of the main loop.
    OutOfReach,
}

/// Crosses the two registries for a name.
///
/// `Supervised` wins when both answer: what the core can stop should take
/// priority over what it can only observe. The conjunction is
/// **unreachable**, but no longer for the reason once written here.
///
/// The old argument was that a name classified `OutOfReach` can never come
/// back into `kill_triggers`, the turn-on guard refusing to launch a process
/// for it — hence never in both tables. That is no longer true: since the
/// closing of sockets is observed, a name **leaves** `non_supervised` once its
/// process stops being reachable, and it can be relaunched afterwards.
///
/// The conclusion still holds, though, by a shorter path: leaving
/// `non_supervised` always precedes the turn-on that would register it in
/// `kill_triggers` — that is what makes that turn-on possible. The two
/// memberships therefore stay exclusive at every instant. The order is
/// written anyway: it makes the function total without depending on that
/// argument.
fn liveness(
    name: &str,
    kill_triggers: &HashMap<String, tokio::sync::oneshot::Sender<()>>,
    non_supervised: &HashSet<String>,
) -> Liveness {
    if kill_triggers.contains_key(name) {
        Liveness::Supervised
    } else if non_supervised.contains(name) {
        Liveness::OutOfReach
    } else {
        Liveness::Off
    }
}

/// only for a plugin that owned a source**: it is its removal that republishes
/// the sources catalog. For a purely-display plugin — the console, and the
/// MPD plugin itself — `remove_source` returns `Ok(false)`, nothing is
/// republished, and in standby (where no state tick is armed) the dead relay
/// stays parked until the next wake-up. Without consequence: it consumes
/// nothing while waiting, and it will exit on the first send. But the status
/// line says "disconnected" before the task has observed anything, and that
/// is intentional — the acknowledgment describes an already-true state, not
/// the instant the task learns of it.
async fn hot_unplug<P: player::Player>(
    name: &str,
    children: &HotPlugChildren,
    core: &mut core::Core<P>,
    gathered: &mut register::Gathered,
    kill_triggers: &mut HashMap<String, tokio::sync::oneshot::Sender<()>>,
    non_supervised: &HashSet<String>,
) -> bool {
    // Nothing to stop it with: unwiring anyway and setting `disabled` would
    // show "inactive" for a plugin still running with its port and sockets.
    // Refusing is the only true answer, and the log names the remedy.
    if liveness(name, kill_triggers, non_supervised) == Liveness::OutOfReach {
        tracing::warn!(
            "refusing to disable {name}: it is alive but the core does not own its process, so it cannot be stopped — kill it yourself, or restart the core to let it take ownership again"
        );
        return false;
    }
    tracing::info!("disabling plugin {name}: killing it and unwiring everything it served");
    if let Some(tx) = kill_triggers.remove(name) {
        // The receiver lives in the supervision future: a send error would
        // mean it is already finished, hence the process is already dead.
        // Nothing to catch up on.
        let _ = tx.send(());
    }
    if let Err(e) = core.remove_source(name).await {
        tracing::warn!("unwiring source {name}: {e:#}");
    }
    // The name leaves the gathering, then the arbitration order is recomputed
    // in **full** from the manifest — the same path any late announcement
    // already takes, and the only way a relaunched plugin regains its file
    // priority.
    gathered.announcements.remove(name);
    gathered.stalled.retain(|n| n != name);
    gathered.dead.retain(|n| n != name);
    core.set_metadata_order(register::metadata_order(&children.manifest_order, gathered));
    // Removed, otherwise `/plugins/<name>/` would wait out the request's
    // timeout budget before ending in error, where a plain 404 says right
    // away that there is nothing at this address.
    admin::forget_page(&children.admin_backends, &children.admin_assets, name).await;
    let mut statuses = children.status_state.write().await;
    status::replace_plugin_lines(&mut statuses, name, vec![PluginStatus::disabled(name)], false);
    statuses.active_source = core.active_source().to_string();
    true
}

/// Turns a plugin back on: we restart its binary, and that is all.
///
/// The wiring is **not** done here: the plugin will announce itself on the
/// registration socket, which the core keeps open for the life of the
/// process, and `hotplug` will do the rest. This is the same path a manually
/// relaunched plugin already takes, already proven.
///
/// This is also what asks a relaunched source for its presets again, and
/// there is nothing more to do here: `hot_unplug` emptied its entry in
/// `presets_by_source` (see `Core::remove_source`), so the sources catalog
/// would give it back empty — but `hotplug` detaches a `ListPresets` on
/// **every** source wiring, first or not, and the list comes back through the
/// update channel. A plugin whose configuration changed while it was off is
/// therefore reread, never inherited.
///
/// Until then, the line says "stalled": launched, not yet announced. That is
/// exactly what the word means, and the page does not need a fourth state for
/// a handful of seconds.
///
/// Returns `false` if the binary could not be launched — the `exec` path
/// changed, the file is no longer executable. The precise cause goes to the
/// log, which the UI already shows in its error popup.
async fn relaunch(
    name: &str,
    exec: &str,
    generation: u64,
    children: &HotPlugChildren,
    register_path: &Path,
    locale: Option<&str>,
    kill_triggers: &mut HashMap<String, tokio::sync::oneshot::Sender<()>>,
) -> Option<PluginExit> {
    let prefix = children.sockets_dir.join(name);
    match plugins::spawn(exec, register_path, name, &prefix, locale) {
        Ok(child) => {
            tracing::info!("plugin {name} re-enabled, launched again");
            let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
            kill_triggers.insert(name.to_string(), kill_tx);
            let mut statuses = children.status_state.write().await;
            // "Starting", not "stalled": the binary was just launched, it has
            // not yet had time to bind its sockets. It is the core loop that
            // downgrades it to "stalled" after `STARTUP_TIMEOUT`, if nothing
            // came in.
            status::replace_plugin_lines(&mut statuses, name, vec![PluginStatus::startup(name)], false);
            Some(supervise(name.to_string(), generation, child, kill_rx))
        }
        Err(e) => {
            tracing::warn!("failed to launch plugin {name}: {e:#}");
            None
        }
    }
}

/// True for a frame the core accepts to write to the log.
///
/// **It filters out only one thing: `lofty`'s chatter below error level.**
/// `player::mpv::embedded_cover` opens the file being played with `lofty` to
/// extract a cover, hence **on every track change**, and `lofty` emits a
/// `WARN` there for every MP3 without a Xing header — "MPEG: Using bitrate to
/// estimate duration". This is not an incident: it is the normal estimation
/// method for this format, it calls for no action, and it repeats per track.
///
/// The cost is twofold, and that is what makes it harmful rather than merely
/// noisy: it drowns the log, **and** it pushes real errors out of the
/// "recent errors" buffer, which only retains `WARN` and above.
///
/// The same filter exists in the `files` plugin, which probes durations with
/// the same library. Two copies of a three-line rule, rather than a shared
/// crate for the occasion — but if a third one appears, that is the sign the
/// crate is needed.
///
/// `lofty` keeps its `ERROR`s: a frame the library judges faulty remains
/// information.
fn frame_to_log(metadata: &tracing::Metadata<'_>) -> bool {
    // `>` and not `<`: in `tracing`, the order of levels follows verbosity,
    // so `ERROR` is the **smallest**. "More verbose than error" is indeed
    // written `> Level::ERROR`.
    !(metadata.target().starts_with("lofty") && *metadata.level() > tracing::Level::ERROR)
}

#[tokio::main]
async fn main() -> Result<()> {
    // 500 and not 50: the UI now has a popup listing the whole buffer behind
    // a filter, and 50 lines would not reach any further back than the card
    // that already shows the latest ones. 500 lines weigh a few dozen KB,
    // read once per popup opening — not on every poll.
    let log_buffer = Arc::new(LogBuffer::new(500));
    let log_buffer_for_writer = log_buffer.clone();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(move || LogBufferWriter(log_buffer_for_writer.clone()))
                .with_filter(LevelFilter::WARN),
        )
        // Applied on the registry, not on a single layer: both layers above
        // must ignore it, the terminal as much as the buffer.
        .with(tracing_subscriber::filter::filter_fn(frame_to_log))
        .init();

    // Sweeps the temp files from a previous run before anything can recreate
    // them: see `cover::purge_temp_files` for the reason (accumulation, not
    // freshness — that is already guaranteed elsewhere).
    cover::purge_temp_files();

    let plugins_path = PathBuf::from(env_or("RITORNELLO_PLUGINS", "/etc/ritornello/plugins.toml"));
    let state_path = PathBuf::from(env_or("RITORNELLO_STATE", "/var/lib/ritornello/state.json"));
    let mpv_socket = PathBuf::from(env_or("RITORNELLO_MPV_SOCKET", "/run/ritornello/mpv.sock"));
    let mpv_bin = env_or("RITORNELLO_MPV_BIN", "mpv");
    let cd_dev = env_or("RITORNELLO_CD_DEV", "/dev/sr0");
    let http_addr = env_or("RITORNELLO_HTTP", "0.0.0.0:8080");
    let runtime_dir = env_or("RITORNELLO_RUNTIME_DIR", "/run/ritornello");

    let manifest = PluginManifest::load(&plugins_path)
        .with_context(|| format!("loading {}", plugins_path.display()))?;
    let persisted = state::load(&state_path);

    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let catalog = Arc::new(RwLock::new(ritornello_i18n::Catalog::load(
        "core",
        persisted.locale.as_deref().unwrap_or("en"),
        &locales_root,
        i18n::EN,
    )));

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<InputMessage>(32);
    // mpv events: an `mpsc`, not a `broadcast` — there is only one consumer
    // (the loop below), and `broadcast`'s lossy semantics (`Lagged`) could
    // drop a `PlaybackIdle` that mpv, which only signals transitions, would
    // never re-emit: a stream cut off with no restart until the next action.
    // Here, a full channel means backpressure on the event pump, never a
    // loss.
    let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(64);
    let (source_update_tx, mut source_update_rx) = mpsc::channel::<(String, SourceUpdate)>(32);
    // What is playing, towards `metadata` plugins: a `watch`, because only
    // the latest value matters and a slow plugin must not block the core.
    let (now_playing_tx, now_playing_rx) = watch::channel(NowPlaying {
        source: persisted.active_source.clone(),
        identity: None,
        known: Known::default(),
    });
    // Structured player state: towards the SPA (SSE route) and towards
    // Display plugins, which compose their own layout from this same frame
    // (a single channel since Task 4 of "displays, structured state").
    let (state_tx, state_rx) = watch::channel(PlayerState {
        source: persisted.active_source.clone(),
        ..Default::default()
    });
    // Sources catalog: towards Display plugins **only**, on its own channel.
    // Empty at first, `Core::new` publishes it as soon as it knows its
    // sources — a display's relay sends the current value on connection, so
    // a display wired before that publication receives the real sources
    // catalog on the next change.
    let (sources_catalog_tx, catalog_rx) = watch::channel(SourcesCatalog::default());
    let (enrich_tx, mut enrich_rx) = mpsc::channel::<(String, Enrichment)>(32);
    let (audio_tx, mut audio_rx) = mpsc::channel::<Option<String>>(4);
    let (locale_tx, mut locale_rx) = mpsc::channel::<String>(4);
    let (theme_tx, mut theme_rx) = mpsc::channel::<theme::ThemeState>(4);
    let (settings_tx, mut settings_rx) = mpsc::channel::<state::Settings>(4);
    let (plugin_order_tx, mut plugin_order_rx) = mpsc::channel::<status::PluginOrder>(4);

    // mpv. Both buffer durations are configurable without recompiling: the
    // right value depends on the network and the machine's load, not on the
    // code.
    let audio_buffer_raw = std::env::var("RITORNELLO_AUDIO_BUFFER").ok();
    let readahead_raw = std::env::var("RITORNELLO_NETWORK_READAHEAD").ok();
    let audio_buffer = player::mpv::audio_buffer_setting(audio_buffer_raw.as_deref());
    let readahead = player::mpv::readahead_setting(readahead_raw.as_deref());
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, audio_buffer, readahead, ev_tx)
            .await
            .context("starting mpv")?;

    // Fresh directory, then the registration socket bound BEFORE any launch:
    // a plugin that starts fast always finds someone there.
    let sockets_dir = plugins::prepare_sockets_dir(Path::new(&runtime_dir))?;
    let register_path = sockets_dir.join("register.sock");
    let register_listener = tokio::net::UnixListener::bind(&register_path)
        .with_context(|| format!("binding {}", register_path.display()))?;

    let mut plugin_waits: FuturesUnordered<PluginExit> = FuturesUnordered::new();
    let mut launched: Vec<String> = Vec::new();
    let mut plugin_statuses = Vec::new();
    // Shutdown triggers, one per launch: this is the only handle on a `Child`
    // moved into its supervision future. The targeted invariant — an entry
    // lives exactly as long as a launched, not-yet-reaped process — is held
    // by **three** purge sites, not one: the `plugin_waits.next()` arm
    // removes it as soon as a death it handles concerns the current
    // incarnation (a matching generation; a stale death does not touch it,
    // the entry already belonging to the relaunched process); the cleanup
    // right after the startup rendezvous (`gathered.dead`) removes it for
    // plugins dead *during* that rendezvous, whose death `plugin_waits` will
    // never see again — see `gather` and `hotplug`'s comment on late
    // announcements; and `hot_unplug` removes it itself as soon as the
    // shutdown is requested from the UI, without waiting for the killed
    // process to actually be reaped. Sending anyway to an already finished
    // supervision simply fails, with no effect: that is why the send's
    // result is ignored everywhere it is used.
    let mut kill_triggers: HashMap<String, tokio::sync::oneshot::Sender<()>> = HashMap::new();
    // The other half of the liveness oracle: plugins that announced
    // themselves without the core holding their process. See `Liveness`,
    // which says why `kill_triggers` alone used to lie both ways.
    //
    // **This registry never gets purged**, and that is accepted rather than
    // suffered: the death of a process the core does not supervise is by
    // definition unobservable, so no site could remove a name without
    // guessing. A plugin classified here therefore stays unmanageable from
    // the UI until the core's next startup — both guards return `false` and
    // say so in the log. The freeze is honest; passing it off as a
    // non-event would not be. The real answer is to pull liveness out of
    // `kill_triggers` (the "documented follow-up" of the active/inactive
    // plugins project), which redesigns their table and belongs to the
    // session that owns this code.
    let mut non_supervised: HashSet<String> = HashSet::new();
    // When each launched plugin stops getting the benefit of the doubt.
    //
    // An entry is set here at launch and removed **only** by the deadline
    // sweep, which then decides by rereading the status line rather than
    // trusting the table. This is deliberate, and it is the lesson of
    // `kill_triggers`: a registry whose correctness depends on three purge
    // sites ends up lying at one of them. Here, whether a plugin announced
    // itself, died, or was turned off in the meantime does not need to be
    // reported to the table — its line already says so.
    let mut startups: HashMap<String, tokio::time::Instant> = HashMap::new();
    // Launch generation, per name. Turning off then immediately relaunching
    // makes the death of the **old** process arrive after the new one's
    // wiring: without this counter, that death would erase status lines
    // that already describe the new one. See the `plugin_waits.next()` arm.
    let mut generations: HashMap<String, u64> = HashMap::new();
    // **Wiring** generation, per name, and distinct from `generations` just
    // above — conflating them would break one or the other.
    //
    // `generations` counts **process launches**: the `plugin_waits` arm
    // compares the generation supervision hands it back to the table's, and
    // bumping it anywhere but at launch would make a real death get ignored.
    // `wirings` counts **socket wirings**, which happen in addition: a
    // manually relaunched plugin rewires itself without the core having
    // launched it.
    //
    // Why this number is indispensable. A display relay only learns of its
    // peer's death on the **next send**, which may only come minutes later,
    // for lack of a state change. So: the plugin dies, the user manually
    // restarts it thirty seconds later, it re-announces, its lines go back
    // to connected — then a track changes, the *old* relay finally wakes up,
    // fails and reports. Without the number, this report would mark
    // disconnected a plugin that just reconnected, and only a further track
    // change would have fixed it.
    let mut wirings: HashMap<String, u64> = HashMap::new();
    // The sockets that are closing. See `HotPlugChildren::unreachable_tx`.
    //
    // Bounded to 16: a send is the end of a task, never a cadence. If the
    // channel filled up — eight plugins dying at the same time, twice over —
    // the senders would wait their turn instead of losing the notice, which
    // is the right trade-off for a message whose loss would leave a line
    // lying indefinitely.
    let (unreachable_tx, mut unreachable_rx) = mpsc::channel::<(String, u64)>(16);

    for p in &manifest.plugins {
        generations.insert(p.name.clone(), 0);
        if !p.enabled {
            // Off: nothing is launched, but the line stays — without it, the
            // page would no longer show it and it would be unrecoverable.
            tracing::info!("plugin {} is disabled, not launching it", p.name);
            plugin_statuses.push(PluginStatus::disabled(&p.name));
            continue;
        }
        let prefix = sockets_dir.join(&p.name);
        match plugins::spawn(
            &p.exec,
            &register_path,
            &p.name,
            &prefix,
            persisted.locale.as_deref(),
        ) {
            Ok(child) => {
                let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
                kill_triggers.insert(p.name.clone(), kill_tx);
                plugin_waits.push(supervise(p.name.clone(), 0, child, kill_rx));
                launched.push(p.name.clone());
            }
            Err(e) => {
                // `{e:#}` and not `{e}`: the context chain carries the path
                // that was looked for, which the bare system error message
                // does not show.
                tracing::warn!("failed to launch plugin {}: {e:#}", p.name);
                // A plugin that failed to start never announced a kind, and
                // the manifest no longer carries it: the status page shows
                // an unknown kind rather than inventing one.
                plugin_statuses.push(PluginStatus::unknown_kind(&p.name, false));
            }
        }
    }

    // The announcements channel, **shared by both stages**: the rendezvous
    // borrows it, the permanent task keeps its sender, and the select loop
    // consumes the rest. A single channel, and an announcement can no longer
    // get lost between the two: whatever `gather` did not have time to read
    // stays queued, and will be hot-wired an instant later. See the doc of
    // `register::gather` for the race this removes.
    let (late_tx, mut late_rx) = mpsc::channel::<Announcement>(16);

    // One announcement per launched plugin. Early deaths shorten the wait;
    // `plugin_waits` stays usable afterwards, only the entries consumed here
    // leave it — and those are precisely the ones whose death we already
    // learned of.
    let mut gathered = register::gather(
        &register_listener,
        &launched,
        (&mut plugin_waits).map(|(name, _gen, _status, _requested)| name),
        std::time::Duration::from_secs(10),
        &late_tx,
        &mut late_rx,
    )
    .await;

    // The dead plugins from this gathering were consumed directly on
    // `plugin_waits` by `gather` (see its doc, and `hotplug`'s comment on
    // late announcements): the main loop will therefore never see them again
    // on its `plugin_waits.next()` arm, which is one of the two other sites
    // purging `kill_triggers` (the third being `hot_unplug`). Without this
    // cleanup, a plugin that died *during* the rendezvous would leave a
    // stale entry, and a later turn-on would take it for a live process and
    // never relaunch the binary.
    for name in &gathered.dead {
        kill_triggers.remove(name);
    }

    // `gather` took the listener by **reference**: the core therefore keeps
    // ownership of it, and the registration socket does not close with the
    // rendezvous. The deadline above no longer condemns anyone — it serves to
    // avoid blocking startup and to name a stalled plugin. A plugin that
    // announces itself at t+12s (a cold start on an SD card, eight binaries
    // mounting their runtime at the same time) is hot-wired, and a manually
    // relaunched plugin is picked back up.
    tokio::spawn(register::accept_forever(register_listener, late_tx));

    // One "unknown kind" line per plugin not announced, distinguishing the
    // stalled from the dead: the former is still running and can still
    // announce itself, the latter has nothing left to say. That is the
    // difference the operator must see before going to relaunch anything.
    for (name, stalled) in gathered
        .stalled
        .iter()
        .map(|n| (n, true))
        .chain(gathered.dead.iter().map(|n| (n, false)))
    {
        plugin_statuses.push(PluginStatus::unknown_kind(name, stalled));
    }

    // `metadata` plugins announced, **in manifest order**: this order is the
    // arbitration priority, and it is a configuration property, not a
    // runtime one. The list is therefore rebuilt from the manifest and never
    // from the order announcements arrived in, which would make the display
    // non-reproducible from one startup to the next.
    let manifest_order: Vec<String> = manifest.plugins.iter().map(|p| p.name.clone()).collect();
    let metadata_plugins = register::metadata_order(&manifest_order, &gathered);
    // The file order arbitrates `metadata` plugins; the `exec`, meanwhile,
    // only served for the initial launch. Relaunching a plugin asks for it
    // again.
    let execs: HashMap<String, String> =
        manifest.plugins.iter().map(|p| (p.name.clone(), p.exec.clone())).collect();

    // The admin page is **announced** by the binary, then observed through a
    // waiting window: the status flag starts from the registration line. But
    // the announcement is only a file declaration — it is an **observed**
    // capability that the UI must see in the end: if the admin connection
    // fails below, the flag is set back to `false` on every line for this
    // name, whatever their kind.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    // The name travels with the client: it is what names the plugin in the
    // log when its relay stops.
    // The announcement's `covers` flag travels with the client: by the time
    // the relays are spawned (below, after `Core::new`), the announcement is
    // no longer at hand, and nothing must rebuild this flag other than by
    // copying it from what the plugin announced.
    let mut display_clients: Vec<(String, Arc<DisplayClient>, bool)> = Vec::new();
    let mut admin_backends: HashMap<String, Arc<dyn admin::AdminBackend>> = HashMap::new();

    for name in &manifest_order {
        let Some(announcement) = gathered.announcements.get(name) else {
            continue;
        };
        let prefix = sockets_dir.join(name);

        for kind in &announcement.kinds {
            let socket = ritornello_plugin_sdk::socket_kind(&prefix, *kind);
            // The announcement proves the socket is bound: a bare `connect`
            // suffices, no more retry loop. A failure here is a real
            // anomaly, not a startup race — and it stays confined to this
            // kind, the other kinds of the same plugin continuing to be
            // wired.
            match kind {
                PluginKind::Source => {
                    // Same gesture as in `hotplug`, which says why the SDK
                    // knows nothing of the core's bookkeeping.
                    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
                    let unreachable = unreachable_tx.clone();
                    let closed_name = name.clone();
                    tokio::spawn(async move {
                        if closed_rx.await.is_ok() {
                            let _ = unreachable.send((closed_name, 0)).await;
                        }
                    });
                    match SourceClient::connect_with_close(
                        &socket,
                        name.clone(),
                        source_update_tx.clone(),
                        Some(closed_tx),
                    )
                    .await
                    {
                        Ok(client) => {
                            sources.insert(name.clone(), client);
                            plugin_statuses.push(PluginStatus::kind(name, "source", true, announcement.admin));
                        }
                        Err(e) => {
                            tracing::warn!("plugin {name} source unavailable: {e}");
                            plugin_statuses.push(PluginStatus::kind(name, "source", false, announcement.admin));
                        }
                    }
                }
                PluginKind::Display => match DisplayClient::connect(&socket).await {
                    Ok(client) => {
                        display_clients.push((name.clone(), client, announcement.covers));
                        plugin_statuses.push(PluginStatus::kind(name, "display", true, announcement.admin));
                    }
                    Err(e) => {
                        tracing::warn!("display plugin {name} unavailable: {e}");
                        plugin_statuses.push(PluginStatus::kind(name, "display", false, announcement.admin));
                    }
                },
                PluginKind::Input => {
                    let tx = cmd_tx.clone();
                    let socket_for_task = socket.clone();
                    let task_name = name.clone();
                    let unreachable = unreachable_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = run_input_client(&socket_for_task, tx).await {
                            tracing::warn!("input plugin {task_name} disconnected: {e}");
                        }
                        // `0`: this is the startup rendezvous's wiring, and
                        // `wirings` does not carry any entry for it yet — its
                        // default value is therefore read as `0` on the
                        // loop's side, which makes this socket the current
                        // incarnation as long as no one has rewired this
                        // name.
                        let _ = unreachable.send((task_name, 0)).await;
                    });
                    plugin_statuses.push(PluginStatus::kind(name, "input", true, announcement.admin));
                }
                PluginKind::Metadata => {
                    // Two-way relay, in its own task: its failure concerns
                    // only metadata. **Playback is never affected** by a
                    // `metadata` plugin.
                    let tx = enrich_tx.clone();
                    let np_rx = now_playing_rx.clone();
                    let socket_for_task = socket.clone();
                    let task_name = name.clone();
                    let unreachable = unreachable_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            run_metadata_client(&socket_for_task, task_name.clone(), tx, np_rx).await
                        {
                            tracing::warn!("metadata plugin {task_name} disconnected: {e}");
                        }
                        let _ = unreachable.send((task_name, 0)).await;
                    });
                    plugin_statuses.push(PluginStatus::kind(name, "metadata", true, announcement.admin));
                }
            }
        }

        if announcement.admin {
            let path = ritornello_plugin_sdk::admin_socket(&prefix);
            match ritornello_plugin_sdk::AdminClient::connect(&path).await {
                Ok(client) => {
                    admin_backends.insert(name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!("admin plugin {name} unreachable: {e}");
                    // The status flag follows what was actually connected,
                    // not what the plugin announced: an announcement with
                    // `admin: true` followed by a failing `connect` must
                    // never leave the UI pointing at a page that answers
                    // 404. Set back here to `false` on every line for this
                    // name, whatever their kind, pushed earlier in the kinds
                    // loop that precedes this connection.
                    for status in plugin_statuses.iter_mut().filter(|s| s.name == *name) {
                        status.admin = false;
                    }
                }
            }
        }
    }

    // Under lock from here on: startup wiring is done, but the table is not
    // frozen for all that — a plugin that announces itself late must see its
    // admin page appear without a core restart.
    let admin_backends: admin::AdminBackends = Arc::new(RwLock::new(admin_backends));
    // Named rather than built inline in the `AppState` literal: the main
    // loop and `hotplug` must purge **this** cache, the one the routes read,
    // never a fresh copy.
    let admin_assets: Arc<admin::AssetCache> = Arc::new(Default::default());

    // Starting **with no source at all** is legitimate since hot
    // registration, and that was the last deadline that used to condemn:
    // refusing to start at t+10s contradicts the idea that a source can
    // arrive at t+30s, and removes the status page precisely when one would
    // want to see the stalled plugin there. There will be nothing to read,
    // but you can already see what is happening.
    //
    // One refusal remains, and it is not slowness but a configuration
    // error: plugins declared enabled, and no **living process** left at all
    // to announce itself. See `register::startup_refused`.
    let declared_enabled = manifest.plugins.iter().filter(|p| p.enabled).count();
    if register::startup_refused(declared_enabled, &launched, &gathered) {
        anyhow::bail!(
            "no plugin process alive (every enabled plugin failed to launch or exited)"
        );
    }
    if declared_enabled == 0 {
        tracing::warn!(
            "every plugin is disabled in plugins.toml: starting anyway so they can be re-enabled from the admin UI"
        );
    }
    if sources.is_empty() {
        tracing::warn!(
            "no source plugin connected, starting anyway: a source that announces itself later will be wired without a restart"
        );
    }

    // The core's status page (plugins, active source, latest errors, audio output).
    let status_state = Arc::new(RwLock::new(StatusState {
        plugins: plugin_statuses,
        active_source: persisted.active_source.clone(),
    }));
    let audio_current = Arc::new(RwLock::new(persisted.audio_device.clone()));
    let locale_current = Arc::new(RwLock::new(persisted.locale.clone()));
    // `state.json` is reread with no guarantee: `theme_put` validates the
    // HTTP path, but a corrupted or hand-edited state file can carry
    // anything. An unknown theme name makes `applyTheme` on the SPA side
    // exit without setting a single CSS variable, and `theme.css` has no
    // fallback value: the UI displays entirely untheme. `from_persisted`
    // validates and falls back to the defaults while logging a warning.
    let theme_current = Arc::new(RwLock::new(theme::from_persisted(
        persisted.theme.as_deref(),
        persisted.mode.as_deref(),
    )));
    let settings_current = Arc::new(RwLock::new(persisted.settings.clone()));
    // Results of the core's detached fetches: the task that
    // `Core::start_cover_fetch` detaches drops a key here once the bytes are
    // in hand (or already cached), and the `select!` loop below consumes
    // them to publish the local URL for the right track.
    let (cover_tx, mut cover_rx) = mpsc::channel::<(String, bool)>(4);
    // Result of a detached embedded-cover extraction (see
    // `Core::handle_path`): same principle as `cover_tx` above, on a
    // separate channel rather than an enrichment on the same one — the two
    // carry different payloads, and nothing synchronizes them with each
    // other.
    let (extraction_tx, mut extraction_rx) =
        mpsc::channel::<(String, Option<ritornello_proto::CoverRef>)>(4);

    // After wiring: ask each source for its sources catalog, **without
    // waiting**.
    //
    // One detached task per source, and none is joined. The reply correlated
    // to `ListPresets` is a `Noop`: it teaches the core nothing, presets
    // arriving through `source_update_rx` as `preset_count`. Waiting for
    // these replies would therefore put the sources protocol's 5s delay on
    // the startup path, once per unreachable source — and removing those
    // windows was the whole point of the previous project.
    for (name, client) in &sources {
        let (c, n) = (client.clone(), name.clone());
        tokio::spawn(async move {
            if let Err(e) = c.request(ritornello_proto::SourceReq::ListPresets).await {
                tracing::debug!("list_presets for {n}: {e}");
            }
        });
    }

    // Core. The displayed active source is kept up to date live by the loop
    // below (updating status_state.active_source after every command).
    let mut core;
    // The cover cache that `assemble_covers_and_core` builds, pulled out of
    // the block below: the display relays (further down) and the hot wiring
    // must read **the same** `Arc` as the core and the HTTP route, never a
    // second cache — this is where the core drops the bytes it fetches.
    let app_covers;
    {
        // Asked once, before serving: the answer gates the System tab's two
        // OS buttons, and asking per request would mean spawning `busctl`
        // twice every five seconds.
        let probe = system::probe_capabilities().await;
        // `covers` below is only a skeleton: `assemble_covers_and_core`
        // overwrites it with the sole `Arc<CoverCache>` it builds, handed
        // back identical to the `Core` it returns — see its doc. Building
        // the `AppState` as a single literal here, rather than in two
        // pieces, avoids duplicating its fifteen-odd fields unrelated to
        // covers.
        let app_state_skeleton = AppState {
            status: status_state.clone(),
            logs: log_buffer.clone(),
            audio_current: audio_current.clone(),
            audio_tx: audio_tx.clone(),
            catalog: catalog.clone(),
            locale_current: locale_current.clone(),
            locale_tx: locale_tx.clone(),
            locales_root: locales_root.clone(),
            admin_backends: admin_backends.clone(),
            admin_assets: admin_assets.clone(),
            cmd_tx: cmd_tx.clone(),
            theme_current: theme_current.clone(),
            theme_tx: theme_tx.clone(),
            settings_current: settings_current.clone(),
            settings_tx: settings_tx.clone(),
            player: state_rx.clone(),
            sources_catalog: catalog_rx.clone(),
            system: Arc::new(system::SystemInfo {
                can_power_off: probe.can_power_off,
                can_reboot: probe.can_reboot,
                logind_reachable: probe.logind_reachable,
                // The restart hook kills mpv **before** exiting. Without
                // this, mpv outlived the core and kept playing: it is
                // launched with `kill_on_drop(true)`, but `std::process::exit`
                // does not unwind the stack and therefore runs no `Drop` —
                // the guarantee `kill_on_drop` advertises was worth nothing
                // on this path.
                //
                // The service did not show it: when a unit's main process
                // exits, systemd kills the rest of the control group before
                // relaunching. It was in development, with no supervisor,
                // that the orphan stuck around — still playing, and holding
                // the audio device that the relaunched core wanted to
                // reclaim.
                //
                // mpv's death also makes the main loop exit (see
                // `mpv_child.wait()` further down): both paths run, but they
                // lead to the same place, and it is the `exit(0)` below that
                // wins in practice. The signal's detail and its justification
                // live in `system::terminate_process`, where a test pins them
                // down on a real process.
                restart: {
                    let pid = mpv_child.id();
                    Arc::new(move || {
                        system::terminate_process(pid);
                        std::process::exit(0)
                    })
                },
                ..Default::default()
            }),
            covers: Arc::new(cover::CoverCache::default()),
            plugins: Arc::new(status::PluginsControl {
                manifest: plugins_path.clone(),
                names: manifest_order.clone(),
                tx: plugin_order_tx,
            }),
        };
        let (app_state, core_engine) = assemble_covers_and_core(
            mpv_player,
            core::Wiring {
                sources,
                persisted,
                state_path,
                catalog: catalog.clone(),
                locales_root: locales_root.clone(),
                metadata: MetadataWiring {
                    plugins: metadata_plugins,
                    now_playing: now_playing_tx,
                    state: state_tx,
                },
                sources_catalog: sources_catalog_tx,
            },
            cover_tx,
            extraction_tx,
            app_state_skeleton,
        );
        core = core_engine;
        app_covers = app_state.covers.clone();
        let app = status::router(app_state);
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("web interface on http://{http_addr}/");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("status server: {e}");
            }
        });
    }
    // Best-effort, like the wake via `Power` (see the comment below): startup
    // must never put systemd in a restart loop. `startup` reads
    // `settings.startup_power`; its standby branch skips the source wake but
    // still configures mpv, so the first `Power` starts right.
    if let Err(e) = core.startup().await {
        tracing::warn!("startup wake: {e}");
    }

    // Relays the state to every connected display: the same channel that
    // feeds the SPA's SSE route, each plugin composing its own layout from
    // the received frame.
    //
    // **One task per display**, not one task looping over N clients: that is
    // what keeps a slow display — busy console, screen blocked in I/O — from
    // delaying the others. Backpressure stays confined per socket, which was
    // the argument retained for not merging the per-kind sockets.
    //
    // **After `Core::new`**, and this is deliberate: it is what publishes the
    // first sources catalog. Spawned before it, the relays used to send each
    // display an empty `SourcesCatalog` followed by the real one — no
    // consequence for a display that draws, but an MPD client connected in
    // that window would read an empty `listplaylists` and could cache it.
    // The ordering removes the window instead of catching up on it
    // downstream.
    //
    // Before, this variable was an `Option`: declaring two displays produced
    // no error, but the core only kept the client of the last one declared,
    // and the first one waited for lines that never arrived.
    if display_clients.is_empty() {
        tracing::warn!("no display plugin connected, continuing without display");
    }
    for (name, display_client, wants_covers) in display_clients {
        display_relay(
            name,
            display_client,
            wants_covers,
            app_covers.clone(),
            state_rx.clone(),
            catalog_rx.clone(),
            UnreachableNotice { wiring: 0, tx: unreachable_tx.clone() },
        );
    }

    // Everything needed to wire a plugin that will speak later: the same
    // children as the startup wiring loop, held beyond it.
    let hot_children = HotPlugChildren {
        sockets_dir: sockets_dir.clone(),
        manifest_order,
        source_update_tx: source_update_tx.clone(),
        cmd_tx: cmd_tx.clone(),
        enrich_tx: enrich_tx.clone(),
        now_playing_rx: now_playing_rx.clone(),
        state_rx: state_rx.clone(),
        catalog_rx: catalog_rx.clone(),
        covers: app_covers.clone(),
        status_state: status_state.clone(),
        admin_backends: admin_backends.clone(),
        admin_assets: admin_assets.clone(),
        unreachable_tx: unreachable_tx.clone(),
    };

    let mut retry_at: Option<tokio::time::Instant> = None;
    // Deadline of the next position refresh. Absolute, like `retry_at`: see
    // the reason at the arming point, in the loop.
    let mut next_tick: Option<tokio::time::Instant> = None;

    loop {
        let retry_sleep = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // Deadline of the nearest "end of benefit of the doubt". The
        // minimum, and recomputed on every turn like the other three:
        // several plugins start together when the service launches, and it
        // is the first one to reach its deadline that must wake the loop.
        let startup_at = startups.values().copied().min();
        let startup_sleep = async {
            match startup_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // Deadline of the volume/mute overlay, read into a local variable
        // before the `select!` (like `retry_at`) so as not to keep a borrow
        // on `core` during the wait.
        let overlay_at = core.overlay_deadline().map(tokio::time::Instant::from);
        let overlay_sleep = async {
            match overlay_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // Position tick: one second, armed only when there is a position to
        // publish (see `Core::tick_position`).
        //
        // The deadline is **absolute**, like `retry_at` and `overlay_at`,
        // and this is a defect found in review that requires it. The three
        // waiting futures are recreated on every loop turn, hence every time
        // any arm resolves — a command, an mpv event, an enrichment, a
        // setting change. Recreating a `sleep_until(at)` on the same
        // deadline changes nothing; recreating a relative `sleep(1s)`
        // restarts the countdown from zero. The tick would then not happen
        // once per second but one second after the `select!`'s last
        // wake-up, and on a device where events succeed each other faster
        // than that, it would be pushed back indefinitely — the position
        // would never move, precisely when something is happening. The
        // computation is extracted into the pure, tested function
        // `core::next_deadline`: this `select!` loop itself has no safety
        // net.
        next_tick = core::next_deadline(
            core.tick_position(),
            next_tick.map(tokio::time::Instant::into_std),
            tokio::time::Instant::now().into_std(),
        )
        .map(tokio::time::Instant::from);
        // Local copy (`Instant` is `Copy`): the future below therefore
        // borrows neither `core` nor the variable reassigned in the arm.
        let position_at = next_tick;
        let position_sleep = async {
            match position_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(msg) = cmd_rx.recv() => {
                if let Err(e) = core.handle_input(msg).await {
                    tracing::warn!("command: {e}");
                }
                status_state.write().await.active_source = core.active_source().to_string();
            }
            // Closed channel (mpv pump dead): the `Some(..)` pattern stops
            // matching and `tokio::select!` disables the arm — the
            // `mpv_child.wait()` arm will take over to exit cleanly.
            Some(ev) = ev_rx.recv() => {
                // It is the core that qualifies the event (see
                // `EventOutcome`): the list of variants that attest to the
                // stream's liveness exists in only one place.
                match core.handle_event(ev).await {
                    core::EventOutcome::StreamAlive => retry_at = None,
                    core::EventOutcome::RetryIn(delay) => {
                        retry_at = Some(tokio::time::Instant::now() + delay);
                    }
                    core::EventOutcome::Nothing => {}
                }
            }
            // Announcement arriving **after** the rendezvous: a plugin slow
            // to start, or manually relaunched. The wiring is the same, kind
            // by kind, and a re-announcement is treated as a late
            // announcement — we rewire.
            Some(announcement) = late_rx.recv() => {
                // A fresh number **before** wiring: the sockets of the
                // previous incarnation, if any remain, become stale at this
                // exact instant, and their closing will be ignored.
                let wiring = wirings.entry(announcement.name.clone()).or_insert(0);
                *wiring += 1;
                let wiring = *wiring;
                hotplug(
                    announcement,
                    &hot_children,
                    &mut core,
                    &mut gathered,
                    &kill_triggers,
                    &mut non_supervised,
                    wiring,
                )
                .await;
                status_state.write().await.active_source = core.active_source().to_string();
            }
            // A plugin's socket has closed. **This is what makes the death
            // of an unsupervised plugin visible**, which until now only
            // produced a log line: the page kept showing it connected,
            // indefinitely.
            //
            // What the closing exactly proves: the peer closed. Either its
            // process died, or it closed its socket. In both cases it is no
            // longer reachable, so "disconnected" is honest — but this is
            // not strict proof of death, and nothing here infers an exit
            // code from it.
            Some((name, wiring)) = unreachable_rx.recv() => {
                if wirings.get(&name).copied().unwrap_or(0) != wiring {
                    // The socket of a previous incarnation, arriving after
                    // the next one's rewiring. See `wirings`.
                    tracing::debug!(
                        "plugin {name} socket from wiring {wiring} closed after it was rewired"
                    );
                } else {
                    tracing::info!("plugin {name} is no longer reachable, reporting it disconnected");
                    // **The name leaves `non_supervised`, and that is the
                    // useful half.** It was only there because a living
                    // process escaped the core's supervision; that process
                    // is no longer reachable, so the plugin becomes
                    // manageable again — turning it on from the UI will
                    // launch a real, supervised one, instead of being
                    // refused by `hot_unplug`.
                    //
                    // This is also why this registry gets purged while
                    // `startups` does not: `non_supervised` describes a
                    // **capability** the core has over a process, and that
                    // capability just changed. No status line carries this
                    // information, so nothing could reread it.
                    non_supervised.remove(&name);
                    // **Unwired if it was a Source**, exactly as the
                    // supervised death branch does. Without this line, this
                    // path would be a half-measure, and a half-measure is
                    // worse than nothing here: the page would say
                    // "unreachable" while the core kept the source wired on
                    // a closed socket, hence still offered to the sources
                    // catalog and the remote control. The two death paths
                    // must produce the **same** state, or behavior would
                    // depend on who launched the process.
                    //
                    // `forget_dead_source` and not `remove_source`: the
                    // distinction is written in its doc, and it holds here
                    // for the same reason — nobody requested this shutdown,
                    // so nothing switches to another source. The music
                    // continues, `active_source` keeps its name, and it is
                    // the conjunction "active source X, plugin X
                    // unreachable" that carries the diagnosis.
                    if !core.forget_dead_source(&name) {
                        tracing::debug!("plugin {name} was not a wired source, nothing to unwire");
                    }
                    // A single lock for both writes, like `hot_unplug`: the
                    // "disconnected" line and the active source's name
                    // describe the same instant.
                    let mut statuses = status_state.write().await;
                    // Idempotent, and it needs to be: for a *supervised*
                    // plugin, the `plugin_waits` arm will also mark it, in
                    // an order nothing fixes. `mark_plugin_disconnected`
                    // only sets booleans, and `remove` on an absent key is a
                    // non-event — verified, not assumed.
                    crate::status::mark_plugin_disconnected(&mut statuses, &name);
                    statuses.active_source = core.active_source().to_string();
                    // After the status lock, not before: `forget_page` takes
                    // two other locks, and nesting them would make safety
                    // depend on an order never to reverse elsewhere.
                    drop(statuses);
                    admin::forget_page(&admin_backends, &admin_assets, &name).await;
                }
            }
            Some((name, update)) = source_update_rx.recv() => {
                core.handle_source_update(&name, update);
            }
            Some((plugin, enrichment)) = enrich_rx.recv() => {
                core.handle_enrichment(&plugin, enrichment);
            }
            // A detached fetch by `Core::start_cover_fetch` finished, with
            // or without success: `cover_arrived` releases the in-flight
            // marker in every case, and only publishes the local URL on
            // success and if it still describes what is playing.
            Some((key, success)) = cover_rx.recv() => {
                core.cover_arrived(key, success).await;
            }
            // A detached extraction by `Core::handle_path` finished (result
            // bounded by `Health::bounded`, see `health.rs`): `extraction_arrived`
            // releases the in-flight marker in every case, and only keeps
            // the result if it still describes what mpv is currently
            // playing.
            Some((path, r)) = extraction_rx.recv() => {
                core.extraction_arrived(path, r).await;
            }
            Some(device) = audio_rx.recv() => {
                if let Err(e) = core.set_audio_device(device).await {
                    tracing::warn!("audio output change: {e}");
                }
            }
            Some(locale) = locale_rx.recv() => {
                if let Err(e) = core.set_locale(locale).await {
                    tracing::warn!("locale change: {e}");
                }
            }
            Some(t) = theme_rx.recv() => {
                core.set_theme(t);
            }
            Some(s) = settings_rx.recv() => {
                core.set_settings(s);
            }
            Some(order) = plugin_order_rx.recv() => {
                let ok = if order.active {
                    // A redundant turn-on (double click, page left open)
                    // must be a non-event, not a second process stealing the
                    // first one's socket prefix: the core cannot rely on the
                    // caller to never resend an order already in effect.
                    //
                    // The predicate used to be `kill_triggers.contains_key`,
                    // hence false precisely in the case this guard exists to
                    // cover. See `Liveness`, which writes why the two
                    // registries had to be crossed.
                    match liveness(&order.name, &kill_triggers, &non_supervised) {
                        // Launched by the core: the order is already in
                        // effect, and the acknowledgment describes a true
                        // state.
                        Liveness::Supervised => true,
                        // A process is running for this name and the core
                        // has no hold on it. Launching a second one would
                        // steal its socket prefix — noisy on the MPD plugin,
                        // which fails to bind its port and dies, but silent
                        // everywhere else. Refuse, and name the remedy.
                        Liveness::OutOfReach => {
                            tracing::warn!(
                                "refusing to enable {}: a process for it is already running outside the core's control — kill it yourself, or restart the core to let it take ownership again",
                                order.name
                            );
                            false
                        }
                        Liveness::Off => {
                            let generation = generations.entry(order.name.clone()).or_insert(0);
                            *generation += 1;
                            let generation = *generation;
                            match execs.get(&order.name) {
                                Some(exec) => {
                                    match relaunch(
                                        &order.name,
                                        exec,
                                        generation,
                                        &hot_children,
                                        &register_path,
                                        core.current_locale().as_deref(),
                                        &mut kill_triggers,
                                    )
                                    .await
                                    {
                                        Some(fut) => {
                                            plugin_waits.push(fut);
                                            // The benefit of the doubt starts
                                            // here, not from the service's
                                            // launch: it is the turn-on from
                                            // the UI that this delay covers.
                                            // The startup rendezvous has its
                                            // own deadline and its own
                                            // report (`stalled`).
                                            startups.insert(
                                                order.name.clone(),
                                                tokio::time::Instant::now() + STARTUP_TIMEOUT,
                                            );
                                            true
                                        }
                                        None => false,
                                    }
                                }
                                // A name refused well before this point by
                                // the HTTP layer: this is a guard, not a use
                                // case.
                                None => false,
                            }
                        }
                    }
                } else {
                    hot_unplug(
                        &order.name,
                        &hot_children,
                        &mut core,
                        &mut gathered,
                        &mut kill_triggers,
                        &non_supervised,
                    )
                    .await
                };
                // The requester is waiting: a lost acknowledgment would leave
                // its HTTP request hanging until its own timeout runs out.
                let _ = order.ack.send(ok);
            }
            _ = startup_sleep => {
                let now = tokio::time::Instant::now();
                let due: Vec<String> = startups
                    .iter()
                    .filter(|(_, at)| **at <= now)
                    .map(|(name, _)| name.clone())
                    .collect();
                let mut statuses = hot_children.status_state.write().await;
                for name in due {
                    // Removed in every case: the entry has done its job, and
                    // leaving it would grow the table for the life of the
                    // process.
                    startups.remove(&name);
                    // But the downgrade only happens if the line **still**
                    // says "starting". The plugin may have announced itself
                    // in the meantime (its line then describes its kinds),
                    // died (it says "disconnected"), or been turned off from
                    // the UI (it says "disabled"): in all three cases,
                    // overwriting would replace true information with
                    // false. Rereading the state rather than keeping a
                    // registry to purge in three places — see the comment on
                    // `startups`.
                    if should_downgrade(&statuses, &name) {
                        tracing::warn!(
                            "plugin {name} still silent {}s after launch, reporting it as stalled",
                            STARTUP_TIMEOUT.as_secs()
                        );
                        status::replace_plugin_lines(
                            &mut statuses,
                            &name,
                            vec![PluginStatus::unknown_kind(&name, true)],
                            false,
                        );
                    }
                }
            }
            _ = retry_sleep => {
                retry_at = None;
                if let Err(e) = core.retry_stream().await {
                    tracing::warn!("stream retry: {e}");
                }
            }
            _ = overlay_sleep => {
                core.expire_overlay();
            }
            _ = position_sleep => {
                // Rearm first, from now: the cadence stays at one second no
                // matter what happens on the other arms.
                next_tick =
                    Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1));
                // Refresh then publish: the position having changed, the
                // frame crosses deduplication and goes out to the SPA as
                // well as to the displays. Any overlay currently active
                // travels within this same frame, intact — it is the
                // display that decides where to place it, and the core
                // keeps control of its deadline (`overlay_sleep` arm).
                core.refresh_position().await;
                core.publish_state();
            }
            // `next()` and not `select_next_some()`: `tokio::select!` does
            // not consult `is_terminated`, and repolling an exhausted
            // `FuturesUnordered` via `select_next_some` panics
            // (`SelectNextSome polled after terminated`) — meaning the death
            // of the **last** plugin would kill the core on the next
            // iteration, the exact opposite of the intended degradation.
            // With `next()`, exhaustion returns `None`, the pattern does not
            // match, and the arm is simply disabled.
            Some((name, generation, status, requested)) = plugin_waits.next() => {
                // Death of a stale incarnation: the plugin was relaunched
                // in the meantime, and the status lines already describe
                // the new process. Marking "disconnected" here would erase
                // them in favor of a death that no longer applies.
                if generations.get(&name).copied() != Some(generation) {
                    tracing::debug!("plugin {name} generation {generation} exited after being replaced");
                } else {
                    // An entry lives exactly as long as a launched,
                    // not-yet-reaped process: this one just was. Removing
                    // it here, and only here (never in the stale branch
                    // above), is what lets a later turn-on distinguish a
                    // living plugin from a dead one — `hot_unplug` already
                    // removed it when `requested` is true, so this removal
                    // is then a second, no-op pass.
                    kill_triggers.remove(&name);
                    if requested {
                        tracing::info!("plugin {name} stopped: disabled from the admin UI");
                    } else {
                        tracing::warn!("plugin {name} exited: {status:?}");
                        // Unwiring the source, but **not** with the
                        // voluntary path's function, and that is the heart
                        // of the decision.
                        //
                        // What must be forgotten is the same in both cases:
                        // without eviction, a dead plugin left its name in
                        // `source_order` and its presets in
                        // `presets_by_source`, so an MPD client kept a
                        // registered list for a source that no longer
                        // exists, and a `load` on it **passed** the guard
                        // of `Command::SelectSource` (which only consults
                        // `source_order`).
                        //
                        // What differs is the consequence on what is
                        // playing. `remove_source` switches to the next
                        // source when it was the active one: that is fine
                        // when **the operator** requested the shutdown, the
                        // switch being the follow-up of their gesture. Here
                        // nobody requested anything. A Source plugin is a
                        // *controller* — the stream is held by mpv, a child
                        // of the core, which its death does not touch — so
                        // switching turned a controller's failure into
                        // silence, then showed "cd" on a device whose user
                        // had chosen the radio. The music continues,
                        // `active_source` keeps its name, and the status
                        // page carries the full diagnosis: active source,
                        // plugin unreachable. See `forget_dead_source`'s
                        // doc, which writes the comparison of the two
                        // paths.
                        if !core.forget_dead_source(&name) {
                            tracing::debug!("plugin {name} was not a wired source, nothing to unwire");
                        }
                        // A single lock for both writes, like
                        // `hot_unplug`: the "disconnected" line and the
                        // active source's name describe the same instant.
                        //
                        // Reasserted even though this path no longer
                        // changes `active_source`: it is the status page
                        // that must show both facts **together** — the
                        // active source is "radio" and the "radio" plugin
                        // is no longer reachable. It is this conjunction
                        // that is the diagnosis, and rereading it from the
                        // core rather than assuming it has not moved keeps
                        // the line correct if the switching decision were
                        // ever revisited.
                        let mut statuses = status_state.write().await;
                        crate::status::mark_plugin_disconnected(&mut statuses, &name);
                        statuses.active_source = core.active_source().to_string();
                        // Same gesture as on the neighboring path: both
                        // deaths must leave the same state, or behavior
                        // would depend on who launched the process.
                        drop(statuses);
                        admin::forget_page(&admin_backends, &admin_assets, &name).await;
                    }
                }
            }
            status = mpv_child.wait() => {
                anyhow::bail!("mpv exited ({status:?}), stopping for restart by systemd");
            }
        }
    }
}

#[cfg(test)]
mod unreachable_tests {
    //! What the core reports when a display's socket closes — and what it
    //! does not report.
    //!
    //! **No time margin here either, not even a cap.** Both directions are
    //! proven by the senders closing: the relay holds the *only* sender of
    //! the channel, so `recv()` returns `Some` if it reports and `None` as
    //! soon as it finishes without doing so. The wait is exact in both
    //! cases, and a relay that got the direction wrong would not make the
    //! test time out — it would make it fail on the value.

    use super::*;
    use ritornello_plugin_sdk::{bind_display, serve_display, DisplayPlugin};

    /// A display that reads and discards: this module does not test what is
    /// received, only who is notified of the closing.
    struct Mute;

    #[async_trait::async_trait]
    impl DisplayPlugin for Mute {
        async fn show(&mut self, _state: PlayerState) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_display_that_no_longer_responds_is_reported_with_its_wiring_generation() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let client = DisplayClient::connect(&socket).await.unwrap();
        // The peer accepts then **vanishes**. This is the death a manually
        // relaunched plugin produces, and that the core used to only see
        // pass through its log.
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
        drop(listener);

        let (_state_tx, state_rx) = watch::channel(PlayerState::default());
        let (_cat_tx, catalog_rx) = watch::channel(SourcesCatalog::default());
        let (tx, mut rx) = mpsc::channel(4);
        // Both `watch`es stay alive: without this the relay could exit
        // through the "the core is stopping" path, and the test would
        // confuse the two directions it exists precisely to separate.
        display_relay(
            "dead".into(),
            client,
            false,
            Arc::new(cover::CoverCache::default()),
            state_rx,
            catalog_rx,
            UnreachableNotice { wiring: 7, tx },
        );

        // The relay writes the state right away, before its loop: that
        // write alone suffices, the peer having closed. `None` here would
        // mean it finished without saying anything — the exact defect this
        // path fixes.
        assert_eq!(
            rx.recv().await,
            Some(("dead".to_string(), 7)),
            "the socket closing must be reported, with the wiring number received"
        );
    }

    #[tokio::test]
    async fn core_shutdown_reports_no_unreachable_plugin() {
        // The observation this test exists to prevent: marking every display
        // disconnected during core shutdown, i.e. painting a failure over a
        // normal stop. The relay's two loop exits look alike — one is a
        // failed send, the other a closed `watch` — and nothing else tells
        // them apart.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        tokio::spawn(async move {
            let _ = serve_display(listener, Mute).await;
        });
        let client = DisplayClient::connect(&socket).await.unwrap();

        let (state_tx, state_rx) = watch::channel(PlayerState::default());
        let (cat_tx, catalog_rx) = watch::channel(SourcesCatalog::default());
        let (tx, mut rx) = mpsc::channel(4);
        display_relay(
            "alive".into(),
            client,
            false,
            Arc::new(cover::CoverCache::default()),
            state_rx,
            catalog_rx,
            UnreachableNotice { wiring: 3, tx },
        );

        // The core stops: its senders drop. The peer, meanwhile, is still
        // there and reading.
        drop(state_tx);
        drop(cat_tx);

        // The relay holds the only remaining sender of the channel. `None`
        // therefore proves it finished **without** reporting, and the wait
        // is exact: it resolves the instant its task finishes, with no
        // duration assumed anywhere.
        assert_eq!(
            rx.recv().await,
            None,
            "core shutdown is not a plugin's death: nothing should be reported"
        );
    }
}

#[cfg(test)]
mod toggle_tests {
    //! The on/off toggle, and what the core knows of a plugin's life.
    //!
    //! What is tested here: the classification (`liveness`), and on the
    //! **real** path `hot_unplug`'s refusal when the process is out of
    //! reach. This refusal must happen **before** any mutation, and that is
    //! the whole point: unwiring then returning `false` would leave a plugin
    //! alive, unwired, and shown as "inactive" — the worst of the three
    //! states. The positive control right next to it is what gives the
    //! refusal test its bite: without the early return, it fails.
    //!
    //! What is **not** tested, and it is worth writing down: the turn-on
    //! guard lives in `main`'s `select!`, out of a test's reach. It consults
    //! the same function, but its wiring is only checked through playback.

    use super::*;
    use crate::core::{Wiring, MetadataWiring};
    use crate::cover::CoverCache;
    use ritornello_proto::{Announcement, PluginKind};

    /// A `Player` that does nothing: no test here looks at the player.
    /// `hot_unplug` only touches it through `remove_source`, and the sources
    /// map is empty — which is deliberate, otherwise a `Source` stub would
    /// also be needed for a path these tests do not visit.
    struct MutePlayer;

    #[async_trait::async_trait]
    impl crate::player::Player for MutePlayer {
        async fn play(&self, _uri: &str) -> Result<()> {
            Ok(())
        }
        async fn load_list(&self, _uri: &str) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<()> {
            Ok(())
        }
        async fn toggle_pause(&self) -> Result<()> {
            Ok(())
        }
        async fn next(&self) -> Result<()> {
            Ok(())
        }
        async fn prev(&self) -> Result<()> {
            Ok(())
        }
        async fn set_playlist_pos(&self, _n: i64) -> Result<()> {
            Ok(())
        }
        async fn set_volume(&self, _volume: u8) -> Result<()> {
            Ok(())
        }
        async fn set_mute(&self, _mute: bool) -> Result<()> {
            Ok(())
        }
        async fn set_audio_device(&self, _device: &str) -> Result<()> {
            Ok(())
        }
        async fn progress(&self) -> Result<crate::player::Progress> {
            Ok(crate::player::Progress::default())
        }
        async fn seek_relative(&self, _delta_s: i64) -> Result<()> {
            Ok(())
        }
        async fn seek_absolute(&self, _position_s: u32) -> Result<()> {
            Ok(())
        }
    }

    struct Bench {
        children: HotPlugChildren,
        core: core::Core<MutePlayer>,
        gathered: register::Gathered,
        kill_triggers: HashMap<String, tokio::sync::oneshot::Sender<()>>,
        non_supervised: HashSet<String>,
        /// Held until the end of the test: `state_path` and `locales_root`
        /// depend on it.
        _dir: tempfile::TempDir,
    }

    /// An `mpd` plugin **announced and wired**, in the state `hotplug`
    /// leaves it in: present in `announcements`, a connected status line,
    /// and the manifest recognizing its name.
    ///
    /// It is this state that makes the test realistic. A
    /// `Gathered::default()` would prove a refusal on a shape the producer
    /// never emits.
    fn bench() -> Bench {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let (now_playing_tx, now_playing_rx) = watch::channel(NowPlaying::default());
        let (state_tx, state_rx) = watch::channel(PlayerState::default());
        let (sources_catalog_tx, catalog_rx) = watch::channel(SourcesCatalog::default());

        let covers = Arc::new(CoverCache::new());
        let catalog = Arc::new(RwLock::new(ritornello_i18n::Catalog::load(
            "core",
            "en",
            &root,
            crate::i18n::EN,
        )));

        let core = core::Core::new(
            MutePlayer,
            Wiring {
                sources: HashMap::new(),
                persisted: Default::default(),
                state_path: root.join("state.json"),
                catalog,
                locales_root: root.clone(),
                sources_catalog: sources_catalog_tx,
                metadata: MetadataWiring {
                    plugins: vec![],
                    now_playing: now_playing_tx,
                    state: state_tx,
                },
            },
            covers.clone(),
            mpsc::channel(4).0,
            mpsc::channel(4).0,
        );

        let children = HotPlugChildren {
            sockets_dir: root.clone(),
            manifest_order: vec!["mpd".to_string()],
            source_update_tx: mpsc::channel(4).0,
            cmd_tx: mpsc::channel(4).0,
            enrich_tx: mpsc::channel(4).0,
            unreachable_tx: mpsc::channel(4).0,
            now_playing_rx,
            state_rx,
            catalog_rx,
            covers,
            status_state: Arc::new(RwLock::new(StatusState {
                // The constructor and not a literal: a field added to
                // `PluginStatus` must not break this bench, whose subject
                // is not the shape of the status line.
                plugins: vec![PluginStatus::kind("mpd", "display", true, true)],
                active_source: String::new(),
            })),
            admin_backends: Arc::new(RwLock::new(HashMap::new())),
            admin_assets: Arc::new(Default::default()),
        };

        let mut gathered = register::Gathered::default();
        gathered.announcements.insert(
            "mpd".to_string(),
            Announcement {
                name: "mpd".into(),
                kinds: vec![PluginKind::Display],
                admin: true,
                covers: false,
            },
        );

        Bench {
            children,
            core,
            gathered,
            kill_triggers: HashMap::new(),
            non_supervised: HashSet::new(),
            _dir: dir,
        }
    }

    async fn line(b: &Bench) -> PluginStatus {
        let statuses = b.children.status_state.read().await;
        statuses.plugins.iter().find(|l| l.name == "mpd").cloned().expect("line mpd")
    }

    async fn turn_off(b: &mut Bench) -> bool {
        hot_unplug(
            "mpd",
            &b.children,
            &mut b.core,
            &mut b.gathered,
            &mut b.kill_triggers,
            &b.non_supervised,
        )
        .await
    }

    fn statuses_of(lines: Vec<PluginStatus>) -> StatusState {
        StatusState { plugins: lines, active_source: String::new() }
    }

    /// The right word at the right time: "starting" observes, "stalled"
    /// accuses. Both say the plugin has not spoken, and swapping them would
    /// accuse a perfectly healthy binary — the defect this reports in
    /// practice.
    #[test]
    fn the_startup_line_is_not_the_stalled_line() {
        let d = PluginStatus::startup("mpd");
        assert!(d.starting, "it must say \"starting\"");
        assert!(!d.stalled, "and definitely not \"stalled\" at the same time");
        assert!(!d.connected);
        assert!(!d.disabled);

        let f = PluginStatus::unknown_kind("mpd", true);
        assert!(f.stalled);
        assert!(!f.starting, "the two states are exclusive");
    }

    /// The property that matters about the startup deadline: it **never**
    /// replaces true information with an accusation.
    #[test]
    fn the_deadline_only_downgrades_what_is_still_starting() {
        assert!(
            should_downgrade(&statuses_of(vec![PluginStatus::startup("mpd")]), "mpd"),
            "a plugin still silent at the deadline must switch to \"stalled\""
        );
        assert!(
            !should_downgrade(&statuses_of(vec![PluginStatus::kind("mpd", "display", true, true)]), "mpd"),
            "it announced itself in the meantime: its line describes its kinds, do not overwrite it"
        );
        assert!(
            !should_downgrade(&statuses_of(vec![PluginStatus::kind("mpd", "display", false, true)]), "mpd"),
            "announced then died: the line says \"disconnected\", truer than \"stalled\""
        );
        assert!(
            !should_downgrade(&statuses_of(vec![PluginStatus::disabled("mpd")]), "mpd"),
            "turned off from the UI during its startup: \"disabled\" must hold"
        );
        assert!(
            !should_downgrade(&statuses_of(vec![PluginStatus::unknown_kind("mpd", true)]), "mpd"),
            "already stalled: nothing to do, and definitely not a second log line"
        );
        assert!(
            !should_downgrade(&statuses_of(vec![]), "mpd"),
            "no line left for this name: nothing to downgrade"
        );
    }

    #[test]
    fn a_plugin_launched_by_the_core_is_supervised() {
        let mut kt = HashMap::new();
        kt.insert("mpd".to_string(), tokio::sync::oneshot::channel::<()>().0);
        assert_eq!(liveness("mpd", &kt, &HashSet::new()), Liveness::Supervised);
    }

    #[test]
    fn a_plugin_announced_outside_supervision_is_out_of_reach() {
        let non_supervised: HashSet<String> = ["mpd".to_string()].into_iter().collect();
        assert_eq!(
            liveness("mpd", &HashMap::new(), &non_supervised),
            Liveness::OutOfReach
        );
    }

    #[test]
    fn a_name_absent_from_both_registries_is_off() {
        assert_eq!(liveness("mpd", &HashMap::new(), &HashSet::new()), Liveness::Off);
    }

    /// The conjunction is unreachable in production (see `liveness`'s doc),
    /// but the function is total: that is what this test says, and it pins
    /// the order for whoever adds a registry.
    #[test]
    fn what_the_core_can_stop_takes_priority_over_what_it_observes() {
        let mut kt = HashMap::new();
        kt.insert("mpd".to_string(), tokio::sync::oneshot::channel::<()>().0);
        let non_supervised: HashSet<String> = ["mpd".to_string()].into_iter().collect();
        assert_eq!(liveness("mpd", &kt, &non_supervised), Liveness::Supervised);
    }

    /// The positive control: the core holds the trigger, so it really turns
    /// off. Without this test, the refusal test would also pass with a
    /// function that **always** refuses.
    #[tokio::test]
    async fn a_supervised_plugin_turns_off_and_its_announcement_disappears() {
        let mut b = bench();
        b.kill_triggers.insert("mpd".to_string(), tokio::sync::oneshot::channel::<()>().0);

        assert!(turn_off(&mut b).await, "turning off a supervised plugin must succeed");
        assert!(!b.kill_triggers.contains_key("mpd"), "the trigger is consumed");
        assert!(!b.gathered.announcements.contains_key("mpd"), "the announcement is removed");
        assert!(line(&b).await.disabled, "the status line says \"disabled\"");
    }

    /// The observation itself. A living process the core cannot stop: the
    /// toggle must return `false` **and** have touched nothing.
    ///
    /// The three stillness assertions are not decorative. A fix that logged
    /// then unwired anyway would pass the first and fail on the following
    /// ones — and this is exactly the half-measure that produces the status
    /// page's most misleading state.
    #[tokio::test]
    async fn a_plugin_out_of_reach_is_not_turned_off_and_nothing_is_unwired() {
        let mut b = bench();
        b.non_supervised.insert("mpd".to_string());

        assert!(
            !turn_off(&mut b).await,
            "reporting a shutdown that was not obtained is the defect to fix"
        );
        assert!(
            b.gathered.announcements.contains_key("mpd"),
            "the announcement stays: the plugin is still running"
        );
        let l = line(&b).await;
        assert!(!l.disabled, "the page must not show it disabled");
        assert!(l.connected, "it is still reachable, the line says so");
    }

    /// Turning off an already-off plugin remains a success: the requester
    /// wanted this state, they get it. Symmetric to the turn-on's non-event,
    /// and what distinguishes `Off` from `OutOfReach` — without which a
    /// double click on "disable" would surface an error.
    #[tokio::test]
    async fn turning_off_an_already_off_plugin_succeeds() {
        let mut b = bench();
        assert!(turn_off(&mut b).await);
        assert!(line(&b).await.disabled);
    }
}

#[cfg(test)]
mod relay_tests {
    //! The display relay, tested on the real path: a SDK `DisplayClient` on
    //! one side, `serve_display` on the other, and between the two exactly
    //! the function `main` calls.
    //!
    //! No time margin anywhere. The positive direction is proven by
    //! **waiting** for what must arrive (a channel, so the wait is exact).
    //! The negative direction — "nothing arrives" — cannot be proven by
    //! waiting: it is proven by a **witness frame** sent afterwards, on the
    //! same socket. Frames arrive there in order and `serve_display`
    //! processes them in order, so seeing the witness proves that whatever
    //! preceded it was already processed — or was never sent.

    use super::*;
    use crate::cover::{fixtures, CoverCache, CoverPayload};
    use ritornello_plugin_sdk::{bind_display, serve_display, DisplayPlugin};
    use ritornello_proto::Cover;

    #[derive(Debug, PartialEq)]
    enum Received {
        State(Box<PlayerState>),
        SourcesCatalog(SourcesCatalog),
        CoverPayload(Cover),
    }

    /// A display that handles **everything**: this is deliberate. If the
    /// negative direction were proven by a display unable to receive a
    /// cover, it would prove nothing about the core's filter — only about
    /// the stub.
    struct Stub {
        tx: mpsc::UnboundedSender<Received>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for Stub {
        async fn show(&mut self, state: PlayerState) -> Result<()> {
            let _ = self.tx.send(Received::State(Box::new(state)));
            Ok(())
        }
        async fn sources_catalog(&mut self, c: SourcesCatalog) -> Result<()> {
            let _ = self.tx.send(Received::SourcesCatalog(c));
            Ok(())
        }
        fn wants_covers(&self) -> bool {
            true
        }
        async fn cover(&mut self, c: Cover) -> Result<()> {
            let _ = self.tx.send(Received::CoverPayload(c));
            Ok(())
        }
    }

    /// A minimal JPEG header followed by padding.
    /// **Deliberately undecodable**: only fit for size and cap tests, where
    /// the image is never decoded. Everything that goes through
    /// `CoverCache::line` must pass through `fixtures::jpeg_decodable`, the
    /// rendition being active by default.
    fn jpeg(padding: usize) -> Vec<u8> {
        let mut v = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        v.resize(6 + padding, 0x42);
        v
    }

    /// A state **as the core emits it**: `cover_href` in it is always of the
    /// form `/api/cover/{key}`, and the key designates a cache entry. A
    /// `Default::default()` with an invented `cover_href` would prove a
    /// causality in a frame the producer cannot produce.
    fn state_with_cover(key: &str) -> PlayerState {
        PlayerState {
            source: "files".into(),
            track: ritornello_proto::Track {
                cover_href: Some(format!("{}{key}", cover::HREF_PREFIX)),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Sets up a display served by the SDK, wires the relay onto it, and
    /// returns what is needed to drive the state and read what the display
    /// receives.
    struct Bench {
        state_tx: watch::Sender<PlayerState>,
        received: mpsc::UnboundedReceiver<Received>,
        /// The last state pushed. A witness is derived from it, to differ
        /// from it only by a field unrelated to the cover (see `witness`).
        last: PlayerState,
        _catalog_tx: watch::Sender<SourcesCatalog>,
        _dir: tempfile::TempDir,
    }

    async fn bench(
        wants_covers: bool,
        covers: Arc<CoverCache>,
        initial_state: PlayerState,
    ) -> Bench {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = bind_display(&socket).unwrap();
        let (tx, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = serve_display(listener, Stub { tx }).await;
        });
        let client = DisplayClient::connect(&socket).await.unwrap();
        let (state_tx, state_rx) = watch::channel(initial_state.clone());
        let (sources_catalog_tx, catalog_rx) = watch::channel(SourcesCatalog::default());
        // The bench says nothing of the unreachable notice: receiver dropped
        // right away, so the relay's end-of-life send fails and is ignored,
        // just as it is in service when the core loop is already gone. Same
        // idiom as `hotplug`'s bench channels.
        display_relay(
            "bench".into(),
            client,
            wants_covers,
            covers,
            state_rx,
            catalog_rx,
            UnreachableNotice { wiring: 0, tx: mpsc::channel(4).0 },
        );
        let mut b = Bench {
            state_tx,
            received,
            last: initial_state,
            _catalog_tx: sources_catalog_tx,
            _dir: dir,
        };
        // Wait for the relay to have consumed the initial value **before**
        // returning the bench. A `watch` only keeps the latest value:
        // without this wait, a test `send` could overwrite the initial state
        // before the relay's `borrow_and_update()`, and the state carrying
        // the cover would never have existed for it. This is not a time
        // margin but an exact synchronization — the sources catalog is the
        // **second** frame the relay sends, so having seen it proves the
        // initial state went through. The initial cover, meanwhile, leaves
        // right after: it therefore still needs collecting, which is what
        // `witness` does.
        loop {
            match b.received.recv().await.expect("the relay must send the state then the sources catalog") {
                Received::SourcesCatalog(_) => break,
                Received::State(_) => {}
                other => panic!("unexpected frame before the sources catalog: {other:?}"),
            }
        }
        b
    }

    /// Closes a collection: sends a witness state and returns everything
    /// that arrived before it.
    ///
    /// The witness differs from the last state only by its **volume**, so it
    /// carries the same `cover_href`. This is necessary: a witness with no
    /// cover would reset the relay's deduplication guard, and the cover
    /// would go out again on the next state — which would mask exactly the
    /// property these tests want to see.
    async fn witness(bench: &mut Bench) -> Vec<Received> {
        let mut t = bench.last.clone();
        t.volume = t.volume.wrapping_add(1);
        bench.last = t.clone();
        bench.state_tx.send(t.clone()).unwrap();
        let mut before = Vec::new();
        loop {
            match bench.received.recv().await.expect("the relay must stay alive") {
                Received::State(e) if *e == t => return before,
                other => before.push(other),
            }
        }
    }

    /// Triggers a state change and returns **exactly** what it triggered.
    ///
    /// Two synchronizations, and both are necessary. The first waits for the
    /// arrival of *this* state: a `watch` only keeps the latest value, so
    /// sending the witness before having seen this one could erase it
    /// without it ever having existed for the relay. The second is the
    /// witness, which closes the collection.
    ///
    /// **A cover frame can arrive late, and it took an intermittent failure
    /// to admit it.** The original reasoning said "the previous window was
    /// closed by its own witness, so nothing remains in flight" and panicked
    /// on any other frame. That is false: `witness` returns control as soon
    /// as it sees **its** state frame, yet the relay then chains into its
    /// cover step for that witness. A witness whose cover is awaiting a
    /// retry therefore triggers activity that is still alive after it
    /// returns — and if the test has meanwhile put the file back in place,
    /// that retry **succeeds** and its frame shows up just before the next
    /// state. Wrong framing, not an anomaly.
    ///
    /// Hence the deliberate asymmetry below: an early **cover** is folded
    /// into the window (it is the delayed consequence of the previous
    /// change, and counting it here is what the assertions want — the relay
    /// deduplicates afterwards, so it cannot count twice), while an
    /// unexpected **state** remains an anomaly and panics: the order of
    /// states has no reason to drift.
    async fn trigger(bench: &mut Bench, state: PlayerState) -> Vec<Received> {
        bench.last = state.clone();
        bench.state_tx.send(state.clone()).unwrap();
        let mut before = Vec::new();
        loop {
            match bench.received.recv().await.expect("the relay must stay alive") {
                Received::State(e) if *e == state => break,
                cover @ Received::CoverPayload(_) => before.push(cover),
                other => panic!("unexpected frame before the sent state: {other:?}"),
            }
        }
        before.extend(witness(bench).await);
        before
    }

    fn covers_from(received: &[Received]) -> Vec<&Cover> {
        received
            .iter()
            .filter_map(|r| match r {
                Received::CoverPayload(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_display_that_did_not_ask_for_covers_receives_none() {
        // **The property that protects the console.** The stub knows how to
        // receive a cover; it is the core that must not send it one.
        let covers = Arc::new(CoverCache::new());
        covers
            .insert("abcd".into(), CoverPayload::Bytes(fixtures::jpeg_decodable(48, 48), "image/jpeg"))
            .await;
        let mut b = bench(false, covers, state_with_cover("abcd")).await;

        let received = witness(&mut b).await;
        assert!(
            covers_from(&received).is_empty(),
            "no cover must reach a display that did not ask for any: {received:?}"
        );
    }

    #[tokio::test]
    async fn a_display_that_asked_receives_the_bytes_and_the_state_href() {
        let image = fixtures::jpeg_decodable(48, 48);
        let covers = Arc::new(CoverCache::new());
        covers.insert("abcd".into(), CoverPayload::Bytes(image.clone(), "image/png")).await;
        let mut b = bench(true, covers, state_with_cover("abcd")).await;

        let received = witness(&mut b).await;
        let seen = covers_from(&received);
        assert_eq!(seen.len(), 1, "one cover, only one: {received:?}");
        assert_eq!(seen[0].bytes, image);
        assert_eq!(seen[0].mime, "image/png");
        assert_eq!(
            seen[0].href,
            format!("{}abcd", cover::HREF_PREFIX),
            "the href must be exactly that of the state frame, otherwise the display \
             cannot correlate the image with what is playing"
        );
    }

    #[tokio::test]
    async fn the_cover_does_not_go_out_again_while_unchanged() {
        // A state frame goes out up to once per second during playback.
        // Without this guard, every second of playback would push the whole
        // image — and redo the local-file read that produces it.
        let covers = Arc::new(CoverCache::new());
        covers
            .insert("abcd".into(), CoverPayload::Bytes(fixtures::jpeg_decodable(48, 48), "image/jpeg"))
            .await;
        covers
            .insert("efgh".into(), CoverPayload::Bytes(fixtures::jpeg_decodable(64, 64), "image/jpeg"))
            .await;
        let mut b = bench(true, covers, state_with_cover("abcd")).await;

        // The initial cover, which goes out with the first state.
        let received = witness(&mut b).await;
        assert_eq!(covers_from(&received).len(), 1, "the initial cover: {received:?}");

        // The same `cover_href`, but a different state (the volume): the
        // state frame goes out again, the cover does not.
        let mut again = state_with_cover("abcd");
        again.volume = 42;
        let received = trigger(&mut b, again).await;
        assert!(
            covers_from(&received).is_empty(),
            "an unchanged cover must not go out again with every state frame: {received:?}"
        );

        // A different key, on the other hand, is a different image: it must
        // go out.
        let received = trigger(&mut b, state_with_cover("efgh")).await;
        let seen = covers_from(&received);
        assert_eq!(seen.len(), 1, "a cover change must push one: {received:?}");
        assert_eq!(seen[0].href, format!("{}efgh", cover::HREF_PREFIX));
    }

    #[tokio::test]
    async fn a_cover_beyond_the_cap_is_not_pushed_and_the_relay_survives() {
        // The defined consequence of the cap, seen from the relay: nothing
        // is pushed, and above all the task keeps serving the state — a
        // cover refusal is not a send failure, otherwise the display would
        // lose *everything* for the rest of the process.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.jpg");
        std::fs::write(&path, jpeg(ritornello_proto::COVER_MAX_BYTES)).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insert("abcd".into(), CoverPayload::File(path)).await;
        let mut b = bench(true, covers, state_with_cover("abcd")).await;

        let received = witness(&mut b).await;
        assert!(covers_from(&received).is_empty(), "beyond the cap, nothing must go out: {received:?}");
        // The witness arrived, so the relay is alive: that is the other half
        // of the property, and `witness` would have blocked indefinitely
        // otherwise.
    }

    #[tokio::test]
    async fn an_href_with_no_cached_cover_does_not_break_the_relay() {
        // The cache is bounded (`ENTRIES` entries): the key published in the
        // state may have been evicted in the meantime.
        let covers = Arc::new(CoverCache::new());
        let mut b = bench(true, covers, state_with_cover("evicted")).await;
        let received = witness(&mut b).await;
        assert!(covers_from(&received).is_empty(), "{received:?}");
    }

    #[tokio::test]
    async fn a_transient_failure_is_retried_and_the_cover_eventually_goes_out() {
        // **The property the defect broke, and nothing else.** `push_cover`
        // marked the attempt as done *before* actually making it: a single
        // exceeded timeout on a sleeping SMB share sacrificed the cover for
        // **the whole track**, because the deduplication guard then
        // considered the matter settled. Yet this is exactly the case where
        // a second attempt succeeds — a woken-up share answers on the
        // second access.
        //
        // The failure is triggered by the file's disappearance, which
        // `read_file_bounded` treats like any IO that does not complete. The
        // sequence is the production one: the entry is inserted while the
        // file exists (`fetch` read its header before inserting), the share
        // goes away, then it comes back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let image = fixtures::jpeg_decodable(48, 48);
        std::fs::write(&path, &image).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insert("abcd".into(), CoverPayload::File(path.clone())).await;
        // The share goes to sleep: the file is no longer readable.
        std::fs::remove_file(&path).unwrap();

        let mut b = bench(true, covers, state_with_cover("abcd")).await;
        let received = witness(&mut b).await;
        assert!(
            covers_from(&received).is_empty(),
            "first attempt: the file is unreadable, nothing must go out: {received:?}"
        );

        // The share comes back. The `cover_href` did **not** change — that
        // is the whole point: with the old code, the guard held it as
        // already handled and no reread ever happened until the next track.
        std::fs::write(&path, &image).unwrap();
        let mut again = state_with_cover("abcd");
        again.volume = 42;
        let received = trigger(&mut b, again).await;
        let seen = covers_from(&received);
        assert_eq!(seen.len(), 1, "the second attempt must push the cover: {received:?}");
        assert_eq!(seen[0].bytes, image);
    }

    #[tokio::test]
    async fn a_permanent_failure_is_not_retried_forever() {
        // The other half of the trade-off, and it counts just as much: a
        // state frame goes out up to once per second during playback, so
        // retrying without a bound would reread an absent file once per
        // second for the rest of the track. The budget is `COVER_ATTEMPTS`
        // attempts, and it runs out.
        //
        // Proof with no time margin: the file is put back **after** the
        // budget is exhausted, and the cover must then no longer go out. If
        // the budget did not exist, it would go out.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let image = fixtures::jpeg_decodable(48, 48);
        std::fs::write(&path, &image).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insert("abcd".into(), CoverPayload::File(path.clone())).await;
        std::fs::remove_file(&path).unwrap();

        // One attempt goes out with the initial state, one per witness
        // below: three attempts total, i.e. the whole budget.
        let mut b = bench(true, covers, state_with_cover("abcd")).await;
        for _ in 0..3 {
            let received = witness(&mut b).await;
            assert!(covers_from(&received).is_empty(), "nothing must go out while the file is missing");
        }

        std::fs::write(&path, &image).unwrap();
        let received = witness(&mut b).await;
        assert!(
            covers_from(&received).is_empty(),
            "this cover's budget is exhausted: no more reread must happen \
             for this href, {received:?}"
        );
    }

    #[tokio::test]
    async fn a_rewired_display_receives_the_current_image_not_the_previous_one() {
        // **The scenario of the most serious finding, end to end.** Three
        // user clicks: disable the display from the admin page, replace the
        // cover on the share, re-enable it. The second relay starts over
        // with its deduplication guard at zero and asks again for the
        // current cover — same key, since the key hashes the *path*.
        //
        // An encoded line kept from one call to the next then served the
        // previous image, and nothing could invalidate it: replacing a file
        // on a share goes through none of our code, and no `insert` happens
        // here. Two successive benches on the **same** `CoverCache`
        // reproduce exactly the unwiring then the rewiring.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let before = fixtures::jpeg_decodable(48, 48);
        let after = fixtures::jpeg_decodable(64, 64);
        std::fs::write(&path, &before).unwrap();
        let covers = Arc::new(CoverCache::new());
        covers.insert("abcd".into(), CoverPayload::File(path.clone())).await;

        let mut first = bench(true, covers.clone(), state_with_cover("abcd")).await;
        let received = witness(&mut first).await;
        let seen = covers_from(&received);
        assert_eq!(seen.len(), 1, "the initial cover: {received:?}");
        assert_eq!(seen[0].bytes, before);
        // The display is disabled: its relay goes away with its bench.
        drop(first);

        // The user replaces the cover they did not like.
        std::fs::write(&path, &after).unwrap();

        // Then they re-enable the display: new relay, same cache, same key.
        let mut second = bench(true, covers, state_with_cover("abcd")).await;
        let received = witness(&mut second).await;
        let seen = covers_from(&received);
        assert_eq!(seen.len(), 1, "the rewired display must receive the current cover: {received:?}");
        assert_eq!(
            seen[0].bytes, after,
            "and it must be the share's current image, not the one the cache had encoded \
             before the replacement"
        );
    }
}
