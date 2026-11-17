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
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ChimeRoute {
    /// Nothing loaded: the chime is loaded on its own (`Player::play_aside`).
    #[default]
    Idle,
    /// A file plays: the chime is mixed into its chain, at its rate and
    /// channel count (`lock_sound::mix_filter`).
    Live { sample_rate: u32, channels: u32 },
    /// A file is paused: the chime is mixed over the music silenced
    /// (`lock_sound::mix_filter_over_silence`), mpv unpaused for it
    /// (`Player::sound_while_paused`), then paused again
    /// (`Player::end_sound_while_paused`). `resume_at` is where to put the
    /// file back, `time-pos` at the pause, when mpv says it is seekable;
    /// `None` for a stream that is not (a radio): the chime then consumes
    /// that much of the stream, and the audio mpv had queued at the pause
    /// plays at full gain before the chime (no flush is possible).
    Paused { sample_rate: u32, channels: u32, resume_at: Option<f64> },
    /// Nothing can carry a chime now: still loading (no audio parameters),
    /// a real load in flight, a paused seekable file too close to its end
    /// (the chime would end it and advance the playlist), or a paused chime
    /// already sounding (see `Player::chime_route`).
    Silent,
}

/// How much of a paused seekable file must remain after the chime for it to
/// play (`ChimeRoute::Paused`). Measured by the paused spike on mpv 0.37,
/// paused 0.2 s before the end: the unpause reaches the end within 6 ms,
/// `end-file` then idle reach the core (which advances to the next track),
/// and not one sample of the chime comes out (`amix duration=first` ends
/// with the music). `keep-open` saves no chime either.
pub const PAUSED_CHIME_END_MARGIN_S: f64 = 1.0;

/// Whether a chime of `chime` played from `position` of a paused seekable
/// file ends before the file does, by `PAUSED_CHIME_END_MARGIN_S`. No
/// `duration` known: no guard. The one rule for the route's first chime
/// (`MpvPlayer::chime_route`) and the core's second (`core::lock_sound`).
pub fn paused_chime_fits(position: f64, duration: Option<f64>, chime: std::time::Duration) -> bool {
    duration.is_none_or(|d| d - position >= chime.as_secs_f64() + PAUSED_CHIME_END_MARGIN_S)
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
    /// `Paused` while a file is paused; `Silent` when the file has no audio
    /// parameters yet (loading), or when a paused seekable file ends sooner
    /// than `chime` + 1 s from where it stands.
    async fn chime_route(&self, chime: std::time::Duration) -> Result<ChimeRoute>;
    /// Loads a generated sound while mpv is idle; none of that load's events
    /// reach the core (mpv's `AsideGate`), so the core keeps believing
    /// nothing plays — which is the truth it acts on.
    async fn play_aside(&self, uri: &str) -> Result<()>;
    /// Starts a chime on a paused file (`ChimeRoute::Paused`): writes
    /// `chain` (the chain mpv holds with `lock_sound::mix_filter_over_silence`
    /// after it), puts the file back at `resume_at` when `Some` (a seek that
    /// flushes the audio queued at the pause, which would otherwise play at
    /// full gain before the chime), and unpauses. From the first call until
    /// `end_sound_while_paused`, the core sees none of it: no
    /// `PlaybackRestarted`, and `progress` answers what it answered at the
    /// pause. A second call while one sounds (another lock change) does the
    /// same again and keeps the first frozen progress.
    ///
    /// **Whatever this returns, `end_sound_while_paused` must follow**: an
    /// error may come after mpv applied the chain, and the music would stay
    /// at gain 0.
    async fn sound_while_paused(&self, chain: &str, resume_at: Option<f64>) -> Result<()>;
    /// Ends a paused chime: pauses again, writes `chain` (the plain chain
    /// mpv held before the chime), waits (bounded) for mpv to rebuild it,
    /// then puts the file back at `resume_at` exactly when `Some`. Every
    /// step is tried even when one fails, so the plain chain is written
    /// whatever happens; the first error is returned. The restore seek's
    /// `PlaybackRestarted`, which mpv sends only once the user resumes,
    /// never reaches the core either, unless a new file starts first; and
    /// when the file was put back, `progress` stays at the pause's until the
    /// user's next transport call (pause toggle, seek, next, prev, chapter,
    /// stop, a load) or a file start.
    async fn end_sound_while_paused(&self, chain: &str, resume_at: Option<f64>) -> Result<()>;
}
