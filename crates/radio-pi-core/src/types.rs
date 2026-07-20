#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Title(String),
    // idle-active=false : la lecture a démarré
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
}
