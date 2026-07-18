use crate::types::Command;

/// Codes evdev (linux/input-event-codes.h). Le récepteur USB MCE remonte soit
/// KEY_1..KEY_9 (mode clavier), soit KEY_NUMERIC_* (keymap rc6_mce) : on mappe
/// les deux. En cas de doute sur une touche, lancer `evtest` sur le Pi et
/// ajuster ici.
pub fn map_key(code: u16) -> Option<Command> {
    Some(match code {
        2..=10 => Command::Preset((code - 1) as u8),      // KEY_1..KEY_9
        513..=521 => Command::Preset((code - 512) as u8), // KEY_NUMERIC_1..9
        115 => Command::VolumeUp,                         // KEY_VOLUMEUP
        114 => Command::VolumeDown,                       // KEY_VOLUMEDOWN
        113 => Command::Mute,                             // KEY_MUTE
        402 => Command::StationNext,                      // KEY_CHANNELUP
        403 => Command::StationPrev,                      // KEY_CHANNELDOWN
        164 => Command::PlayPause,                        // KEY_PLAYPAUSE
        163 => Command::NextTrack,                        // KEY_NEXTSONG
        165 => Command::PrevTrack,                        // KEY_PREVIOUSSONG
        166 => Command::Stop,                             // KEY_STOPCD
        161 => Command::Eject,                            // KEY_EJECTCD
        226 => Command::ToggleMode,                       // KEY_MEDIA
        116 | 356 => Command::Power,                      // KEY_POWER / KEY_POWER2
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Command;

    #[test]
    fn chiffres_vers_presets() {
        assert_eq!(map_key(2), Some(Command::Preset(1)));   // KEY_1
        assert_eq!(map_key(10), Some(Command::Preset(9)));  // KEY_9
        assert_eq!(map_key(513), Some(Command::Preset(1))); // KEY_NUMERIC_1 (rc6_mce)
        assert_eq!(map_key(521), Some(Command::Preset(9))); // KEY_NUMERIC_9
    }

    #[test]
    fn touches_media_et_volume() {
        assert_eq!(map_key(115), Some(Command::VolumeUp));
        assert_eq!(map_key(114), Some(Command::VolumeDown));
        assert_eq!(map_key(113), Some(Command::Mute));
        assert_eq!(map_key(402), Some(Command::StationNext));
        assert_eq!(map_key(403), Some(Command::StationPrev));
        assert_eq!(map_key(164), Some(Command::PlayPause));
        assert_eq!(map_key(163), Some(Command::NextTrack));
        assert_eq!(map_key(165), Some(Command::PrevTrack));
        assert_eq!(map_key(166), Some(Command::Stop));
        assert_eq!(map_key(161), Some(Command::Eject));
        assert_eq!(map_key(226), Some(Command::ToggleMode));
        assert_eq!(map_key(116), Some(Command::Power));
        assert_eq!(map_key(356), Some(Command::Power));
    }

    #[test]
    fn touche_inconnue_ignoree() {
        assert_eq!(map_key(30), None); // KEY_A
    }
}
