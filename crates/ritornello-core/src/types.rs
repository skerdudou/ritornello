#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Title(String),
    /// Title announced by the stream itself (ICY metadata, key `icy-title`).
    ///
    /// Distinct from `Title`, which comes from `media-title`: the latter falls
    /// back to the stream URL when the station sends nothing, which would show a
    /// URL as the title. `IcyTitle` therefore only carries what the station
    /// actually emitted.
    IcyTitle(String),
    /// Tags carried by the **file** currently playing, as mpv exposes them in
    /// that same `metadata` property. Ignored by the core while the Source has
    /// declared the local file it plays (`SourceMessage::media_path`): the
    /// core then reads that file itself, tied to the identity. What remains is
    /// what has no declared file — an Ogg stream's Vorbis comments, notably.
    ///
    /// Distinct from `IcyTitle`, and mutually exclusive with it: one describes a
    /// stream, the other a file. The track's `origin` field is already filled at
    /// extraction time, so the event carries a track ready to display.
    ///
    /// **Boxed**, unlike the other variants: `Track` now carries an origin map,
    /// and its size far exceeds that of a string. Without the box, *every*
    /// player event — one per track, state or title change — would cost the
    /// size of the largest.
    ///
    /// `lyrics` are the words embedded in those same tags (see
    /// `mpv::file_lyrics`). Emitted when either half is present; `track`
    /// may then be `None` for a file whose only tag is its lyrics.
    ///
    /// `path` is mpv's `path` when the tags were reported: which file they
    /// describe. mpv reports property changes apart from its `start-file`
    /// events, so the tags of the file it just left can reach the core
    /// after the next file started — measured on the device: "next" then
    /// "previous" on a share too slow to read, and the previous track was
    /// described with the next one's tags. `None` when mpv has reported no
    /// path yet (and in tests that do not care).
    FileTags {
        track: Option<Box<ritornello_proto::Track>>,
        lyrics: Option<ritornello_proto::Lyrics>,
        path: Option<String>,
    },
    /// mpv's `file-loaded` IPC event: documented (read from mpv's own
    /// manual, not measured against a real running mpv — no session
    /// available to confirm it) as sent "after a file has been loaded and
    /// playback has started". Used in place of `idle-active` turning
    /// false, which this project found (same caveat: unmeasured) does not
    /// reliably fire again once mpv is already playing something else —
    /// see `player::mpv`'s own comment on that arm.
    PlaybackActive,
    PlaybackIdle,
    /// mpv's `start-file` IPC event: a file is about to load and mpv is
    /// about to rebuild its audio filter chain from the recorded `af`.
    /// Measured on mpv 0.37: a `set_property af` sent at this moment applies
    /// from the first frame of the file (`audio-reconfig` comes several times
    /// and too early, `playback-restart` leaves 100 to 730 ms of the old
    /// curve).
    FileStarting,
    /// mpv's `playback-restart` IPC event: the file is decoding and its
    /// audio filter chain is live. Measured on mpv 0.37: `af-command` is
    /// refused from `start-file` until this event (0.1 to 3 s, a radio
    /// connecting) and accepted from it on. Also sent after every seek.
    PlaybackRestarted,
    TrackChanged(i64),
}
