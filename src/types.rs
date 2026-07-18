use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Radio,
    Cd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Preset(u8),
    StationNext,
    StationPrev,
    VolumeUp,
    VolumeDown,
    Mute,
    ToggleMode,
    PlayPause,
    NextTrack,
    PrevTrack,
    Stop,
    Eject,
    Power,
    ReloadStations,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Title(String),
    // idle-active=false : la lecture a démarré
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
    CdInserted,
    CdRemoved,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub line1: String,
    pub line2: String,
    pub line3: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscInfo {
    pub artist: String,
    pub album: String,
    pub tracks: Vec<String>,
}
