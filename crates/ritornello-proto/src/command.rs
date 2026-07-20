use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "arg")]
pub enum Command {
    Select(u8),
    Next,
    Prev,
    NextTrack,
    PrevTrack,
    VolumeUp,
    VolumeDown,
    Mute,
    SourceCycle,
    PlayPause,
    Stop,
    Eject,
    Power,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json_avec_argument() {
        let c = Command::Select(3);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"cmd":"Select","arg":3}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), c);
    }

    #[test]
    fn roundtrip_json_sans_argument() {
        let c = Command::Stop;
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"cmd":"Stop"}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), c);
    }
}
