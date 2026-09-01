use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "arg")]
pub enum Command {
    Select(u8),
    /// The preset/track distinction is not carried by the command itself but
    /// by the active source: the radio interprets `Next`/`Prev` as a preset
    /// change, the CD player as next/previous track.
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
    /// Step forward in what is playing. The step lives in the core (setting
    /// `seek_step_s`), exactly like the 5 % of the volume: the key carries no
    /// quantity, so changing the step does not require reprogramming a remote.
    SeekForward,
    SeekBackward,
    /// Absolute positioning, in seconds. Serves the clickable bar of the SPA;
    /// no physical key emits it.
    SeekTo(u32),
    /// Absolute volume, in percent. Serves MPD's `setvol`; no physical key
    /// emits it — same reason for being as `SeekTo`.
    ///
    /// Stacking `VolumeUp`s would not replace this command: the step is a
    /// setting and not a constant, and each step writes an overlay on the
    /// screen.
    SetVolume(u8),
    /// Source designated by its **name**, where `SourceCycle` only knows how to
    /// move one notch forward. Serves MPD's `load "radio"`.
    ///
    /// An unknown name is silently ignored by the core, like an unbound key:
    /// the emitter is the one who knows what it offers.
    SelectSource(String),
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
    fn json_roundtrip_with_argument() {
        let c = Command::Select(3);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"cmd":"Select","arg":3}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), c);
    }

    #[test]
    fn json_roundtrip_without_argument() {
        let c = Command::Stop;
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"cmd":"Stop"}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), c);
    }

    #[test]
    fn input_message_without_held_is_a_bare_command() {
        // Backward compatibility: an input plugin that writes a plain Command
        // line keeps working, and the non-held serialization is byte-identical.
        let msg: InputMessage = serde_json::from_str(r#"{"cmd":"VolumeUp"}"#).unwrap();
        assert_eq!(msg, InputMessage { cmd: Command::VolumeUp, held: false });
        assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"cmd":"VolumeUp"}"#);
    }

    #[test]
    fn input_message_held_roundtrip_with_argument() {
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
    fn plus10_and_select_zero_roundtrip() {
        // Plus10 is the remote's +10 key, Select(0) its 0 key: both must
        // travel as they are.
        let p = Command::Plus10;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"cmd":"Plus10"}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), p);
        let z = Command::Select(0);
        let json = serde_json::to_string(&z).unwrap();
        assert_eq!(json, r#"{"cmd":"Select","arg":0}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), z);
    }

    #[test]
    fn seek_commands_roundtrip() {
        for (cmd, expected) in [
            (Command::SeekForward, r#"{"cmd":"SeekForward"}"#),
            (Command::SeekBackward, r#"{"cmd":"SeekBackward"}"#),
            (Command::SeekTo(198), r#"{"cmd":"SeekTo","arg":198}"#),
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(json, expected);
            assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
        }
    }

    #[test]
    fn absolute_value_commands_roundtrip() {
        for (cmd, expected) in [
            (Command::SetVolume(40), r#"{"cmd":"SetVolume","arg":40}"#),
            (Command::SelectSource("radio".into()), r#"{"cmd":"SelectSource","arg":"radio"}"#),
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(json, expected);
            assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
        }
    }
}
