use crate::view::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum SourceReq {
    Activate,
    /// Réveil piloté par le plugin (boot / sortie de veille). Défaut côté SDK :
    /// se comporte comme `Activate` ; un plugin peut surcharger `wake()`.
    Wake,
    Deactivate,
    Select(u8),
    Next,
    Prev,
    Eject,
    SetLocale(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: SourceReq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "data")]
pub enum SourceAction {
    Noop,
    Play { uri: String },
    Stop,
    PlayerNext,
    PlayerPrev,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMessage {
    /// `Some(id)` = réponse corrélée à une requête ; `None` = notification spontanée.
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub action: Option<SourceAction>,
    #[serde(default)]
    pub view: Option<View>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_roundtrip() {
        let r = SourceRequest { id: 4, req: SourceReq::Wake };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"req\":\"Wake\""));
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, SourceReq::Wake);
    }

    #[test]
    fn set_locale_roundtrip() {
        let r = SourceRequest { id: 9, req: SourceReq::SetLocale("fr".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"req\":\"SetLocale\""));
        assert!(json.contains("\"arg\":\"fr\""));
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, SourceReq::SetLocale("fr".into()));
    }

    #[test]
    fn request_roundtrip() {
        let r = SourceRequest { id: 7, req: SourceReq::Select(3) };
        let json = serde_json::to_string(&r).unwrap();
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.req, SourceReq::Select(3));
    }

    #[test]
    fn message_reponse_avec_action_et_vue() {
        let m = SourceMessage {
            id: Some(1),
            action: Some(SourceAction::Play { uri: "http://fip".into() }),
            view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, Some(1));
        assert_eq!(back.action, Some(SourceAction::Play { uri: "http://fip".into() }));
    }

    #[test]
    fn message_notification_sans_id() {
        let m = SourceMessage { id: None, action: None, view: Some(View::default()) };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, None);
        assert_eq!(back.action, None);
    }
}
