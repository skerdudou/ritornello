use crate::bindings::Bindings;
use crate::learn::LearnState;
use evdev::{Device, EventType};
use ritornello_proto::{Command, InputMessage};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

/// Root of evdev nodes on a standard Linux.
pub const INPUT_DIR: &str = "/dev/input";

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

/// Disk listing of evdev nodes. Missing or unreadable directory → empty list
/// and a `warn`: never fatal.
pub fn scan_event_nodes(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else {
        tracing::warn!("directory {} unreadable: no input device", root.display());
        return Vec::new();
    };
    let entries: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    event_nodes(root, &entries)
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

/// Same resolution as `key_outcome`, plus the autorepeat rule: a held key
/// (evdev `value == 2`) only emits for the volume commands, marked `held` so
/// the core paces them (the kernel repeats much faster than one step per
/// 500 ms should go). Pure function, testable without hardware.
pub fn key_outcome_held(
    bindings: &Bindings,
    learning_device: Option<&str>,
    device_name: &str,
    code: u16,
    held: bool,
) -> Option<InputMessage> {
    let cmd = key_outcome(bindings, learning_device, device_name, code)?;
    if held && !matches!(cmd, Command::VolumeUp | Command::VolumeDown) {
        return None;
    }
    Some(InputMessage { cmd, held })
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
    pub tx: mpsc::Sender<InputMessage>,
}

impl Hub {
    pub fn new(bindings: Bindings, tx: mpsc::Sender<InputMessage>) -> Hub {
        Hub {
            bindings: Arc::new(RwLock::new(bindings)),
            learn: Arc::new(RwLock::new(LearnState::default())),
            open: Arc::new(RwLock::new(BTreeMap::new())),
            tx,
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
    /// is logged as `warn` and skipped — never fatal.
    pub fn open_new_devices(&self, root: &Path) -> usize {
        let mut new_count = 0;
        for path in scan_event_nodes(root) {
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
                    tracing::warn!("device {} unreadable, skipped: {e}", path.display());
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

    /// One playback task per node, all feeding the same mpsc.
    fn spawn_reader(&self, path: PathBuf, dev: Device, name: String) {
        let hub = self.clone();
        tokio::spawn(async move {
            let mut stream = match dev.into_event_stream() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("evdev stream {} unavailable: {e}", path.display());
                    hub.forget(&path);
                    return;
                }
            };
            tracing::info!("listening on device: {name} ({})", path.display());
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
                let value = ev.value();
                // 1 = key down, 2 = kernel autorepeat while held. Release (0)
                // stays ignored: the core paces repeats, no timer to stop here.
                if ev.event_type() != EventType::KEY || (value != 1 && value != 2) {
                    continue;
                }
                if value == 1 {
                    // Learning consumes the first press and emits nothing.
                    let capture = { hub.learn.write().unwrap().capture(&name, ev.code()) };
                    if capture {
                        continue;
                    }
                }
                // No lock guard crosses the send `.await`.
                let msg = {
                    let learn = hub.learn.read().unwrap();
                    let b = hub.bindings.read().unwrap();
                    key_outcome_held(&b, learn.device(), &name, ev.code(), value == 2)
                };
                if let Some(msg) = msg {
                    tracing::debug!("{name}: key {} -> {:?}", ev.code(), msg.cmd);
                    let _ = hub.tx.send(msg).await;
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
        (Hub::new(table(), tx), rx)
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
    fn scan_event_nodes_missing_directory_gives_empty() {
        assert!(scan_event_nodes(Path::new("/nonexistent-input-xyz")).is_empty());
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

    #[test]
    fn key_outcome_held_marks_volume_repeats() {
        let t = table();
        let pressed = key_outcome_held(&t, None, "eHome", 115, false).unwrap();
        assert_eq!(pressed, InputMessage::from(Command::VolumeUp));
        let repeated = key_outcome_held(&t, None, "eHome", 115, true).unwrap();
        assert_eq!(repeated.cmd, Command::VolumeUp);
        assert!(repeated.held);
    }

    #[test]
    fn key_outcome_held_ignores_repeats_outside_volume() {
        // Holding Stop or Next must not machine-gun the command: autorepeat
        // only means something for the volume.
        let mut t = table();
        t.devices[0].bindings.push(Binding::new(166, &Command::Stop));
        assert_eq!(key_outcome_held(&t, None, "eHome", 166, true), None);
        // The fresh press still goes through.
        assert!(key_outcome_held(&t, None, "eHome", 166, false).is_some());
    }

    #[test]
    fn key_outcome_held_respects_learning() {
        let t = table();
        assert_eq!(key_outcome_held(&t, Some("eHome"), "eHome", 115, true), None);
    }
}
