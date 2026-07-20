use radio_pi_proto::Command;

pub fn map_key(code: u16) -> Option<Command> {
    Some(match code {
        2..=10 => Command::Select((code - 1) as u8),
        513..=521 => Command::Select((code - 512) as u8),
        115 => Command::VolumeUp,
        114 => Command::VolumeDown,
        113 => Command::Mute,
        402 => Command::Next,
        403 => Command::Prev,
        164 => Command::PlayPause,
        163 => Command::NextTrack,
        165 => Command::PrevTrack,
        166 => Command::Stop,
        161 => Command::Eject,
        226 => Command::SourceCycle,
        116 | 356 => Command::Power,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chiffres_vers_select() {
        assert_eq!(map_key(2), Some(Command::Select(1)));
        assert_eq!(map_key(10), Some(Command::Select(9)));
        assert_eq!(map_key(513), Some(Command::Select(1)));
        assert_eq!(map_key(521), Some(Command::Select(9)));
    }

    #[test]
    fn touches_media_et_volume() {
        assert_eq!(map_key(115), Some(Command::VolumeUp));
        assert_eq!(map_key(114), Some(Command::VolumeDown));
        assert_eq!(map_key(113), Some(Command::Mute));
        assert_eq!(map_key(402), Some(Command::Next));
        assert_eq!(map_key(403), Some(Command::Prev));
        assert_eq!(map_key(164), Some(Command::PlayPause));
        assert_eq!(map_key(163), Some(Command::NextTrack));
        assert_eq!(map_key(165), Some(Command::PrevTrack));
        assert_eq!(map_key(166), Some(Command::Stop));
        assert_eq!(map_key(161), Some(Command::Eject));
        assert_eq!(map_key(226), Some(Command::SourceCycle));
        assert_eq!(map_key(116), Some(Command::Power));
        assert_eq!(map_key(356), Some(Command::Power));
    }

    #[test]
    fn touche_inconnue_ignoree() {
        assert_eq!(map_key(30), None);
    }
}
