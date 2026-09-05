pub mod mpv;

use anyhow::Result;

/// Where playback stands and how long it lasts, as the player knows them at
/// this instant.
///
/// Both together rather than two methods: they are read at the same moment,
/// for the same frame, and a caller taking only one of them would publish an
/// inconsistent pair (the position of one track, the duration of the next).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    pub position_s: Option<f64>,
    pub duration_s: Option<f64>,
}

#[async_trait::async_trait]
pub trait Player: Send + Sync + 'static {
    async fn play(&self, uri: &str) -> Result<()>;
    /// Loads `uri` as a **playlist**, expanded right away and already
    /// positioned on entry `start` (`None` = from the beginning).
    ///
    /// Distinct from `play`, and that is a lesson paid for: `loadfile` on a
    /// `.m3u` only expands it after the fact. Measured on mpv 0.37 —
    /// `playlist-count` is 1, then 3 only after an `end-file`/`start-file`.
    /// Anything following immediately therefore landed out of bounds, and the
    /// expansion sent playback back to the beginning.
    ///
    /// **The index is a parameter of the load, not a correction sent after
    /// it**, and that too is a lesson paid for — a later one. Loading the list
    /// and repositioning it in two steps leaves mpv the time to really open
    /// entry 0: measured, it publishes that entry's `path` before the
    /// reposition lands. The core, which listens to `path` to know what is
    /// playing, then went and read a cover off a track nobody had asked for —
    /// over a network share — and the display flipped through it. Positioning
    /// **cannot** be a separate method for that reason: the interface would
    /// hand back the very sequence this signature exists to forbid.
    async fn load_list(&self, uri: &str, start: Option<i64>) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn toggle_pause(&self) -> Result<()>;
    async fn next(&self) -> Result<()>;
    async fn prev(&self) -> Result<()>;
    async fn set_volume(&self, volume: u8) -> Result<()>;
    async fn set_mute(&self, mute: bool) -> Result<()>;
    async fn set_audio_device(&self, device: &str) -> Result<()>;
    /// Current position and duration. `Ok` with `None` fields when the player
    /// does not know: an unknown position is a normal case (nothing loaded,
    /// the stream has no duration), never a failure.
    async fn progress(&self) -> Result<Progress>;
    /// Relative move, in seconds (negative to go backwards).
    async fn seek_relative(&self, delta_s: i64) -> Result<()>;
    /// Absolute move, in seconds from the beginning.
    async fn seek_absolute(&self, position_s: u32) -> Result<()>;
}
