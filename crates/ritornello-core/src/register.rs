//! Gathering of the plugins' announcements.
//!
//! The core binds a socket before launching anything, then waits for one
//! announcement per launched plugin. Since the plugin binds its own sockets
//! **before** announcing itself, the received line is an availability barrier:
//! the core can connect behind it without retrying. This is what replaces the
//! two guessed waits of before — the 2 s window of the admin page and the
//! 10 s of connection retries.

use futures::{Stream, StreamExt};
use ritornello_proto::{Announcement, PluginKind};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

/// What the gathering learned.
///
/// The two lists of silent plugins are **separate** because the core does two
/// different things with them: a stalled one keeps a chance to speak — the
/// register socket stays open for the whole life of the process — whereas a
/// dead one has none left. Confusing them meant reporting the same status line
/// for two failures that are not fixed the same way.
#[derive(Debug, Default)]
pub struct Gathered {
    /// Announced, by name.
    pub announcements: HashMap<String, Announcement>,
    /// Launched, never announced, and whose death was **not** observed: alive
    /// and silent at the deadline. Named, so that the log points at a culprit
    /// instead of leaving it to be deduced.
    pub stalled: Vec<String>,
    /// Launched and dead without leaving a usable announcement: either dead
    /// before speaking, or dead during the gathering after having spoken (their
    /// announcement is then withdrawn, see the deaths branch).
    pub dead: Vec<String>,
}

/// Time given to a connection to write its announcement line.
///
/// An announcement is written right after the `connect` by the SDK: a few
/// seconds cover a loaded device with a wide margin, and whatever has said
/// nothing after that timeout is not a slow plugin but a faulty one.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads **one** announcement line on an accepted connection, decodes it, and
/// pushes it into the announcements channel.
///
/// One task per connection: a silent connection must delay neither the
/// rendezvous nor the late announcements.
///
/// The read is **bounded** in time. Without that, every silent connection
/// pinned a task and a descriptor for the life of the process: a plugin with a
/// reconnection bug, hitting the socket once per second without ever writing,
/// ended up exhausting the descriptors — and the core, whose `accept` then
/// failed permanently, could no longer wire **any** announcement on a device
/// that is never rebooted. The gathering had the same reader but its deadline
/// bounded it; the permanent loop has none.
async fn read_announcement(
    stream: tokio::net::UnixStream,
    tx: tokio::sync::mpsc::Sender<Announcement>,
    timeout: Duration,
) {
    let mut lines = BufReader::new(stream).lines();
    match tokio::time::timeout(timeout, lines.next_line()).await {
        Ok(Ok(Some(l))) => match serde_json::from_str::<Announcement>(&l) {
            Ok(a) => {
                let _ = tx.send(a).await;
            }
            Err(e) => tracing::warn!("unreadable announcement ignored ({e}): {l}"),
        },
        Ok(Ok(None)) => {
            tracing::warn!("a plugin connected to the register socket and said nothing")
        }
        Ok(Err(e)) => tracing::warn!("reading an announcement failed: {e}"),
        // The connection is dropped on the way out: the task and the descriptor
        // are given back, which is the whole point of the timeout.
        Err(_) => tracing::warn!(
            "a plugin held the register socket open for {}s without announcing, dropping it",
            timeout.as_secs()
        ),
    }
}

/// Waits for one announcement per launched plugin.
///
/// Returns as soon as every expected plugin is either announced or dead — so
/// in practice well before `deadline`. A delay is only paid on failure.
///
/// `announcements_tx` / `announcements_rx` are the **single channel of both
/// stages**: the one `accept_forever` will feed afterwards, and the one the
/// main loop consumes. The gathering has no channel of its own, and that is
/// what makes an announcement unlosable: when an announcement and the deadline
/// are ready at the same instant, `tokio::select!` picks at random, and the
/// draw now only decides the path. What `gather` does not consume — the
/// announcement ready at the deadline, and those of already accepted
/// connections whose read task has not completed yet — **stays queued** for
/// hot wiring. With a channel private to the gathering, destroyed on its
/// return, it left with the receiver: the plugin, which announces only once,
/// believed itself registered and waited for the next service restart, without
/// a single log line.
pub async fn gather<S>(
    listener: &UnixListener,
    expected: &[String],
    deaths: S,
    deadline: Duration,
    announcements_tx: &tokio::sync::mpsc::Sender<Announcement>,
    announcements_rx: &mut tokio::sync::mpsc::Receiver<Announcement>,
) -> Gathered
where
    S: Stream<Item = String> + Unpin,
{
    // `remaining` = those still awaited. An early death leaves it (stop
    // waiting) but remains a silent one: the two lists of silent plugins are
    // therefore computed at the end from `expected`, and not taken from
    // `remaining` — otherwise a plugin dead before announcing vanished from the
    // report, exactly the diagnosis this gathering exists to name.
    let mut remaining: Vec<String> = expected.to_vec();
    let mut announcements: HashMap<String, Announcement> = HashMap::new();
    // The **observed** deaths. This is what separates a living silent plugin
    // from a dead one: without this trace, the deadline could only deduce, and
    // a merely slow plugin would be reported as a lost one.
    let mut deaths_seen: Vec<String> = Vec::new();
    let mut deaths = deaths.fuse();
    let end = tokio::time::sleep(deadline);
    tokio::pin!(end);

    // **One read task per connection**, and not an inline read in the `accept`
    // branch: a plugin that connects then writes nothing must not delay the
    // announcement of the others. Head-of-line blocking on the rendezvous would
    // be the very defect the protocol refuses elsewhere.
    //
    // It is the same task as `accept_forever`'s, towards the same channel: a
    // connection accepted here but read after `gather` returns is not lost for
    // that, its announcement simply waits in the queue.
    //
    // The original sender lives with the caller, beyond this function:
    // `recv()` therefore never returns `None`, and its `select!` branch never
    // disarms.
    while !remaining.is_empty() {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        tokio::spawn(read_announcement(stream, announcements_tx.clone(), READ_TIMEOUT));
                    }
                    Err(e) => tracing::warn!("register socket accept failed: {e}"),
                }
            }
            Some(announcement) = announcements_rx.recv() => {
                // The manifest's name is authoritative: an announcement
                // carrying another one comes from a badly launched binary, or
                // from a plugin inventing its identity. It is named then
                // discarded, never wired.
                if !remaining.contains(&announcement.name) {
                    if announcements.contains_key(&announcement.name) {
                        tracing::warn!("duplicate announcement for {}, ignored", announcement.name);
                    } else {
                        tracing::warn!("announcement from unknown plugin {}, ignored", announcement.name);
                    }
                    continue;
                }
                remaining.retain(|n| n != &announcement.name);
                tracing::info!("{} announced {:?} (admin: {})", announcement.name, announcement.kinds, announcement.admin);
                announcements.insert(announcement.name.clone(), announcement);
            }
            Some(death) = deaths.next() => {
                // The death is **observed** here, and nowhere else: this list
                // is what will allow, below, naming a living silent plugin
                // (stalled) rather than confusing it with a dead one.
                if !deaths_seen.contains(&death) {
                    deaths_seen.push(death.clone());
                }
                // The process left before announcing itself: stop waiting for
                // it. This is what makes a startup crash faster to diagnose
                // than before, when it burned the 10 s of retries for nothing.
                if remaining.contains(&death) {
                    tracing::warn!("plugin {death} exited before announcing");
                    remaining.retain(|n| n != &death);
                } else if announcements.remove(&death).is_some() {
                    // Dead **after** announcing itself, while someone else was
                    // still awaited. Its future left `plugin_waits` by being
                    // consumed here: `main`'s selection loop will never see it
                    // again, nor its exit code, nor its
                    // `mark_plugin_disconnected`. Without this withdrawal, it
                    // would be wired then displayed "connected" for good — the
                    // very silent loss this rendezvous exists to remove, and
                    // all the more for the `input` and `metadata` kinds whose
                    // status is set to true without waiting for the task.
                    //
                    // Removing it from the announcements is enough: the silent
                    // ones being computed at the end by difference, it falls
                    // back on its own into `dead` — its death has just been
                    // observed — and `main` sets it `connected: false` like the
                    // others.
                    //
                    // Log line distinct from the one above: "dead before
                    // announcing" and "dead during the gathering" are not the
                    // same failure.
                    tracing::warn!("plugin {death} exited during registration");
                }
            }
            () = &mut end => {
                tracing::warn!("register deadline reached, still waiting for: {}", remaining.join(", "));
                break;
            }
        }
    }

    // In the order of `expected`, hence in the manifest's order: the log names
    // the culprits in the order the operator declared them (`partition`
    // preserves the source order).
    //
    // The partition is made on the **observed** death, never on the deadline:
    // a plugin whose process nobody saw exit is presumed alive, hence
    // stalled, hence still hot-wirable.
    let (dead, stalled): (Vec<String>, Vec<String>) = expected
        .iter()
        .filter(|name| !announcements.contains_key(*name))
        .cloned()
        .partition(|name| deaths_seen.contains(name));

    Gathered { announcements, stalled, dead }
}

/// Keeps accepting on the register socket **for the whole life of the
/// process**, and pushes every readable announcement into `tx`.
///
/// This is what strips `gather`'s deadline of its power to condemn: it now
/// only serves to avoid blocking startup and to name a stalled plugin. The
/// core owns this socket, so it can listen as long as it lives — a plugin that
/// announces itself at t+12 s, or that is restarted by hand a month later, is
/// hot-wired instead of being lost until the next service restart.
///
/// Only returns if `tx` is closed, that is if the main loop is dead: nobody
/// left to wire anything.
pub async fn accept_forever(listener: UnixListener, tx: tokio::sync::mpsc::Sender<Announcement>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                // **One read task per connection**, as in `gather`: a silent
                // connection must not block the late announcements any more
                // than it blocked the initial ones. Head-of-line blocking was
                // already fixed once on this socket, it is not reintroduced
                // here.
                tokio::spawn(read_announcement(stream, tx.clone(), READ_TIMEOUT));
            }
            Err(e) => {
                tracing::warn!("register socket accept failed: {e}");
                // This loop is bounded by no deadline, unlike `gather`'s: a
                // lasting error — no free descriptor left — would make it spin
                // for nothing at full load on a device that only has a small
                // processor. A breath before retrying.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// The `metadata` plugins, **in the manifest's order**.
///
/// The file's order is the arbitration priority: between two plugins that
/// answer for the same track, the first declared wins. Before, the list was
/// built from the manifest before any launch, so the order was acquired by
/// construction; it is now rebuilt here, and a sort by arrival order of the
/// announcements would make the display non-reproducible from one startup to
/// the next. Never sort this list any other way.
pub fn metadata_order(manifest: &[String], g: &Gathered) -> Vec<String> {
    manifest
        .iter()
        .filter(|name| {
            g.announcements
                .get(*name)
                .is_some_and(|a| a.kinds.contains(&PluginKind::Metadata))
        })
        .cloned()
        .collect()
}

/// Is there still, at the deadline, a **living** plugin process — hence one
/// that may announce itself later?
///
/// This is the only condition that still prevents the core from starting. A
/// slow plugin is no longer an error: the register socket stays open, an
/// announcement at t+30 s is hot-wired, and the status page must precisely be
/// **there** to show that stalled plugin. Refusing to start at t+10 s removed
/// it at the moment one wanted to consult it, and systemd looped without
/// fixing anything.
///
/// But if nothing runs anymore — empty `plugins.toml`, executables not found,
/// or all dead before the deadline — nobody will ever announce. That is a
/// configuration error, not slowness, and silently starting a device that will
/// never play anything helps nobody.
///
/// Deduced from `launched` and `dead` rather than from `announcements` and
/// `stalled`: we do not assume the three collections partition `launched`, we
/// only exclude what was **observed** dying.
pub fn a_live_plugin(launched: &[String], g: &Gathered) -> bool {
    launched.iter().any(|name| !g.dead.contains(name))
}

/// Must startup be refused?
///
/// `a_live_plugin` is no longer enough since a plugin can be switched off:
/// switching everything off launches no process, and the refusal would then
/// put the core in a systemd restart loop — **UI included**, hence with no
/// means left to switch anything back on. Everything off is a configuration,
/// not a failure.
///
/// The refusal only remains for what it targeted: plugins declared active,
/// and not a single living process left to announce itself.
pub fn startup_refused(declared_active: usize, launched: &[String], g: &Gathered) -> bool {
    declared_active > 0 && !a_live_plugin(launched, g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::PluginKind;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    /// Writes an announcement on the register socket, as a plugin would, then
    /// closes.
    async fn announcement(register: &std::path::Path, line: &str) {
        let mut s = UnixStream::connect(register).await.unwrap();
        s.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        s.shutdown().await.unwrap();
    }

    fn no_deaths() -> impl futures::Stream<Item = String> + Unpin {
        futures::stream::pending()
    }

    /// The single channel of both stages, set up as in `main`: `gather`
    /// borrows it, `accept_forever` keeps its sender, and the main loop
    /// consumes what the gathering left behind.
    fn channel() -> (
        tokio::sync::mpsc::Sender<Announcement>,
        tokio::sync::mpsc::Receiver<Announcement>,
    ) {
        tokio::sync::mpsc::channel(16)
    }

    #[tokio::test]
    async fn gathers_every_announcement_and_returns_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            announcement(&r, r#"{"name":"radio","kinds":["source"],"admin":true}"#).await;
            announcement(&r, r#"{"name":"console","kinds":["display"]}"#).await;
        });

        let start = std::time::Instant::now();
        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string(), "console".to_string()],
            no_deaths(),
            // One hour: the deadline is out of reach, so the only way for this
            // function to return is to have gathered everyone. The clock
            // margin below then only has to tell "returned at once" from
            // "waited an hour", instead of arbitrating between 2 s and 10 s —
            // a ratio the machine's load could cross, and the only fragile
            // link of this test.
            Duration::from_secs(3600),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.announcements.len(), 2);
        assert!(g.stalled.is_empty());
        assert!(g.dead.is_empty());
        assert!(g.announcements["radio"].admin);
        assert_eq!(g.announcements["console"].kinds, vec![PluginKind::Display]);
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the loop must return as soon as everyone is there, not at the deadline"
        );
    }

    #[tokio::test]
    async fn a_silent_plugin_is_named_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            announcement(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string(), "silent".to_string()],
            no_deaths(),
            Duration::from_millis(300),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.announcements.len(), 1);
        // Alive, silent: stalled, and not dead — nobody saw its process
        // exit.
        assert_eq!(g.stalled, vec!["silent".to_string()]);
        assert!(g.dead.is_empty());
    }

    #[tokio::test]
    async fn an_early_death_shortens_the_wait() {
        // Today a crashing plugin burns 10 s of retries for nothing. Here,
        // `child.wait()` must settle it right away.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            announcement(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let start = std::time::Instant::now();
        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string(), "crashed".to_string()],
            Box::pin(futures::stream::iter(vec!["crashed".to_string()])),
            // One hour, out of reach: returning proves that the observed death
            // shortened the wait, without making the test depend on a ratio
            // between two durations that the machine's load could cross.
            Duration::from_secs(3600),
            &tx,
            &mut rx,
        )
        .await;

        // A dead one, not a stalled one: its exit was observed.
        assert_eq!(g.dead, vec!["crashed".to_string()]);
        assert!(g.stalled.is_empty());
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the process death must shorten the wait, not endure it"
        );
    }

    #[tokio::test]
    async fn at_the_deadline_a_living_silent_one_is_stalled_and_a_dead_one_is_not() {
        // Both silent ones in the same gathering: it is the only way to check
        // that the partition separates them, and not that one of the two lists
        // collects everything. `crashed` dies before our eyes, `sleeping` says
        // nothing but nobody saw its process exit — so it may still announce
        // itself, and the core will hot-wire it.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();

        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["crashed".to_string(), "sleeping".to_string()],
            Box::pin(futures::stream::iter(vec!["crashed".to_string()])),
            Duration::from_millis(300),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.dead, vec!["crashed".to_string()]);
        assert_eq!(g.stalled, vec!["sleeping".to_string()]);
    }

    #[tokio::test]
    async fn an_unknown_name_is_ignored_without_blocking_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            announcement(&r, r#"{"name":"intruder","kinds":["source"]}"#).await;
            announcement(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string()],
            no_deaths(),
            Duration::from_secs(5),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.announcements.len(), 1);
        assert!(g.announcements.contains_key("radio"));
        assert!(!g.announcements.contains_key("intruder"));
    }

    #[tokio::test]
    async fn a_death_after_announcing_removes_the_plugin_from_the_gathering() {
        // Real window: a fast plugin announces itself then dies while the core
        // is still waiting for a silent one. Its future left `plugin_waits` by
        // being consumed by the gathering, so `main`'s selection loop will
        // never see it again — neither its exit code nor its
        // `mark_plugin_disconnected`. If it stayed in the announcements it
        // would be wired, then displayed "connected" for good.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            announcement(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        // The death only arrives AFTER the announcement: that is the whole
        // point of the test. An immediate stream would take the other branch
        // ("dead before announcing"), already covered by
        // `an_early_death_shortens_the_wait`.
        let dead = Box::pin(futures::stream::once(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            "radio".to_string()
        }));

        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string(), "silent".to_string()],
            dead,
            Duration::from_millis(800),
            &tx,
            &mut rx,
        )
        .await;

        assert!(
            !g.announcements.contains_key("radio"),
            "a plugin dead during the gathering must not remain wirable"
        );
        assert_eq!(g.dead, vec!["radio".to_string()]);
        assert_eq!(g.stalled, vec!["silent".to_string()]);
    }

    #[tokio::test]
    async fn an_unreadable_announcement_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            announcement(&r, "this is not json").await;
        });

        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string()],
            no_deaths(),
            Duration::from_millis(300),
            &tx,
            &mut rx,
        )
        .await;

        assert!(g.announcements.is_empty());
        // The process is still there: unreadable does not mean dead.
        assert_eq!(g.stalled, vec!["radio".to_string()]);
        assert!(g.dead.is_empty());
    }

    #[tokio::test]
    async fn a_silent_connection_does_not_delay_the_others() {
        // Head-of-line blocking: if the line were read in the `accept` branch,
        // a connected and silent plugin would freeze the announcement of ALL
        // the others until the deadline. That is the defect the read task per
        // connection exists to prevent.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();

        let r = register.clone();
        tokio::spawn(async move {
            // Connects, stays silent, and keeps the connection open.
            let silent = UnixStream::connect(&r).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(silent);
        });
        let r2 = register.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            announcement(&r2, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let start = std::time::Instant::now();
        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string()],
            no_deaths(),
            // One hour, out of reach: if the silent connection blocked the
            // queue, the announcement would never arrive and the margin below
            // would fail loudly, instead of depending on a ratio between 5 s
            // and 30 s that the machine's load could cross.
            Duration::from_secs(3600),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(
            g.announcements.len(),
            1,
            "the announcement must get through despite the silent connection"
        );
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "a silent connection must not delay the gathering"
        );
    }

    #[tokio::test]
    async fn an_announcement_arriving_after_the_gathering_reaches_the_loop() {
        // The case motivating this whole work: the plugin speaks **after**
        // `gather` returns. Before, the socket stopped being read and the
        // announcement was lost until the next service restart.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();

        // The rendezvous ends on a stalled one, without anybody having spoken.
        let (tx, mut rx) = channel();
        let g = gather(
            &listener,
            &["radio".to_string()],
            no_deaths(),
            Duration::from_millis(200),
            &tx,
            &mut rx,
        )
        .await;
        assert_eq!(g.stalled, vec!["radio".to_string()]);

        // The socket, though, keeps being read: `gather` took it by reference,
        // here it is handed to the task that will live as long as the process.
        // The channel is the gathering's: a single channel for both stages.
        tokio::spawn(accept_forever(listener, tx));

        announcement(&register, r#"{"name":"radio","kinds":["source"],"admin":true}"#).await;
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("the late announcement must reach the main loop")
            .unwrap();
        assert_eq!(received.name, "radio");
        assert_eq!(received.kinds, vec![PluginKind::Source]);
        assert!(received.admin);
    }

    #[tokio::test]
    async fn an_announcement_ready_at_the_deadline_is_never_lost() {
        // Trial of the single channel. When an announcement and the deadline
        // are ready at the same instant, `tokio::select!` picks at random: one
        // time out of two the deadline wins. With a channel private to the
        // gathering, destroyed on its return, the announcement then left with
        // the receiver — and the SDK announcing only once, the plugin believed
        // itself registered and waited for the next service restart without
        // leaving a trace.
        //
        // Here both stages share a single channel: the draw now only decides
        // the path. Either `gather` consumes it, or it **stays queued** for
        // hot wiring, and the main loop wires it an instant later. The test
        // asserts this outcome, not a path.
        //
        // **Simulated** clock: that is what makes the race reproducible. With
        // the real clock, the two timers never expire on the same tick and the
        // rendezvous always wins; the test would then pass just as well with
        // the defect it is meant to forbid. Under the simulated clock, the
        // deadline wins — that is exactly the path on which the old setup lost
        // the announcement.
        //
        // 200 rounds rather than one: the wake-up order of two timers expired
        // at the same instant is guaranteed by nothing, and the day it changes
        // the test must keep checking the outcome on both paths rather than
        // fall on an order that became wrong.
        tokio::time::pause();

        let mut via_gather = 0usize;
        let mut left_in_queue = 0usize;
        for _ in 0..200 {
            let dir = tempfile::tempdir().unwrap();
            let register = dir.path().join("register.sock");
            let listener = UnixListener::bind(&register).unwrap();
            let (tx, mut rx) = channel();
            // The plugin connects right away — `gather` accepts, and its read
            // task waits — but only writes its line at the **exact** instant
            // of the deadline. The task therefore deposits the announcement on
            // the same clock tick as the rendezvous's expiry, and both arms of
            // the `select!` are ready at the same poll. This is the full path,
            // socket included, and not a direct deposit into the channel.
            let r = register.clone();
            tokio::spawn(async move {
                let mut s = UnixStream::connect(&r).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                s.write_all(b"{\"name\":\"radio\",\"kinds\":[\"source\"]}\n").await.unwrap();
                s.shutdown().await.unwrap();
            });

            let g = gather(
                &listener,
                &["radio".to_string()],
                no_deaths(),
                Duration::from_millis(100),
                &tx,
                &mut rx,
            )
            .await;

            if g.announcements.contains_key("radio") {
                via_gather += 1;
            } else {
                // The deadline won the draw. The announcement is not lost for
                // that: it is in the queue — or arrives there the next instant,
                // the read task's sender being still alive — and the main loop
                // is the one that will hot-wire it. This is precisely what the
                // old setup made impossible: its receiver died with `gather`.
                let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .expect("the announcement must remain wirable after the rendezvous")
                    .expect("the channel of both stages does not close");
                assert_eq!(received.name, "radio");
                left_in_queue += 1;
            }
        }
        // The path that lost the announcement must have been taken, otherwise
        // this test proves nothing: it is the trial. If it stops happening — a
        // wake-up order that changes — better a loud failure than a test that
        // passes without checking anything anymore.
        assert!(
            left_in_queue > 0,
            "the deadline never won the draw ({via_gather} via gather): the path that lost the announcement is no longer reproduced"
        );
    }

    #[tokio::test]
    async fn a_silent_connection_is_dropped_after_the_timeout() {
        // Without a read timeout, every silent connection pinned a task and a
        // descriptor for the life of the process. A plugin with a reconnection
        // bug, hitting the socket once per second without writing, ended up
        // exhausting the descriptors: no wirable announcement left on a device
        // that is never rebooted.
        let (a, mut b) = tokio::net::UnixStream::pair().unwrap();
        let (tx, mut rx) = channel();
        tokio::spawn(read_announcement(a, tx, Duration::from_millis(100)));

        // The connection dropped by the task is visible from the other end: a
        // zero-byte read, that is an end of file.
        let mut buffer = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), b.read(&mut buffer))
            .await
            .expect("a silent connection must be dropped, not held for the life of the process")
            .unwrap();
        assert_eq!(read, 0, "end of file: the core gave its descriptor back");
        assert!(rx.try_recv().is_err(), "nothing to wire from a silent connection");
    }

    #[tokio::test]
    async fn a_silent_connection_does_not_block_late_announcements() {
        // Same head-of-line blocking as on the rendezvous, same fix: without
        // the read task per connection, the silent connection below would hold
        // back every following announcement forever — and this loop no longer
        // has a deadline to unblock it.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Announcement>(4);
        tokio::spawn(accept_forever(listener, tx));

        let silent = UnixStream::connect(&register).await.unwrap();
        announcement(&register, r#"{"name":"radio","kinds":["source"]}"#).await;

        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("a silent connection must not hold back the announcements")
            .unwrap();
        assert_eq!(received.name, "radio");
        drop(silent);
    }

    #[tokio::test]
    async fn an_unreadable_late_announcement_does_not_close_the_socket() {
        // A faulty binary must not deprive the others of hot wiring.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Announcement>(4);
        tokio::spawn(accept_forever(listener, tx));

        announcement(&register, "this is not json").await;
        announcement(&register, r#"{"name":"radio","kinds":["source"]}"#).await;

        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("the socket must keep accepting after an unreadable line")
            .unwrap();
        assert_eq!(received.name, "radio", "only the readable announcement must come through");
    }

    #[tokio::test]
    async fn metadata_order_follows_the_manifest_not_the_arrivals() {
        // The guarantee was acquired by construction (list built before any
        // launch); it is now maintained by code, hence tested.
        let mut announcements = HashMap::new();
        for name in ["musicbrainz", "ouifm-metas", "radiofrance-metas"] {
            announcements.insert(
                name.to_string(),
                Announcement {
                    name: name.to_string(),
                    kinds: vec![PluginKind::Metadata],
                    admin: false,
                    covers: false,
                    ui_version: None,
                },
            );
        }
        announcements.insert(
            "radio".to_string(),
            Announcement {
                name: "radio".into(),
                kinds: vec![PluginKind::Source],
                admin: true,
                covers: false,
                ui_version: None,
            },
        );
        let g = Gathered { announcements, ..Default::default() };

        // Manifest order, deliberately different from alphabetical order and
        // from any plausible arrival order.
        let manifest = vec![
            "radio".to_string(),
            "ouifm-metas".to_string(),
            "radiofrance-metas".to_string(),
            "musicbrainz".to_string(),
        ];
        assert_eq!(
            metadata_order(&manifest, &g),
            vec![
                "ouifm-metas".to_string(),
                "radiofrance-metas".to_string(),
                "musicbrainz".to_string()
            ]
        );
    }

    #[test]
    fn no_launched_plugin_leaves_nobody_alive() {
        // Empty `plugins.toml`, or every executable not found: nobody will
        // ever announce. That is a configuration error, and the core still
        // refuses to start in this sole case.
        assert!(!a_live_plugin(&[], &Gathered::default()));
    }

    #[test]
    fn all_plugins_dead_leave_nobody_alive() {
        // Launched, then dead before the deadline: nothing runs anymore, so
        // nothing can hot-announce itself. Same refusal.
        let launched = vec!["radio".to_string(), "console".to_string()];
        let g = Gathered { dead: launched.clone(), ..Default::default() };
        assert!(!a_live_plugin(&launched, &g));
    }

    #[test]
    fn a_stalled_plugin_remains_a_living_process() {
        // The case justifying the whole work: `files` runs, it said nothing at
        // the deadline, it can still speak. The core must start so that the
        // status page shows it stalled — a refusal would remove it precisely
        // when one wants to consult it.
        let launched = vec!["radio".to_string(), "files".to_string()];
        let g = Gathered {
            dead: vec!["radio".to_string()],
            stalled: vec!["files".to_string()],
            ..Default::default()
        };
        assert!(a_live_plugin(&launched, &g));
    }

    #[test]
    fn switching_everything_off_is_not_a_failure() {
        let g = Gathered::default();
        // No active plugin declared: nothing was launched, and that is
        // intended. The core must start — without its UI, nobody could switch
        // anything back on.
        assert!(!startup_refused(0, &[], &g));
        // Active plugins declared, but no living process left: that is the
        // configuration error the refusal exists to report.
        assert!(startup_refused(2, &[], &g));
    }

    #[test]
    fn a_single_living_one_is_enough_to_start() {
        let mut g = Gathered::default();
        g.dead.push("cd".into());
        assert!(!startup_refused(2, &["radio".into(), "cd".into()], &g));
    }
}
