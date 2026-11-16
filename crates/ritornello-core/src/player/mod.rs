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

/// Where a chime can play at this instant (see `crate::lock_sound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChimeRoute {
    /// Nothing loaded: the chime is loaded on its own (`Player::play_aside`).
    #[default]
    Idle,
    /// A file plays: the chime is mixed into its chain, at its rate and
    /// channel count (`lock_sound::mix_filter`).
    Live { sample_rate: u32, channels: u32 },
    /// Something is loaded but nothing is heard (paused, or still loading):
    /// no chime can play.
    Silent,
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
    /// Positions playback on chapter `n` of what is loaded. Used by the cd,
    /// whose tracks are expected to be chapters; a seek, never a reload —
    /// not measured on real hardware, see `SourceAction::PlayerChapter`.
    async fn set_chapter(&self, n: i64) -> Result<()>;
    /// Loop the playing file at its natural end (mpv's `loop-file`), or stop
    /// doing so. A setting of the player, not of the file: it survives the
    /// loads that follow until it is set again.
    async fn set_loop_track(&self, on: bool) -> Result<()>;
    /// Decides which program mpv's `ytdl_hook` runs for the **next** load:
    /// `Some(path)` names the source's `yt-dlp`, `None` names one that does
    /// nothing (`mpv::YTDL_NOWHERE`), so no `yt-dlp` runs for a load that did
    /// not ask for one. A setting of the player that stays until set again,
    /// so the core sends it before **every** `Play`. It never unloads the
    /// hook script (see `MpvPlayer`'s implementation for what that did).
    async fn set_ytdl(&self, path: Option<&std::path::Path>) -> Result<()>;
    /// Replaces the whole audio filter chain (`set_property af`); an empty
    /// chain removes every filter. mpv rebuilds the chain, so it is sent at
    /// `start-file`, when the equalizer is enabled or disabled, and
    /// mid-playback by the lock chime (`core::lock_sound`), which mixes
    /// itself in after the equalizer and is removed the same way. A rewrite
    /// that changes the equalizer's filters mid-playback clicked (the
    /// equalizer's prototype); the chime's addition and removal, which leave
    /// them as they are, were measured without a click by the lock-sound
    /// spike (mpv 0.37 / FFmpeg 6.1, `ao=pcm`; never heard on the device,
    /// see `docs/installation.md`). A change of one parameter goes through
    /// `audio_filter_command` instead.
    async fn set_audio_filters(&self, chain: &str) -> Result<()>;
    /// Changes one parameter of one labelled filter of the running chain
    /// (`af-command`), without a click. The change is lost at the next file
    /// load, which rebuilds the chain from the recorded `af`.
    async fn audio_filter_command(&self, cmd: &crate::equalizer::AfCommand) -> Result<()>;
    /// Where a chime can play now: `Idle` when mpv has nothing loaded or
    /// plays only an earlier chime (`play_aside`, replaced by the next);
    /// `Live` while a file plays (rate and channels from `audio-params`);
    /// `Silent` when paused, or when the file has no audio parameters yet
    /// (loading).
    async fn chime_route(&self) -> Result<ChimeRoute>;
    /// Loads a generated sound while mpv is idle; none of that load's events
    /// reach the core (mpv's `AsideGate`), so the core keeps believing
    /// nothing plays — which is the truth it acts on.
    async fn play_aside(&self, uri: &str) -> Result<()>;
}
