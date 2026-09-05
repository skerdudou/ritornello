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
    /// that same `metadata` property.
    ///
    /// Distinct from `IcyTitle`, and mutually exclusive with it: one describes a
    /// stream, the other a file. The track's `origin` field is already filled at
    /// extraction time, so the event carries a track ready to display.
    ///
    /// **Boxed**, unlike the other variants: `Track` now carries an origin map,
    /// and its size far exceeds that of a string. Without the box, *every*
    /// player event — one per track, state or title change — would cost the
    /// size of the largest.
    FileTags(Box<ritornello_proto::Track>),
    /// Path of the file mpv actually opened (`path` property).
    ///
    /// The only way the core learns this detail: it never extracts it from the
    /// Source's opaque identity, on principle (see `OBSERVED` in
    /// `player::mpv`). Used solely to attempt extracting the embedded cover — a
    /// stream has no usable path, and mpv then republishes it as is (its URL),
    /// without consequence: the extraction attempt stops by itself on a scheme.
    Path(String),
    /// mpv's `file-loaded` IPC event: documented (read from mpv's own
    /// manual, not measured against a real running mpv — no session
    /// available to confirm it) as sent "after a file has been loaded and
    /// playback has started". Used in place of `idle-active` turning
    /// false, which this project found (same caveat: unmeasured) does not
    /// reliably fire again once mpv is already playing something else —
    /// see `player::mpv`'s own comment on that arm.
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
}
