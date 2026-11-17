use crate::bindings::Bindings;
use crate::learn::LearnState;
use crate::lock::{LONG_PRESS, LockKeyPress, Refusals};
#[cfg(test)]
use crate::lock::REFUSAL_INTERVAL;
use evdev::{Device, EventType};
use ritornello_proto::{Command, InputMessage};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Root of evdev nodes on a standard Linux.
pub const INPUT_DIR: &str = "/dev/input";

/// How often the plugin does on its own what the page's "Refresh" does.
/// Without it, a receiver unplugged and plugged back stayed dead until the
/// plugin restarted or someone clicked "Refresh" (seen on the device,
/// 2026-10-06): its reader ends with the old node, and nothing ever opened
/// the new one. A scan that finds nothing new costs one listing of the input
/// directory, so five seconds is cheap and still feels immediate.
pub const RESCAN_PERIOD: Duration = Duration::from_secs(5);

/// Pure filter over a directory listing: keeps only `eventN` nodes, sorted.
/// Separated from disk access to stay testable without hardware (like the
/// core's `audio_output::parse_device_list`).
pub fn event_nodes(root: &Path, entries: &[String]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = entries
        .iter()
        .filter(|n| {
            n.strip_prefix("event")
                .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|n| root.join(n))
        .collect();
    v.sort();
    v
}

/// Disk listing of evdev nodes. A missing or unreadable directory is an
/// error for the caller to report — once, see `Reported` — never fatal.
pub fn scan_event_nodes(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries: Vec<String> = std::fs::read_dir(root)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    Ok(event_nodes(root, &entries))
}

/// The failures already reported as a `warn`, so that a rescan every
/// `RESCAN_PERIOD` does not repeat the same line every five seconds for a
/// node that simply stays unreadable (the plugin's output ends up in a log
/// the owner reads). Repeats go to `debug`.
///
/// What makes a failure worth reporting again:
/// * the node **reached listening** (`node_listening`): it worked, so a
///   later failure is news;
/// * the node **left the input directory** (`keep_only`): a node that comes
///   back is a new device behind a possibly reused `eventN` name — the
///   receiver plugged back in — and deserves its own line;
/// * for the directory itself, one listing that **succeeded**.
///
/// Opening a node is not enough to forget its failure: a node that opens
/// but then refuses its event stream would otherwise be reopened, fail and
/// warn again at every tick. Only listening counts as having recovered.
#[derive(Debug, Default)]
pub struct Reported {
    nodes: BTreeSet<PathBuf>,
    root: bool,
}

impl Reported {
    /// Records a failure of `path`; `true` when it is the first since the
    /// last reset, i.e. when it must be reported as a `warn`.
    pub fn node_failed(&mut self, path: &Path) -> bool {
        self.nodes.insert(path.to_path_buf())
    }

    /// `path` is being listened to: a later failure is new again.
    pub fn node_listening(&mut self, path: &Path) {
        self.nodes.remove(path);
    }

    /// Forgets the failures of every node no longer in the directory.
    pub fn keep_only(&mut self, present: &[PathBuf]) {
        self.nodes.retain(|p| present.contains(p));
    }

    /// Same as `node_failed`, for the input directory itself.
    pub fn root_failed(&mut self) -> bool {
        !std::mem::replace(&mut self.root, true)
    }

    /// The input directory could be listed: a later failure is new again.
    pub fn root_readable(&mut self) {
        self.root = false;
    }

    #[cfg(test)]
    fn has_node(&self, path: &Path) -> bool {
        self.nodes.contains(path)
    }
}

/// What a key press produces: the bound command, or nothing. The device
/// currently being learned emits nothing (otherwise learning "Volume +"
/// would trigger a volume +); the others keep working normally. Pure
/// function, testable without hardware.
pub fn key_outcome(
    bindings: &Bindings,
    learning_device: Option<&str>,
    device_name: &str,
    code: u16,
) -> Option<Command> {
    if learning_device == Some(device_name) {
        return None;
    }
    bindings.resolve(device_name, code)
}

/// The three things the kernel reports about a key: evdev `value` 1, 2, 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEdge {
    Press,
    /// Kernel autorepeat while the key stays down.
    Repeat,
    Release,
}

impl KeyEdge {
    pub fn from_value(value: i32) -> Option<KeyEdge> {
        match value {
            1 => Some(KeyEdge::Press),
            2 => Some(KeyEdge::Repeat),
            0 => Some(KeyEdge::Release),
            _ => None,
        }
    }
}

/// What one key event asks of the plugin.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Forward this message to the core.
    Send(InputMessage),
    /// The lock key went down: arm the long press, send nothing yet.
    LockPress,
    /// The lock key went up: disarm it.
    LockRelease,
    /// Another key went down while locked: refused, which the core is told
    /// (`InputLockRefused`, throttled by `key_loop`) so the press gets an
    /// answer on the displays.
    Refused,
    Nothing,
}

/// Same resolution as `key_outcome`, plus the two rules that depend on the
/// event rather than on the key:
///
/// * the autorepeat rule: a held key only emits for the volume commands,
///   marked `held` so the core paces them (the kernel repeats much faster than
///   one step per 500 ms should go);
/// * the remote lock: the key bound to `ToggleInputLock` never sends on a
///   press (it is a *long* press, timed by `key_loop`) and works whether the
///   remote is locked or not, since it is the way out; every other key, Power
///   included, is ignored while `locked`. The core applies every command it
///   receives, so this is the only place that decides to stay silent. A
///   press of such a key is `Refused` rather than `Nothing`, so that
///   `key_loop` can tell the core (LOCKED on the displays); its autorepeats
///   and its release are `Nothing`.
///
/// Pure function, testable without hardware.
pub fn key_outcome_event(
    bindings: &Bindings,
    learning_device: Option<&str>,
    device_name: &str,
    code: u16,
    edge: KeyEdge,
    locked: bool,
) -> Outcome {
    let Some(cmd) = key_outcome(bindings, learning_device, device_name, code) else {
        return Outcome::Nothing;
    };
    if cmd == Command::ToggleInputLock {
        return match edge {
            KeyEdge::Press => Outcome::LockPress,
            KeyEdge::Release => Outcome::LockRelease,
            KeyEdge::Repeat => Outcome::Nothing,
        };
    }
    if locked {
        return match edge {
            KeyEdge::Press => Outcome::Refused,
            KeyEdge::Repeat | KeyEdge::Release => Outcome::Nothing,
        };
    }
    match edge {
        KeyEdge::Press => Outcome::Send(InputMessage { cmd, held: false }),
        KeyEdge::Repeat if matches!(cmd, Command::VolumeUp | Command::VolumeDown) => {
            Outcome::Send(InputMessage { cmd, held: true })
        }
        KeyEdge::Repeat | KeyEdge::Release => Outcome::Nothing,
    }
}

/// One device's key events, in order: `(evdev code, value 0 | 1 | 2)`. Runs
/// until the sender is dropped (the node went away).
///
/// The loop, not the reader, owns the long press: it waits for the next event
/// **or** the lock key's deadline, so a remote that sends no autorepeat still
/// toggles after `LONG_PRESS`, and one that does repeat toggles once.
pub(crate) async fn key_loop(mut events: mpsc::Receiver<(u16, i32)>, name: String, hub: Hub) {
    let mut press = LockKeyPress::default();
    // The code whose hold is armed. A release of that code disarms it
    // whatever the bindings say by then: learning may have started on this
    // device, or the table changed, during the hold, and the release then no
    // longer resolves to `LockRelease` — yet the key is up, and the deadline
    // must not toggle for a key nobody holds.
    let mut armed: Option<u16> = None;
    loop {
        let deadline = press.deadline();
        let event = tokio::select! {
            ev = events.recv() => match ev {
                Some(ev) => Some(ev),
                None => break,
            },
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)),
                if deadline.is_some() => None,
        };
        let toggle = match event {
            // The deadline passed with no event.
            None => press.due(Instant::now()),
            Some((code, value)) => {
                let Some(edge) = KeyEdge::from_value(value) else {
                    continue;
                };
                if edge == KeyEdge::Press {
                    // Learning consumes the first press and emits nothing.
                    let capture = { hub.learn.write().unwrap().capture(&name, code) };
                    if capture {
                        continue;
                    }
                }
                if edge == KeyEdge::Release && armed == Some(code) {
                    press.release();
                    armed = None;
                }
                // No lock guard crosses an `.await`.
                let (outcome, is_lock_key) = {
                    let learn = hub.learn.read().unwrap();
                    let b = hub.bindings.read().unwrap();
                    let locked = *hub.locked.borrow();
                    (
                        key_outcome_event(&b, learn.device(), &name, code, edge, locked),
                        key_outcome(&b, learn.device(), &name, code)
                            == Some(Command::ToggleInputLock),
                    )
                };
                match outcome {
                    Outcome::Send(msg) => {
                        tracing::debug!("{name}: key {code} -> {:?}", msg.cmd);
                        let _ = hub.tx.send(msg).await;
                        false
                    }
                    Outcome::LockPress => {
                        press.press(Instant::now());
                        armed = Some(code);
                        false
                    }
                    Outcome::LockRelease => {
                        press.release();
                        armed = None;
                        false
                    }
                    // An autorepeat of the lock key is a chance to notice the
                    // deadline, not a command.
                    Outcome::Nothing if is_lock_key && edge == KeyEdge::Repeat => {
                        press.due(Instant::now())
                    }
                    Outcome::Refused => {
                        let say = hub.refusals.lock().unwrap().say(Instant::now());
                        if say {
                            tracing::debug!("{name}: key {code} refused, remote locked");
                            let _ = hub.tx.send(InputMessage::from(Command::InputLockRefused)).await;
                        }
                        false
                    }
                    Outcome::Nothing => false,
                }
            }
        };
        if toggle {
            tracing::debug!("{name}: lock key held {LONG_PRESS:?} -> ToggleInputLock");
            let _ = hub.tx.send(InputMessage::from(Command::ToggleInputLock)).await;
        }
    }
}

/// State shared between the Input half (the playback tasks) and the Admin
/// half. `std::sync::RwLock`: guards are always released before any
/// `.await`, and `page()` (synchronous) can read without a runtime.
#[derive(Clone)]
pub struct Hub {
    pub bindings: Arc<RwLock<Bindings>>,
    pub learn: Arc<RwLock<LearnState>>,
    /// Currently open nodes: path → device name.
    pub open: Arc<RwLock<BTreeMap<PathBuf, String>>>,
    /// Failures already reported, see `Reported`. `std::sync::Mutex`, never
    /// held across an `.await`.
    pub reported: Arc<Mutex<Reported>>,
    pub tx: mpsc::Sender<InputMessage>,
    /// The remote lock, as the core last told the SDK. Read per event, never
    /// across an `.await`.
    pub locked: watch::Receiver<bool>,
    /// When a refused key was last answered, for every device at once.
    /// `std::sync::Mutex`, never held across an `.await`.
    pub refusals: Arc<Mutex<Refusals>>,
}

impl Hub {
    pub fn new(
        bindings: Bindings,
        tx: mpsc::Sender<InputMessage>,
        locked: watch::Receiver<bool>,
    ) -> Hub {
        Hub {
            bindings: Arc::new(RwLock::new(bindings)),
            learn: Arc::new(RwLock::new(LearnState::default())),
            open: Arc::new(RwLock::new(BTreeMap::new())),
            reported: Arc::new(Mutex::new(Reported::default())),
            tx,
            locked,
            refusals: Arc::new(Mutex::new(Refusals::default())),
        }
    }

    /// Names of the currently open devices, sorted and deduplicated (several
    /// nodes can share the same name). Empty entries are dropped: the empty
    /// name is a reservation placeholder set in `open` while `Device::open`
    /// is in progress (see `open_new_devices`), and the admin page probes
    /// `device_names()` every 300 ms during learning — without this filter
    /// it would transiently display a ghost entry.
    pub fn device_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .open
            .read()
            .unwrap()
            .values()
            .filter(|n| !n.is_empty())
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Opens every readable evdev node not already open and spawns one
    /// playback task per node. Returns the number of new nodes. An
    /// unreadable device (permissions, gone between enumeration and open)
    /// is skipped — never fatal — and reported as a `warn` only the first
    /// time (see `Reported`), since this runs every `RESCAN_PERIOD`.
    ///
    /// Synchronous file I/O: from async code, go through `rescan`.
    pub fn open_new_devices(&self, root: &Path) -> usize {
        let nodes = match scan_event_nodes(root) {
            Ok(nodes) => {
                self.reported.lock().unwrap().root_readable();
                nodes
            }
            Err(e) => {
                if self.reported.lock().unwrap().root_failed() {
                    tracing::warn!("directory {} unreadable: no input device: {e}", root.display());
                } else {
                    tracing::debug!("directory {} still unreadable: {e}", root.display());
                }
                Vec::new()
            }
        };
        self.reported.lock().unwrap().keep_only(&nodes);
        let mut new_count = 0;
        for path in nodes {
            // Atomic reservation: the membership check and the insert happen
            // under the same write lock, so a concurrent second rescan
            // (double-click on "Refresh") cannot open the same node twice
            // and spawn two readers on it.
            {
                let mut open = self.open.write().unwrap();
                if open.contains_key(&path) {
                    continue;
                }
                open.insert(path.clone(), String::new());
            }
            let dev = match Device::open(&path) {
                Ok(d) => d,
                Err(e) => {
                    self.report_unreadable(&path, &e);
                    self.open.write().unwrap().remove(&path);
                    continue;
                }
            };
            let name = dev.name().unwrap_or("?").to_string();
            self.open.write().unwrap().insert(path.clone(), name.clone());
            self.spawn_reader(path, dev, name);
            new_count += 1;
        }
        new_count
    }

    /// `open_new_devices` off the async runtime: a directory listing and an
    /// open plus a few ioctls per new node are quick on `/dev/input`, but
    /// they are blocking I/O all the same, and this now runs every
    /// `RESCAN_PERIOD` beside the remote's own tasks.
    pub async fn rescan(&self, root: &Path) -> usize {
        let hub = self.clone();
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || hub.open_new_devices(&root))
            .await
            .unwrap_or_else(|e| {
                tracing::error!("input rescan failed: {e}");
                0
            })
    }

    /// A node that could not be used: `warn` the first time, `debug` while
    /// it stays that way (see `Reported`).
    fn report_unreadable(&self, path: &Path, e: &dyn std::fmt::Display) {
        if self.reported.lock().unwrap().node_failed(path) {
            tracing::warn!("device {} unreadable, skipped: {e}", path.display());
        } else {
            tracing::debug!("device {} still unreadable: {e}", path.display());
        }
    }

    /// One playback task per node, all feeding the same mpsc.
    fn spawn_reader(&self, path: PathBuf, dev: Device, name: String) {
        let hub = self.clone();
        tokio::spawn(async move {
            let mut stream = match dev.into_event_stream() {
                Ok(s) => s,
                Err(e) => {
                    // Same memory as an open that fails: the node is
                    // forgotten, so the next rescan opens it again, and
                    // without it this would warn every five seconds.
                    let e = format!("evdev stream unavailable: {e}");
                    hub.report_unreadable(&path, &e);
                    hub.forget(&path);
                    return;
                }
            };
            hub.reported.lock().unwrap().node_listening(&path);
            tracing::info!("listening on device: {name} ({})", path.display());
            // One loop per device keeps the lock key's long press apart from
            // the others' timing; it ends when this sender is dropped.
            let (events, rx) = mpsc::channel(64);
            tokio::spawn(key_loop(rx, name.clone(), hub.clone()));
            loop {
                let ev = match stream.next_event().await {
                    Ok(ev) => ev,
                    Err(e) => {
                        // Unplugged: this task ends, the others keep
                        // running.
                        tracing::info!("read from {} ended: {e}", path.display());
                        break;
                    }
                };
                // 1 = down, 2 = kernel autorepeat, 0 = up: `key_loop` decides.
                if ev.event_type() != EventType::KEY || KeyEdge::from_value(ev.value()).is_none() {
                    continue;
                }
                if events.send((ev.code(), ev.value())).await.is_err() {
                    break;
                }
            }
            hub.forget(&path);
        });
    }

    /// Forgets a node whose playback has ended. If no node still carries
    /// this name, any learning session in progress on it is abandoned (the
    /// device has disappeared).
    fn forget(&self, path: &Path) {
        let name = self.open.write().unwrap().remove(path);
        if let Some(name) = name
            && !self.device_names().contains(&name)
        {
            self.learn.write().unwrap().cancel_if(&name);
        }
    }
}

/// Runs `rescan` every `period`, for as long as the plugin lives. The first
/// tick is one period away: `main` has just scanned. A tick that finds
/// nothing new logs nothing above `debug`.
pub fn spawn_periodic_rescan(hub: Hub, root: PathBuf, period: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        // A missed tick (a slow scan, a suspended clock) is not caught up in
        // a burst: one scan sees everything that changed meanwhile.
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            let n = hub.rescan(&root).await;
            if n > 0 {
                tracing::info!("periodic rescan: {n} new device(s) opened");
            } else {
                tracing::debug!("periodic rescan: nothing new");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, Device as BindDevice};

    fn table() -> Bindings {
        Bindings {
            devices: vec![BindDevice {
                name: "eHome".into(),
                bindings: vec![Binding::new(115, &Command::VolumeUp)],
            }],
        }
    }

    fn test_hub() -> (Hub, mpsc::Receiver<InputMessage>) {
        let (tx, rx) = mpsc::channel(8);
        (Hub::new(table(), tx, watch::channel(false).1), rx)
    }

    #[test]
    fn event_nodes_keeps_only_event_nodes() {
        let entries = vec![
            "event10".to_string(),
            "event2".to_string(),
            "mice".to_string(),
            "by-id".to_string(),
            "eventX".to_string(),
            "event".to_string(),
        ];
        assert_eq!(
            event_nodes(Path::new("/dev/input"), &entries),
            vec![PathBuf::from("/dev/input/event10"), PathBuf::from("/dev/input/event2")]
        );
    }

    #[test]
    fn scan_event_nodes_missing_directory_is_an_error() {
        assert!(scan_event_nodes(Path::new("/nonexistent-input-xyz")).is_err());
    }

    #[test]
    fn key_outcome_resolves_the_right_devices_binding() {
        let t = table();
        assert_eq!(key_outcome(&t, None, "eHome", 115), Some(Command::VolumeUp));
        assert_eq!(key_outcome(&t, None, "eHome", 42), None);
        assert_eq!(key_outcome(&t, None, "Autre", 115), None);
    }

    #[test]
    fn key_outcome_suppresses_emission_only_from_the_device_being_learned() {
        let mut t = table();
        t.devices.push(BindDevice {
            name: "USB Keyboard".into(),
            bindings: vec![Binding::new(115, &Command::VolumeUp)],
        });
        // learning on eHome: eHome silent, the keyboard keeps working
        assert_eq!(key_outcome(&t, Some("eHome"), "eHome", 115), None);
        assert_eq!(
            key_outcome(&t, Some("eHome"), "USB Keyboard", 115),
            Some(Command::VolumeUp)
        );
    }

    #[test]
    fn device_names_deduplicates_and_sorts() {
        let (hub, _rx) = test_hub();
        {
            let mut open = hub.open.write().unwrap();
            open.insert(PathBuf::from("/dev/input/event3"), "eHome".into());
            open.insert(PathBuf::from("/dev/input/event1"), "USB Keyboard".into());
            open.insert(PathBuf::from("/dev/input/event2"), "eHome".into());
        }
        assert_eq!(hub.device_names(), vec!["USB Keyboard", "eHome"]);
    }

    #[tokio::test]
    async fn open_new_devices_on_a_directory_with_no_node_opens_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mice"), "").unwrap();
        let (hub, _rx) = test_hub();
        assert_eq!(hub.open_new_devices(dir.path()), 0);
        assert!(hub.device_names().is_empty());
    }

    #[test]
    fn forget_removes_the_node_from_the_map() {
        let (hub, _rx) = test_hub();
        let p = PathBuf::from("/dev/input/event7");
        hub.open.write().unwrap().insert(p.clone(), "eHome".into());
        hub.forget(&p);
        assert!(hub.device_names().is_empty());
    }

    #[test]
    fn the_hub_suppresses_emission_from_the_device_being_learned() {
        let (hub, _rx) = test_hub();
        hub.bindings.write().unwrap().devices.push(BindDevice {
            name: "USB Keyboard".into(),
            bindings: vec![Binding::new(115, &Command::VolumeUp)],
        });
        hub.learn.write().unwrap().learn("eHome");

        let outcome = |name: &str, code: u16| {
            let learn = hub.learn.read().unwrap();
            let b = hub.bindings.read().unwrap();
            key_outcome(&b, learn.device(), name, code)
        };
        assert_eq!(outcome("eHome", 115), None);
        assert_eq!(outcome("USB Keyboard", 115), Some(Command::VolumeUp));

        // once the code is captured, eHome emits again
        hub.learn.write().unwrap().capture("eHome", 115);
        assert_eq!(outcome("eHome", 115), Some(Command::VolumeUp));
    }

    #[test]
    fn forget_abandons_learning_when_the_last_node_disappears() {
        let (hub, _rx) = test_hub();
        let p1 = PathBuf::from("/dev/input/event1");
        let p2 = PathBuf::from("/dev/input/event2");
        {
            let mut open = hub.open.write().unwrap();
            open.insert(p1.clone(), "eHome".into());
            open.insert(p2.clone(), "eHome".into());
        }
        hub.learn.write().unwrap().learn("eHome");
        // only one of the two nodes disappears: learning continues
        hub.forget(&p1);
        assert_eq!(hub.learn.read().unwrap().device(), Some("eHome"));
        // the last one disappears: learning is abandoned
        hub.forget(&p2);
        assert_eq!(hub.learn.read().unwrap().snapshot(), None);
    }

    fn msg(cmd: Command) -> Outcome {
        Outcome::Send(InputMessage::from(cmd))
    }

    #[test]
    fn key_outcome_event_marks_volume_repeats() {
        let t = table();
        let pressed = key_outcome_event(&t, None, "eHome", 115, KeyEdge::Press, false);
        assert_eq!(pressed, msg(Command::VolumeUp));
        let repeated = key_outcome_event(&t, None, "eHome", 115, KeyEdge::Repeat, false);
        assert_eq!(repeated, Outcome::Send(InputMessage { cmd: Command::VolumeUp, held: true }));
    }

    #[test]
    fn key_outcome_event_ignores_repeats_outside_volume() {
        // Holding Stop or Next must not machine-gun the command: autorepeat
        // only means something for the volume.
        let mut t = table();
        t.devices[0].bindings.push(Binding::new(166, &Command::Stop));
        assert_eq!(
            key_outcome_event(&t, None, "eHome", 166, KeyEdge::Repeat, false),
            Outcome::Nothing
        );
        // The fresh press still goes through.
        assert_eq!(
            key_outcome_event(&t, None, "eHome", 166, KeyEdge::Press, false),
            msg(Command::Stop)
        );
    }

    #[test]
    fn key_outcome_event_respects_learning() {
        let t = table();
        assert_eq!(
            key_outcome_event(&t, Some("eHome"), "eHome", 115, KeyEdge::Repeat, false),
            Outcome::Nothing
        );
    }

    /// A table whose device has the lock key (code 200) next to Power, a
    /// source key and the volume.
    fn lock_table() -> Bindings {
        Bindings {
            devices: vec![BindDevice {
                name: "eHome".into(),
                bindings: vec![
                    Binding::new(115, &Command::VolumeUp),
                    Binding::new(116, &Command::Power),
                    Binding::new(2, &Command::Select(3)),
                    Binding::new(200, &Command::ToggleInputLock),
                ],
            }],
        }
    }

    #[test]
    fn the_lock_key_never_sends_on_press() {
        let t = lock_table();
        for locked in [false, true] {
            let o = |edge| key_outcome_event(&t, None, "eHome", 200, edge, locked);
            assert_eq!(o(KeyEdge::Press), Outcome::LockPress);
            assert_eq!(o(KeyEdge::Release), Outcome::LockRelease);
            assert_eq!(o(KeyEdge::Repeat), Outcome::Nothing);
        }
    }

    #[test]
    fn every_other_key_is_ignored_while_locked() {
        let t = lock_table();
        for (code, cmd) in [(116, Command::Power), (2, Command::Select(3)), (115, Command::VolumeUp)] {
            // A press is refused (the loop says so, throttled); its
            // autorepeats and its release say nothing at all.
            assert_eq!(
                key_outcome_event(&t, None, "eHome", code, KeyEdge::Press, true),
                Outcome::Refused,
                "{cmd:?}"
            );
            for edge in [KeyEdge::Repeat, KeyEdge::Release] {
                assert_eq!(
                    key_outcome_event(&t, None, "eHome", code, edge, true),
                    Outcome::Nothing,
                    "{cmd:?} {edge:?}"
                );
            }
            assert_eq!(
                key_outcome_event(&t, None, "eHome", code, KeyEdge::Press, false),
                msg(cmd)
            );
        }
    }

    #[test]
    fn autorepeat_of_another_key_is_ignored_while_locked() {
        let t = lock_table();
        assert_eq!(
            key_outcome_event(&t, None, "eHome", 115, KeyEdge::Repeat, false),
            Outcome::Send(InputMessage { cmd: Command::VolumeUp, held: true })
        );
        assert_eq!(
            key_outcome_event(&t, None, "eHome", 115, KeyEdge::Repeat, true),
            Outcome::Nothing
        );
    }

    // --- key_loop, on the paused clock ---

    const LOCK_CODE: u16 = 200;

    /// A hub over `lock_table()` with its lock at `locked`, the sender of the
    /// events a reader would forward, and the receiver of what the hub emits.
    /// The lock sender is returned so the watch stays open.
    fn loop_fixture(
        locked: bool,
    ) -> (mpsc::Sender<(u16, i32)>, mpsc::Receiver<InputMessage>, watch::Sender<bool>) {
        let (tx, rx) = mpsc::channel(8);
        let (lock_tx, lock_rx) = watch::channel(locked);
        let hub = Hub::new(lock_table(), tx, lock_rx);
        let (ev_tx, ev_rx) = mpsc::channel(64);
        tokio::spawn(key_loop(ev_rx, "eHome".into(), hub));
        (ev_tx, rx, lock_tx)
    }

    /// Lets the loop task run what is pending.
    async fn settle() {
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }

    /// Moves the paused clock, with the loop settled on both sides: before,
    /// so an event just sent is stamped at the current time and not at the
    /// advanced one; after, so what the advance made due has run.
    async fn advance(ms: u64) {
        settle().await;
        tokio::time::advance(Duration::from_millis(ms)).await;
        settle().await;
    }

    fn nothing(rx: &mut mpsc::Receiver<InputMessage>) {
        assert!(rx.try_recv().is_err(), "nothing was expected");
    }

    async fn expect_toggle(rx: &mut mpsc::Receiver<InputMessage>) {
        let m = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("a toggle was expected")
            .unwrap();
        assert_eq!(m, InputMessage::from(Command::ToggleInputLock));
        nothing(rx);
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_press_toggles_without_autorepeat() {
        let (ev, mut rx, _lock) = loop_fixture(false);
        ev.send((LOCK_CODE, 1)).await.unwrap();
        advance(1999).await;
        nothing(&mut rx);
        advance(1).await;
        expect_toggle(&mut rx).await;
    }

    #[tokio::test(start_paused = true)]
    async fn releasing_before_the_threshold_does_not_toggle() {
        let (ev, mut rx, _lock) = loop_fixture(false);
        ev.send((LOCK_CODE, 1)).await.unwrap();
        advance(1500).await;
        ev.send((LOCK_CODE, 0)).await.unwrap();
        advance(5000).await;
        nothing(&mut rx);
    }

    /// A release that no longer resolves to the lock key (the table changed
    /// during the hold) must still disarm the hold: the key is up.
    #[tokio::test(start_paused = true)]
    async fn a_release_after_the_bindings_changed_still_disarms_the_hold() {
        let (tx, mut rx) = mpsc::channel(8);
        let (_lock_tx, lock_rx) = watch::channel(false);
        let hub = Hub::new(lock_table(), tx, lock_rx);
        let bindings = hub.bindings.clone();
        let (ev, ev_rx) = mpsc::channel(64);
        tokio::spawn(key_loop(ev_rx, "eHome".into(), hub));
        ev.send((LOCK_CODE, 1)).await.unwrap();
        advance(500).await;
        // The key no longer resolves to the lock.
        *bindings.write().unwrap() = Bindings { devices: vec![] };
        ev.send((LOCK_CODE, 0)).await.unwrap();
        advance(5000).await;
        nothing(&mut rx);
    }

    #[tokio::test(start_paused = true)]
    async fn holding_with_autorepeats_toggles_once() {
        let (ev, mut rx, _lock) = loop_fixture(false);
        ev.send((LOCK_CODE, 1)).await.unwrap();
        let mut got = 0;
        for _ in 0..60 {
            advance(100).await;
            ev.send((LOCK_CODE, 2)).await.unwrap();
            advance(0).await;
            while let Ok(m) = rx.try_recv() {
                assert_eq!(m, InputMessage::from(Command::ToggleInputLock));
                got += 1;
            }
        }
        assert_eq!(got, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_lock_key_works_while_locked() {
        let (ev, mut rx, _lock) = loop_fixture(true);
        ev.send((116, 1)).await.unwrap(); // Power
        ev.send((116, 0)).await.unwrap();
        advance(10).await;
        expect_refusal(&mut rx).await;
        ev.send((LOCK_CODE, 1)).await.unwrap();
        advance(2000).await;
        expect_toggle(&mut rx).await;
    }

    async fn expect_refusal(rx: &mut mpsc::Receiver<InputMessage>) {
        let m = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("a refusal was expected")
            .unwrap();
        assert_eq!(m, InputMessage::from(Command::InputLockRefused));
        nothing(rx);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_press_says_so_at_most_once_per_interval() {
        let (ev, mut rx, _lock) = loop_fixture(true);
        ev.send((116, 1)).await.unwrap(); // Power, refused
        advance(10).await;
        expect_refusal(&mut rx).await;
        // Its autorepeats and its release say nothing.
        ev.send((116, 2)).await.unwrap();
        ev.send((116, 2)).await.unwrap();
        ev.send((116, 0)).await.unwrap();
        advance(10).await;
        nothing(&mut rx);
        // Another key, pressed within the interval: still nothing.
        ev.send((2, 1)).await.unwrap();
        ev.send((2, 0)).await.unwrap();
        advance(REFUSAL_INTERVAL.as_millis() as u64 - 30).await;
        ev.send((115, 1)).await.unwrap();
        advance(10).await;
        nothing(&mut rx);
        // Once the interval has passed since the last refusal said, the next
        // press says it again.
        ev.send((115, 0)).await.unwrap();
        advance(20).await;
        ev.send((116, 1)).await.unwrap();
        advance(10).await;
        expect_refusal(&mut rx).await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_lock_key_is_never_refused() {
        let (ev, mut rx, _lock) = loop_fixture(true);
        // A short press of the lock key, then a hold that toggles: no refusal.
        ev.send((LOCK_CODE, 1)).await.unwrap();
        advance(500).await;
        ev.send((LOCK_CODE, 0)).await.unwrap();
        advance(10).await;
        nothing(&mut rx);
        ev.send((LOCK_CODE, 1)).await.unwrap();
        advance(2000).await;
        expect_toggle(&mut rx).await;
    }

    #[tokio::test(start_paused = true)]
    async fn unlocked_a_press_sends_its_own_command() {
        let (ev, mut rx, _lock) = loop_fixture(false);
        ev.send((116, 1)).await.unwrap();
        advance(10).await;
        let m = rx.try_recv().expect("Power was expected");
        assert_eq!(m, InputMessage::from(Command::Power));
        nothing(&mut rx);
    }

    #[tokio::test(start_paused = true)]
    async fn the_refusal_interval_is_shared_by_every_device() {
        // Two receivers of the same remote (or two remotes): one refusal per
        // interval in all, not one per device.
        let (tx, mut rx) = mpsc::channel(8);
        let (_lock_tx, lock_rx) = watch::channel(true);
        let mut t = lock_table();
        let mut other = t.devices[0].clone();
        other.name = "Other".into();
        t.devices.push(other);
        let hub = Hub::new(t, tx, lock_rx);
        let (ev1, ev1_rx) = mpsc::channel(64);
        let (ev2, ev2_rx) = mpsc::channel(64);
        tokio::spawn(key_loop(ev1_rx, "eHome".into(), hub.clone()));
        tokio::spawn(key_loop(ev2_rx, "Other".into(), hub));
        ev1.send((116, 1)).await.unwrap();
        advance(10).await;
        expect_refusal(&mut rx).await;
        ev2.send((116, 1)).await.unwrap();
        advance(10).await;
        nothing(&mut rx);
    }

    /// The `warn` lines `f` emits on this thread, as text.
    fn warnings_of(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    /// A regular file named like a node: listed by the scan, refused by
    /// `Device::open` (its ioctls fail on a file) — an unreadable node.
    fn fake_node(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "").unwrap();
        p
    }

    #[test]
    fn an_unreadable_node_is_warned_about_once_not_at_every_scan() {
        // With a rescan every five seconds, warning at each scan would write
        // the same line twelve times a minute for as long as the node stays.
        let dir = tempfile::tempdir().unwrap();
        fake_node(dir.path(), "event0");
        let (hub, _rx) = test_hub();
        let log = warnings_of(|| {
            for _ in 0..3 {
                assert_eq!(hub.open_new_devices(dir.path()), 0);
            }
        });
        assert_eq!(log.matches("event0 unreadable").count(), 1, "{log}");
    }

    #[test]
    fn a_node_that_leaves_and_comes_back_unreadable_is_warned_about_again() {
        // A node that disappears and reappears is a device plugged back in:
        // its failure is news again.
        let dir = tempfile::tempdir().unwrap();
        let node = fake_node(dir.path(), "event0");
        let (hub, _rx) = test_hub();
        let log = warnings_of(|| {
            hub.open_new_devices(dir.path());
            std::fs::remove_file(&node).unwrap();
            hub.open_new_devices(dir.path());
            fake_node(dir.path(), "event0");
            hub.open_new_devices(dir.path());
            hub.open_new_devices(dir.path());
        });
        assert_eq!(log.matches("event0 unreadable").count(), 2, "{log}");
    }

    #[test]
    fn an_unreadable_input_directory_is_warned_about_once_per_outage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("input");
        let (hub, _rx) = test_hub();
        let log = warnings_of(|| {
            hub.open_new_devices(&root);
            hub.open_new_devices(&root);
            // it comes back, then goes away again: a second outage
            std::fs::create_dir(&root).unwrap();
            hub.open_new_devices(&root);
            std::fs::remove_dir(&root).unwrap();
            hub.open_new_devices(&root);
            hub.open_new_devices(&root);
        });
        assert_eq!(log.matches("input unreadable").count(), 2, "{log}");
    }

    #[test]
    fn a_failure_is_news_again_only_once_the_node_has_been_listened_to() {
        let p = PathBuf::from("/dev/input/event4");
        let mut r = Reported::default();
        assert!(r.node_failed(&p));
        assert!(!r.node_failed(&p));
        // Opening is not recovering: only `node_listening` resets, so a node
        // whose stream keeps failing after a successful open is not
        // reported anew at every tick.
        r.node_listening(&p);
        assert!(r.node_failed(&p));
        // A node still present keeps its memory; one gone loses it.
        r.keep_only(std::slice::from_ref(&p));
        assert!(!r.node_failed(&p));
        r.keep_only(&[]);
        assert!(r.node_failed(&p));
    }

    #[test]
    fn the_input_directory_failure_is_news_again_after_one_good_listing() {
        let mut r = Reported::default();
        assert!(r.root_failed());
        assert!(!r.root_failed());
        r.root_readable();
        assert!(r.root_failed());
    }

    /// Waits, on the paused clock, until a scan has met `node`.
    async fn until_scanned(hub: &Hub, node: &Path) {
        tokio::time::timeout(Duration::from_secs(60), async {
            while !hub.reported.lock().unwrap().has_node(node) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no scan met {} within a minute", node.display()));
    }

    #[tokio::test(start_paused = true)]
    async fn the_periodic_rescan_meets_nodes_that_appear_after_startup() {
        // The regression (2026-10-06): a receiver plugged back in was never
        // opened again, because only startup and "Refresh" scanned. Here no
        // one calls `rescan`: only the timer can meet the nodes. Simulated
        // clock, so no real duration is assumed.
        let dir = tempfile::tempdir().unwrap();
        let (hub, _rx) = test_hub();
        let start = tokio::time::Instant::now();
        let task = spawn_periodic_rescan(hub.clone(), dir.path().to_path_buf(), RESCAN_PERIOD);

        let first = fake_node(dir.path(), "event0");
        until_scanned(&hub, &first).await;
        let met = start.elapsed();
        assert!(met >= RESCAN_PERIOD, "scanned before the first period: {met:?}");
        assert!(met < 2 * RESCAN_PERIOD, "the first tick came late: {met:?}");

        // And again later: a timer, not a single deferred scan.
        let second = fake_node(dir.path(), "event1");
        until_scanned(&hub, &second).await;
        assert!(start.elapsed() >= 2 * RESCAN_PERIOD);
        task.abort();
    }
}
