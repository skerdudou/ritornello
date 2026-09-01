use serde::Serialize;

/// Learning state as the UI reads it in `GetData`: `captured` stays `null`
/// as long as no key has been pressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Learning {
    pub device: String,
    pub captured: Option<u16>,
}

/// State machine of learning. Pure: no I/O, entirely testable without
/// hardware. Learning is **exclusive** — a new request replaces the
/// previous one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LearnState {
    current: Option<Learning>,
}

impl LearnState {
    /// Enters (or re-enters) learning for this device.
    pub fn learn(&mut self, device: &str) {
        self.current = Some(Learning { device: device.to_string(), captured: None });
    }

    /// Exits learning without keeping anything.
    pub fn cancel(&mut self) {
        self.current = None;
    }

    /// Abandons learning if it targeted this device (used when the device
    /// disappears).
    pub fn cancel_if(&mut self, device: &str) {
        if self.current.as_ref().is_some_and(|l| l.device == device) {
            self.current = None;
        }
    }

    /// Device whose events must be **suppressed**, i.e. the one being
    /// learned as long as no code has been captured. Once the code is
    /// captured, learning is over: the device emits again.
    pub fn device(&self) -> Option<&str> {
        match &self.current {
            Some(l) if l.captured.is_none() => Some(l.device.as_str()),
            _ => None,
        }
    }

    /// Retains the first code pressed on the targeted device. Returns
    /// `true` if the event was consumed by learning.
    pub fn capture(&mut self, device: &str, code: u16) -> bool {
        match &mut self.current {
            Some(l) if l.device == device && l.captured.is_none() => {
                l.captured = Some(code);
                tracing::info!("key learning: {device} -> code {code}");
                true
            }
            _ => false,
        }
    }

    /// Snapshot of the state for `GetData` (`None` outside learning).
    pub fn snapshot(&self) -> Option<Learning> {
        self.current.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learn_then_capture_retains_the_first_code() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        assert_eq!(s.device(), Some("USB Keyboard"));
        assert_eq!(s.snapshot(), Some(Learning { device: "USB Keyboard".into(), captured: None }));
        assert!(s.capture("USB Keyboard", 115));
        assert_eq!(
            s.snapshot(),
            Some(Learning { device: "USB Keyboard".into(), captured: Some(115) })
        );
        // second press: nothing left to capture, learning is over
        assert!(!s.capture("USB Keyboard", 42));
        assert_eq!(s.snapshot().unwrap().captured, Some(115));
        // and the device emits its commands again
        assert_eq!(s.device(), None);
    }

    #[test]
    fn capture_ignores_other_devices() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        assert!(!s.capture("eHome", 115));
        assert_eq!(s.snapshot().unwrap().captured, None);
    }

    #[test]
    fn capture_without_learning_does_nothing() {
        let mut s = LearnState::default();
        assert!(!s.capture("USB Keyboard", 115));
        assert_eq!(s.snapshot(), None);
    }

    #[test]
    fn cancel_clears_the_state() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.cancel();
        assert_eq!(s.snapshot(), None);
        assert_eq!(s.device(), None);
    }

    #[test]
    fn a_new_learn_replaces_the_previous_one() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.capture("USB Keyboard", 115);
        s.learn("eHome");
        assert_eq!(s.snapshot(), Some(Learning { device: "eHome".into(), captured: None }));
        assert_eq!(s.device(), Some("eHome"));
    }

    #[test]
    fn cancel_if_abandons_only_the_targeted_device() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.cancel_if("eHome");
        assert_eq!(s.device(), Some("USB Keyboard"));
        s.cancel_if("USB Keyboard");
        assert_eq!(s.snapshot(), None);
    }

    #[test]
    fn snapshot_serializes_as_expected() {
        let mut s = LearnState::default();
        assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), serde_json::Value::Null);
        s.learn("USB Keyboard");
        assert_eq!(
            serde_json::to_value(s.snapshot()).unwrap(),
            serde_json::json!({ "device": "USB Keyboard", "captured": null })
        );
        s.capture("USB Keyboard", 115);
        assert_eq!(
            serde_json::to_value(s.snapshot()).unwrap(),
            serde_json::json!({ "device": "USB Keyboard", "captured": 115 })
        );
    }
}
