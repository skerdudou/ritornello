use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "req")]
pub enum SinkReq {
    Activate,
    Deactivate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: SinkReq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkMessage {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub audio_device: Option<String>,
    #[serde(default)]
    pub connected: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = SinkRequest { id: 1, req: SinkReq::Activate };
        let json = serde_json::to_string(&r).unwrap();
        let back: SinkRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.req, SinkReq::Activate);
    }

    #[test]
    fn message_avec_peripherique_audio() {
        let m = SinkMessage {
            id: Some(1),
            audio_device: Some("alsa/bluealsa:DEV=XX".into()),
            connected: None,
            error: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SinkMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.audio_device.as_deref(), Some("alsa/bluealsa:DEV=XX"));
    }
}
