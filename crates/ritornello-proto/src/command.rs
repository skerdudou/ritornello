use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "arg")]
pub enum Command {
    Select(u8),
    /// La distinction présélection/piste n'est pas portée par la commande
    /// elle-même mais par la source active : la radio interprète
    /// `Next`/`Prev` comme un changement de présélection, le lecteur CD
    /// comme piste suivante/précédente.
    Next,
    Prev,
    VolumeUp,
    VolumeDown,
    Mute,
    SourceCycle,
    PlayPause,
    Stop,
    Eject,
    Power,
    /// Cumulative tens key of the remote: each press shifts the next digit
    /// key by +10 (`+10` then `4` selects 14, `+10 +10` then `0` selects 20).
    /// The pending offset lives in the core — which also displays it and
    /// expires it; input plugins just relay the key press.
    Plus10,
}

/// One line of the input protocol: the command, plus whether it comes from a
/// **key being held down** (kernel autorepeat) rather than a fresh press.
///
/// `held` is additive and backward compatible: a plugin that writes a plain
/// `Command` line parses as `held: false`, and `held: false` is not
/// serialized, so existing messages stay byte-identical on the wire. The core
/// paces held volume commands itself (see `Core::handle_input`); `held` on any
/// other command is ignored there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMessage {
    #[serde(flatten)]
    pub cmd: Command,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

impl From<Command> for InputMessage {
    fn from(cmd: Command) -> Self {
        Self { cmd, held: false }
    }
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

    #[test]
    fn input_message_sans_held_est_une_commande_nue() {
        // Backward compatibility: an input plugin that writes a plain Command
        // line keeps working, and the non-held serialization is byte-identical.
        let msg: InputMessage = serde_json::from_str(r#"{"cmd":"VolumeUp"}"#).unwrap();
        assert_eq!(msg, InputMessage { cmd: Command::VolumeUp, held: false });
        assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"cmd":"VolumeUp"}"#);
    }

    #[test]
    fn input_message_held_roundtrip_avec_argument() {
        let msg: InputMessage = serde_json::from_str(r#"{"cmd":"Select","arg":3,"held":true}"#).unwrap();
        assert_eq!(msg, InputMessage { cmd: Command::Select(3), held: true });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<InputMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn input_message_from_command() {
        assert_eq!(InputMessage::from(Command::Stop), InputMessage { cmd: Command::Stop, held: false });
    }

    #[test]
    fn plus10_et_select_zero_font_le_tour() {
        // Plus10 est la touche +10 de la télécommande, Select(0) sa touche 0 :
        // les deux doivent voyager tels quels.
        let p = Command::Plus10;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"cmd":"Plus10"}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), p);
        let z = Command::Select(0);
        let json = serde_json::to_string(&z).unwrap();
        assert_eq!(json, r#"{"cmd":"Select","arg":0}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), z);
    }
}
