use anyhow::{Context, Result};
use ritornello_proto::{NextAlarm, Overlay, Playback, PlayerState, SleepTimerState, Weekday};
use std::io::Write;
use std::path::Path;

// `libc` 0.2 declares `tzset` for Windows only, so it is declared here, as in
// the core. Only glibc builds are shipped (armv7 and aarch64 `-gnu`).
unsafe extern "C" {
    fn tzset();
}

/// What gets written in place of the source name when the core has none.
const NO_SOURCE: &str = "—";

/// The device's local time, in hours (0-23) and minutes.
///
/// `None` when the system clock is not convertible — before the network has
/// set a Raspberry Pi's time, for example, a Pi having no battery. The display
/// then writes the standby screen without a clock, rather than a wrong time.
///
/// **`libc::localtime_r` and not a new date crate.** The call is already this
/// repository's idiom for what the C library can do on its own (see
/// `system.rs` and the cd plugin), and `libc` is already there. Adding `chrono`
/// would cost a dependency and its transitive timezone one, for two integers.
///
/// **The zone is re-read on every call** (`tzset`): glibc's `localtime_r`
/// loads it once per process, and the zone can now be changed from the web
/// page, so without this the standby clock would show the old zone until the
/// plugin restarts. `tzset()` re-reads `/etc/localtime` when `TZ` is unset
/// and costs one `stat` when the file is unchanged. This is only reached in
/// standby (see `show`), one call per 10 s tick.
///
/// The reentrant variant, the only safe one in a process with several threads.
pub fn local_time() -> Option<(u32, u32)> {
    let seconds =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(seconds).ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // Safety: `tzset` takes no argument and only updates libc's own zone state,
    // which glibc guards with the lock `localtime_r` shares.
    unsafe { tzset() };
    // Safety: `localtime_r` fills `tm` and keeps no pointer to it; we pass it
    // two references valid for the duration of the call. It returns NULL —
    // never a half-written `tm` — for a time it cannot convert, which the test
    // below treats as "no time".
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let (h, m) = (u32::try_from(tm.tm_hour).ok()?, u32::try_from(tm.tm_min).ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// `13:05`, or `1:05 PM` — depending on the setting the state frame carries.
///
/// In 12 h, midnight is written `12:00 AM` and noon `12:00 PM`: that is the
/// Anglo-Saxon convention, and a `0:00 AM` exists nowhere. Hours are not
/// zero-padded in this form (`1:05 PM`, not `01:05 PM`), where the 24 h form
/// pads them — the two usages differ, and that is what the user reads
/// elsewhere.
pub fn format_time(hours: u32, minutes: u32, twelve_hour: bool) -> String {
    if !twelve_hour {
        return format!("{hours:02}:{minutes:02}");
    }
    let (h, suffix) = match hours {
        0 => (12, "AM"),
        1..=11 => (hours, "AM"),
        12 => (12, "PM"),
        _ => (hours - 12, "PM"),
    };
    format!("{h}:{minutes:02} {suffix}")
}

/// Width of the console in columns, as far as the layout is concerned: the
/// owner's tty shows about twenty. The alarm and the sleep timer are optional
/// additions to a line, and are left out rather than truncated or wrapped when
/// they would not fit in it.
const COLUMNS: usize = 20;

/// Seconds since the Unix epoch, or `None` before the system clock is set.
pub fn unix_now() -> Option<u64> {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Minutes left on the sleep timer, rounded **up**: `0:30` left reads `1m`,
/// so the figure only reaches `0m` once `ends_at_unix_s` has passed. That
/// instant follows the phase: the deadline while armed, then the 5-minute
/// cap while waiting for the end of the track, then the end of the fade —
/// so waiting reads up to `5m` and fading `1m`, not `0m`; `0m` shows only in
/// the gap before the core's next frame.
///
/// Computed here from the deadline: the core does not push a frame per second
/// for the timer, only when its state changes.
pub fn timer_minutes(timer: &SleepTimerState, now_unix_s: u64) -> u64 {
    timer.ends_at_unix_s.saturating_sub(now_unix_s).div_ceil(60)
}

/// `Z 25m`: the sleep timer as a line suffix. Plain letter and not a glyph:
/// the console font (Lat15-Fixed16) has no moon nor bell, and a missing glyph
/// renders as a blank or a box.
fn timer_text(timer: &SleepTimerState, now_unix_s: u64) -> String {
    format!("Z {}m", timer_minutes(timer, now_unix_s))
}

/// Three-letter English day. Like the rest of this display it is not
/// translated (it has no language); a day name is still more readable than a
/// number, and short enough for the line.
fn weekday_abbr(day: Weekday) -> &'static str {
    match day {
        Weekday::Sunday => "Sun",
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
    }
}

/// `A Mon 07:00`: the next alarm, in the device's 12/24 h form. `A` and not a
/// bell: see [`timer_text`].
fn alarm_text(alarm: &NextAlarm, twelve_hour: bool) -> String {
    format!(
        "A {} {}",
        weekday_abbr(alarm.weekday),
        format_time(u32::from(alarm.hour), u32::from(alarm.minute), twelve_hour)
    )
}

/// `base` followed by two spaces and `extra`, when the result fits in
/// [`COLUMNS`]; otherwise `base` alone.
fn append_if_fits(base: String, extra: &str) -> String {
    let joined = if base.is_empty() { extra.to_string() } else { format!("{base}  {extra}") };
    if joined.chars().count() <= COLUMNS { joined } else { base }
}

/// Like [`compose`], but with the time to write in standby.
///
/// Separate so that the layout stays **pure**: `compose` reads no clock, so
/// every case is tested on a chosen time. `None` = no time to display (system
/// clock unusable, or state outside standby).
#[cfg(test)]
pub fn compose_at(state: &PlayerState, now: Option<(u32, u32)>) -> [String; 3] {
    compose_full(state, now, None)
}

/// [`compose_at`] plus the Unix time, which the sleep timer's countdown needs.
/// `None` for the Unix time = no countdown shown. Pure, like its siblings.
pub fn compose_full(
    state: &PlayerState,
    now: Option<(u32, u32)>,
    unix_now_s: Option<u64>,
) -> [String; 3] {
    if state.overlay.is_none() && state.standby {
        // **The time in standby**, requested by the owner: it is the only
        // moment the screen has nothing else to say, and a clock is more useful
        // there than a black tty. The standby word stays on the first line — it
        // says *why* nothing plays — and the time takes the second.
        let time = now
            .map(|(h, m)| format_time(h, m, state.clock.twelve_hour))
            .unwrap_or_default();
        // The next alarm shares the clock's line when it fits: that is the
        // moment it is useful ("will it wake me?"). Without a clock the line
        // stays empty: an alarm with no time next to it would be a puzzle.
        let line2 = match (&state.next_alarm, time.is_empty()) {
            (Some(alarm), false) => {
                append_if_fits(time, &alarm_text(alarm, state.clock.twelve_hour))
            }
            _ => time,
        };
        return [state.status.clone().unwrap_or_default(), line2, String::new()];
    }
    let mut lines = compose(state);
    // **The sleep timer rides on line 1**, which is the one with room: the
    // source name and the preset number are short, where line 2 carries a
    // station or album name and line 3 the title, both free text that fills
    // the width. Overlays take the whole screen and show no countdown.
    if let (Some(timer), Some(unix), None) = (&state.sleep_timer, unix_now_s, &state.overlay) {
        lines[0] = append_if_fits(std::mem::take(&mut lines[0]), &timer_text(timer, unix));
    }
    lines
}

/// Three lines for a text screen of about twenty columns, composed from the
/// structured state.
///
/// This is **where** the layout lives, and not in the core: another display
/// will write another one from the same frame, without changing anything in
/// the core.
///
/// Reads **no** clock: the standby time comes in through `compose_at`, which
/// delegates everything else to this function.
pub fn compose(state: &PlayerState) -> [String; 3] {
    // An overlay takes all the room: it is transient and it is what one wants
    // to read while it lasts. Owner's decision: the text arrives in one piece
    // from the core, and is displayed in one piece — on one line, where the
    // volume overlay took two lines before that project ("VOLUME" then
    // "65 %"). The owner saw the difference and accepted it: it is not a
    // regression.
    if let Some(o) = &state.overlay {
        return [overlay_text(o).to_string(), String::new(), String::new()];
    }
    if state.standby {
        return [state.status.clone().unwrap_or_default(), String::new(), String::new()];
    }
    // "SOURCE  n/total", and "SOURCE  n" when the total is unknown.
    //
    // Owner's choice, ruled during that project. Each source used to have its
    // own idiom, encoded in its sources_catalog: the radio wrote "RADIO  P3",
    // the cd "CD 1/3". A single display cannot replay them all without
    // hard-coding plugin names, which we refuse — hence a common idiom, which
    // gives the cd back the total it had lost and teaches the radio how many
    // stations are configured.
    //
    // A zero total ("nothing to number": empty tray) is not written: "1/0"
    // would be absurd. The case is reachable, `preset_count` being `Some(0)`
    // meaningfully in this protocol.
    // An empty `source` **is** the absence of a source: since hot
    // registration, the core starts even if no `source` plugin has answered,
    // waiting for a latecomer to announce itself. Without this fallback, the
    // three lines of the screen were empty — indistinguishable from a dead
    // display, while precisely everything works.
    //
    // A dash, and not a word: this display translates nothing. Everything it
    // writes reaches it already translated from the core (the status, the
    // standby word), it has neither a sources_catalog nor a current language —
    // a hard-coded `NO SOURCE` here would lie on a French device. The em dash
    // is already among its characters (see `title_line`).
    let name = if state.source.is_empty() {
        NO_SOURCE.to_string()
    } else {
        state.source.to_uppercase()
    };
    let line1 = match (state.preset, state.preset_count) {
        (Some(n), Some(total)) if total > 0 => format!("{name}  {n}/{total}"),
        (Some(n), _) => format!("{name}  {n}"),
        (None, _) => name,
    };
    // The playback state, as one glyph and one space at the head of the line
    // (two columns). Owner's decision. Overlay and standby return earlier and
    // show none. The glyphs are the ones the console font actually has:
    // measured on the Pi, it is Lat15-Fixed16 (console-setup FONTFACE=Fixed,
    // 8x16, C.UTF-8), whose unicode table contains U+25B6, U+2551 and U+25A0
    // (and the em dash above) but neither U+23F8 (pause) nor U+25BA. A glyph
    // missing from the font would render as a blank or a box.
    let line1 = format!("{} {line1}", playback_glyph(state.playback));
    // The preset name first, then the album, then the status: from the most
    // specific to the most generic.
    let line2 = state
        .preset_name
        .clone()
        .or_else(|| state.track.album.clone())
        .or_else(|| state.status.clone())
        .unwrap_or_default();
    let line3 = title_line(state.track.artist.as_deref(), state.track.title.as_deref())
        .unwrap_or_default();
    [line1, line2, line3]
}

/// One glyph per playback state; see the font note in `compose`.
fn playback_glyph(playback: Playback) -> char {
    match playback {
        Playback::Playing => '\u{25B6}',
        Playback::Paused => '\u{2551}',
        Playback::Stopped => '\u{25A0}',
    }
}

fn overlay_text(o: &Overlay) -> &str {
    match o {
        Overlay::Volume { text, .. } | Overlay::Tens { text, .. } | Overlay::Message { text, .. } => text,
    }
}

/// "artist — title" line, with its four fallbacks. Moved out of the core: it
/// is a layout decision, so it belongs to the display.
///
/// Partial information is better than nothing: the artist alone stays
/// displayed, because it already says something about what is being listened
/// to.
pub fn title_line(artist: Option<&str>, title: Option<&str>) -> Option<String> {
    match (artist, title) {
        (Some(a), Some(t)) => Some(format!("{a} — {t}")),
        (None, Some(t)) => Some(t.to_string()),
        (Some(a), None) => Some(a.to_string()),
        (None, None) => None,
    }
}

/// Text rendering for the console (ANSI: clears the screen, cursor top left).
/// \r\n because on /dev/tty1 canonical mode does not insert the carriage return.
///
/// `#[cfg(test)]`: since `ConsoleDisplay::show` memorizes its last rendering
/// (see below), production calls `render_lines` directly on the already
/// composed lines, to compare them with the previous ones before writing
/// anything. This function remains the tests' convenience, which do not have
/// that comparison to make and reason on a complete `PlayerState`.
#[cfg(test)]
fn render_console(state: &PlayerState) -> String {
    render_lines(&compose(state))
}

/// Assembles the ANSI rendering from already composed lines: shared by
/// `render_console` (which composes from a complete state, tests only) and
/// `ConsoleDisplay::show` (which needs the lines separately to compare them
/// with the previous ones before writing anything).
fn render_lines(lines: &[String; 3]) -> String {
    format!(
        "\x1b[2J\x1b[H\r\n  {}\r\n\r\n  {}\r\n\r\n  {}\r\n",
        sanitize(&lines[0]),
        sanitize(&lines[1]),
        sanitize(&lines[2])
    )
}

/// Strips the control characters from a line before writing it to the tty.
///
/// Since this plugin composes the display itself, **each** of the three lines
/// comes from network data: the preset name (a remotely editable
/// configuration), a source's status, an ICY title. A stream (or a compromised
/// configuration) slipping `\x1b[...` into one of these fields could
/// manipulate the console. The only control sequences of the rendering are
/// those this module writes itself; the content, for its part, remains data.
fn sanitize(line: &str) -> String {
    line.chars().filter(|c| !c.is_control()).collect()
}

pub struct ConsoleDisplay {
    out: std::fs::File,
    /// Last lines actually written to the tty. The core's channel deduplicates
    /// on the *whole* state (`PlayerState`): a frame that only changes
    /// `preset_count`, `duration_s` or `origin` — invisible to `compose` —
    /// therefore passes that guard and gets here. Without a memory of its own
    /// rendering, this plugin would reprint the same three lines, preceded by
    /// the screen clear: a visible flicker on a tty for a frame it does not
    /// even show.
    last_lines: Option<[String; 3]>,
}

impl ConsoleDisplay {
    pub fn open(path: &Path) -> Result<Self> {
        let out = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self { out, last_lines: None })
    }

    /// Rewrites the screen from the current state, reading the system clock.
    ///
    /// The clock is read **here** and not in `compose_at`, which stays pure:
    /// the rendering's only impure call lives in the only place that already
    /// touches the tty.
    pub fn show(&mut self, state: &PlayerState) -> Result<()> {
        // Read only when it is used: outside standby, `compose_at` does not
        // look at it, and a `tzset` per state frame — one per second during
        // playback — would be one `stat` per second for nothing.
        let now = state.standby.then(local_time).flatten();
        // Same reasoning for the Unix time: only a running timer needs it.
        let unix = state.sleep_timer.and_then(|_| unix_now());
        let lines = compose_full(state, now, unix);
        if self.last_lines.as_ref() == Some(&lines) {
            return Ok(());
        }
        self.out.write_all(render_lines(&lines).as_bytes())?;
        self.out.flush()?;
        self.last_lines = Some(lines);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn radio_state() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 60,
            preset: Some(3),
            preset_count: Some(12),
            preset_name: Some("France Inter".into()),
            ..Default::default()
        }
    }

    #[test]
    fn composes_the_source_the_preset_and_the_total_on_the_first_line() {
        let l = compose(&radio_state());
        assert_eq!(l[0], "■ RADIO  3/12");
        assert_eq!(l[1], "France Inter");
    }

    /// **[MUTATION]** mapping `Paused` to the stopped glyph makes the paused
    /// assertion fire; mapping `Playing` to it makes the playing one fire.
    #[test]
    fn the_first_line_leads_with_one_glyph_per_playback_state() {
        let mut e = radio_state();
        e.playback = Playback::Playing;
        assert_eq!(compose(&e)[0], "▶ RADIO  3/12");
        e.playback = Playback::Paused;
        assert_eq!(compose(&e)[0], "║ RADIO  3/12");
        e.playback = Playback::Stopped;
        assert_eq!(compose(&e)[0], "■ RADIO  3/12");
    }

    #[test]
    fn the_glyphs_reach_the_tty_and_overlay_and_standby_show_none() {
        // `sanitize` strips control characters only: the glyphs go through.
        let mut e = radio_state();
        e.playback = Playback::Playing;
        assert!(render_console(&e).contains("▶ RADIO  3/12\r\n"));
        // The overlay and standby branches are not the normal view.
        e.overlay = Some(Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4000 });
        assert_eq!(compose(&e)[0], "VOLUME 65 %");
        let st = PlayerState {
            standby: true,
            status: Some("VEILLE".into()),
            playback: Playback::Paused,
            ..Default::default()
        };
        assert_eq!(compose(&st)[0], "VEILLE");
        assert_eq!(compose_at(&st, Some((13, 5))), ["VEILLE", "13:05", ""]);
    }

    #[test]
    fn standby_displays_the_time_under_the_standby_word() {
        // Owner's request: the standby screen has nothing else to say, a clock
        // is more useful there than a black tty. The standby word stays on top
        // — it says *why* nothing plays.
        let state = PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        assert_eq!(compose_at(&state, Some((13, 5))), ["VEILLE", "13:05", ""]);
    }

    #[test]
    fn standby_follows_the_twelve_hour_setting() {
        // The setting travels in the state frame: a display never goes looking
        // for anything on the side.
        let mut state =
            PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        state.clock.twelve_hour = true;
        assert_eq!(compose_at(&state, Some((13, 5)))[1], "1:05 PM");
    }

    #[test]
    fn an_unusable_clock_leaves_standby_without_a_time() {
        // A Pi has no battery: before the network has set its time, no time at
        // all is better than a wrong one.
        let state = PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        assert_eq!(compose_at(&state, None), ["VEILLE", "", ""]);
    }

    #[test]
    fn an_overlay_comes_before_the_standby_clock() {
        // The overlay is transient and it is what one wants to read while it
        // lasts — the rule that holds everywhere else in `compose`.
        let state = PlayerState {
            standby: true,
            status: Some("VEILLE".into()),
            overlay: Some(Overlay::Message { text: "PAS DE DISQUE".into(), remaining_ms: 2000 }),
            ..Default::default()
        };
        assert_eq!(compose_at(&state, Some((13, 5))), ["PAS DE DISQUE", "", ""]);
    }

    #[test]
    fn both_time_formats_cover_midnight_and_noon() {
        // The two bounds the Anglo-Saxon convention treats apart: a `0:00 AM`
        // exists nowhere, and noon is `12:00 PM`.
        assert_eq!(format_time(0, 0, false), "00:00");
        assert_eq!(format_time(0, 0, true), "12:00 AM");
        assert_eq!(format_time(12, 0, true), "12:00 PM");
        assert_eq!(format_time(23, 59, true), "11:59 PM");
        assert_eq!(format_time(9, 5, true), "9:05 AM");
        assert_eq!(format_time(9, 5, false), "09:05");
    }

    #[test]
    fn the_local_time_is_readable_and_within_bounds() {
        // The module's only impure call. The time cannot be predicted, but it
        // can be required to be a time: this is what would catch a misread
        // `tm` (a field taken for another, an overflowing timezone).
        let (h, m) = local_time().expect("the test system's clock must be convertible");
        assert!(h < 24, "hour out of bounds: {h}");
        assert!(m < 60, "minutes out of bounds: {m}");
    }

    #[test]
    fn the_first_line_omits_an_unknown_or_zero_total() {
        // Without a declared total, the number alone. And above all: a zero
        // total ("nothing to number", empty tray) is not written — "1/0" would
        // be absurd, and `Some(0)` is a meaningful value of this protocol, not
        // an accident.
        let mut e = radio_state();
        e.preset_count = None;
        assert_eq!(compose(&e)[0], "■ RADIO  3");
        e.preset_count = Some(0);
        assert_eq!(compose(&e)[0], "■ RADIO  3");
    }

    #[test]
    fn a_core_without_any_source_says_the_absence_instead_of_writing_nothing() {
        // The core now starts without a source, waiting for a plugin to
        // announce itself. Empty `source`, and nothing else to write: the whole
        // screen was empty, indistinguishable from a dead display or a lost
        // tty.
        let e = PlayerState::default();
        assert_eq!(compose(&e), ["■ —".to_string(), String::new(), String::new()]);
    }

    #[test]
    fn the_cd_gets_its_track_over_its_total_back() {
        // What the cd plugin composed itself before that project ("CD 1/3"),
        // rendered by the display from the frame's data alone.
        let e = PlayerState {
            source: "cd".into(),
            preset: Some(1),
            preset_count: Some(3),
            ..Default::default()
        };
        assert_eq!(compose(&e)[0], "■ CD  1/3");
    }

    #[test]
    fn the_four_fallbacks_of_the_title_line() {
        // Moved from the core along with the function they test.
        assert_eq!(title_line(Some("Miles Davis"), Some("So What")).as_deref(), Some("Miles Davis — So What"));
        assert_eq!(title_line(None, Some("So What")).as_deref(), Some("So What"));
        // Owner's decision: every available piece of information is displayed,
        // even partial.
        assert_eq!(title_line(Some("Miles Davis"), None).as_deref(), Some("Miles Davis"));
        assert_eq!(title_line(None, None), None);
    }

    #[test]
    fn the_album_takes_precedence_over_the_status_when_both_exist() {
        // What `line2_replaceable` used to negotiate: the plugin decides,
        // without having to ask the core for permission.
        let mut e = PlayerState { source: "cd".into(), preset: Some(1), preset_count: Some(3), ..Default::default() };
        e.status = Some("AUDIO CD".into());
        assert_eq!(compose(&e)[1], "AUDIO CD");
        e.track.album = Some("Kind of Blue".into());
        assert_eq!(compose(&e)[1], "Kind of Blue");
    }

    #[test]
    fn a_preset_name_is_never_supplanted_by_an_album() {
        // The other half of the rule above, and the easiest to break: the cd
        // lets the album win because it does not name its tracks, but a named
        // station must stay displayed even when a `metadata` plugin ends up
        // resolving an album. The core guaranteed this through
        // `line2_replaceable`, which the radio did not declare; here it is the
        // order of the `or_else` that guarantees it, and nothing would flag its
        // inversion.
        let mut e = radio_state();
        e.track.album = Some("Kind of Blue".into());
        assert_eq!(compose(&e)[1], "France Inter");
    }

    #[test]
    fn the_rendering_clears_the_screen_and_spaces_the_three_lines() {
        // Format of the rendering itself, independently of what `compose`
        // decides: ANSI clear header, explicit carriage returns (on /dev/tty1
        // canonical mode does not insert them), and an empty line between
        // each.
        let mut e = radio_state();
        e.track.artist = Some("Miles Davis".into());
        e.track.title = Some("So What".into());
        let s = render_console(&e);
        assert!(s.starts_with("\x1b[2J\x1b[H"));
        assert!(s.contains("■ RADIO  3/12\r\n"));
        assert!(s.contains("France Inter\r\n"));
        assert!(s.contains("Miles Davis — So What\r\n"));
        assert_eq!(s.matches("\r\n\r\n").count(), 2, "an empty line between each of the three");
    }

    #[test]
    fn an_overlay_takes_all_the_room() {
        let mut e = radio_state();
        e.overlay = Some(Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4000 });
        assert_eq!(compose(&e)[0], "VOLUME 65 %");
        assert_eq!(compose(&e)[1], "");
    }

    #[test]
    fn standby_displays_its_word_alone() {
        let e = PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        assert_eq!(compose(&e)[0], "VEILLE");
    }

    #[test]
    fn all_content_is_sanitized_not_only_the_third_line() {
        // Since the plugin composes, **every** piece comes from the network: a
        // remotely configured station name, a status, an ICY title. A stream
        // slipping `\x1b[2J` into one of them could manipulate the console.
        let e = PlayerState {
            source: "radio".into(),
            preset: Some(1),
            preset_name: Some("FI\x1b[2JP".into()),
            ..Default::default()
        };
        let s = render_console(&e);
        assert!(!s.contains("FI\x1b[2JP"));
        assert_eq!(s.matches('\x1b').count(), 2, "only the two ESC of the rendering header");
    }

    #[test]
    fn a_bel_is_also_stripped_not_only_the_esc() {
        // Regression M4 (branch review): the old test also pinned the
        // disappearance of BEL (`\x07`), on top of the ESC count. A `sanitize`
        // mistakenly reduced to ESC filtering alone would pass the previous
        // test without being really safe — `is_control` must cover every
        // control character, not only the one of the rendering itself.
        let e = PlayerState {
            source: "radio".into(),
            preset_name: Some("FI\x07P".into()),
            ..Default::default()
        };
        let s = render_console(&e);
        assert!(!s.contains('\x07'), "BEL must disappear like any other control character");
    }

    #[test]
    fn a_second_frame_with_the_same_lines_does_not_rewrite_the_screen() {
        // Regression M3 (branch review): the core's channel deduplicates on
        // the WHOLE state, not on the composed lines. A frame that only changes
        // `duration_s` — invisible to `compose` — therefore passes the core's
        // guard and gets here: without a memory of its own rendering, the
        // plugin would reprint the same three lines, preceded by the screen
        // clear (visible flicker on a tty).
        //
        // The file is not opened in truncating write mode: a second real write
        // would land after the first (the cursor has advanced) and double the
        // file content, which the equality below would detect.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tty");
        std::fs::write(&path, b"").unwrap();
        let mut display = ConsoleDisplay::open(&path).unwrap();
        let mut e = radio_state();
        display.show(&e).unwrap();
        let after_first = std::fs::read(&path).unwrap();
        assert!(!after_first.is_empty());

        e.track.duration_s = Some(210);
        display.show(&e).unwrap();
        let after_second = std::fs::read(&path).unwrap();
        assert_eq!(
            after_first, after_second,
            "the three composed lines are identical: the second write should not have happened"
        );
    }

    #[test]
    fn a_frame_with_different_lines_does_rewrite_the_screen() {
        // Guard-rail of the test above: the memorization must not prevent a
        // real visible change from being displayed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tty");
        std::fs::write(&path, b"").unwrap();
        let mut display = ConsoleDisplay::open(&path).unwrap();
        let mut e = radio_state();
        display.show(&e).unwrap();
        let after_first = std::fs::read(&path).unwrap();

        e.preset = Some(4);
        display.show(&e).unwrap();
        let after_second = std::fs::read(&path).unwrap();
        assert!(after_second.len() > after_first.len(), "the second write did happen");
    }

    /// Design decision: this display **does not show** the position. Three
    /// lines of about twenty columns already full, and a clock there would
    /// cost one screen clear per second — while the core publishes one frame
    /// per second throughout playback. The field travels here anyway: any
    /// other display plugin may use it.
    #[test]
    fn a_frame_that_only_changes_the_position_composes_the_same_lines() {
        let mut e = radio_state();
        let before = compose(&e);
        e.position_s = Some(87);
        assert_eq!(compose(&e), before);
        e.position_s = Some(88);
        assert_eq!(compose(&e), before);
    }

    /// And the corollary on the overlay: during a transient message, the
    /// per-second frames compose the same single line, so the `last_lines`
    /// guard absorbs them — no flicker while the message is on screen.
    #[test]
    fn an_overlay_survives_the_per_second_frames() {
        let mut e = radio_state();
        e.overlay = Some(Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 5000 });
        e.position_s = Some(87);
        let before = compose(&e);
        e.position_s = Some(88);
        e.overlay = Some(Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 4000 });
        assert_eq!(compose(&e), before);
    }

    // ---- next alarm and sleep timer ----------------------------------

    /// A frame as the core pushes it with an alarm and a timer set.
    ///
    /// `PlayerState` has no serde defaults (the core always writes every
    /// field), so the fragment given here is laid over a default frame
    /// serialized the way the core writes it: what is parsed back is a whole
    /// frame, the new fields travelling as JSON, as on the socket.
    fn frame(json: &str) -> PlayerState {
        let mut base = serde_json::to_value(PlayerState::default()).unwrap();
        let over: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        for (k, v) in over.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).expect("a valid frame")
    }

    const STANDBY_WITH_ALARM: &str = r#"{"standby":true,"status":"VEILLE",
        "next_alarm":{"weekday":"monday","hour":7,"minute":0}}"#;

    const PLAYING_WITH_TIMER: &str = r#"{"source":"radio","preset":3,"preset_count":12,
        "preset_name":"France Inter","playback":"playing",
        "sleep_timer":{"ends_at_unix_s":1000600,"mode":"immediate","phase":"armed"}}"#;

    /// **[MUTATION]** dropping the alarm branch of `compose_full` makes this
    /// fire; so does swapping the weekday table.
    #[test]
    fn standby_shows_the_next_alarm_after_the_clock() {
        let st = frame(STANDBY_WITH_ALARM);
        assert_eq!(compose_at(&st, Some((7, 12))), ["VEILLE", "07:12  A Mon 07:00", ""]);
    }

    #[test]
    fn the_alarm_follows_the_twelve_hour_setting_and_drops_when_it_does_not_fit() {
        let mut st = frame(STANDBY_WITH_ALARM);
        st.clock.twelve_hour = true;
        // "7:12 AM  A Mon 7:00 AM" is 22 columns: too wide, so the clock
        // stays alone rather than being truncated.
        assert_eq!(compose_at(&st, Some((7, 12)))[1], "7:12 AM");
        // The boundary, on the helper itself: exactly 20 columns fits.
        assert_eq!(append_if_fits("12345678".into(), "1234567890"), "12345678  1234567890");
        assert_eq!(append_if_fits("12345678".into(), "12345678901"), "12345678");
    }

    #[test]
    fn no_clock_means_no_alarm_either() {
        let st = frame(STANDBY_WITH_ALARM);
        assert_eq!(compose_at(&st, None), ["VEILLE", "", ""]);
    }

    #[test]
    fn a_standby_frame_without_the_new_fields_renders_as_before() {
        let st = frame(r#"{"standby":true,"status":"VEILLE"}"#);
        assert_eq!(compose_at(&st, Some((13, 5))), ["VEILLE", "13:05", ""]);
        assert_eq!(compose_full(&st, Some((13, 5)), Some(1_000_000)), ["VEILLE", "13:05", ""]);
    }

    /// **[MUTATION]** rounding down instead of up, or ignoring the unix time,
    /// makes one of these fire.
    #[test]
    fn playback_shows_the_remaining_minutes_on_the_first_line() {
        let st = frame(PLAYING_WITH_TIMER);
        let l = compose_full(&st, None, Some(1_000_000));
        assert_eq!(l[0], "▶ RADIO  3/12  Z 10m");
        assert_eq!(l[1], "France Inter");
        // 1 s short of the deadline still reads one minute; 61 s reads two.
        assert_eq!(compose_full(&st, None, Some(1_000_599))[0], "▶ RADIO  3/12  Z 1m");
        assert_eq!(compose_full(&st, None, Some(1_000_539))[0], "▶ RADIO  3/12  Z 2m");
        // Past `ends_at_unix_s` with no newer frame yet: 0m. (Waiting and
        // fading do not read 0m: their frames move `ends_at_unix_s` to the
        // cap or to the end of the fade.)
        assert_eq!(compose_full(&st, None, Some(1_000_700))[0], "▶ RADIO  3/12  Z 0m");
    }

    #[test]
    fn the_timer_is_left_out_when_unknown_overlaid_or_too_wide() {
        let st = frame(PLAYING_WITH_TIMER);
        // No Unix time: no countdown.
        assert_eq!(compose_full(&st, None, None)[0], "▶ RADIO  3/12");
        // An overlay takes everything.
        let mut ov = st.clone();
        ov.overlay = Some(Overlay::Message { text: "OK".into(), remaining_ms: 1000 });
        assert_eq!(compose_full(&ov, None, Some(1_000_000)), ["OK", "", ""]);
        // "▶ RADIO  12/12" + "  Z 120m" is 22 columns: omitted whole.
        let mut wide = st.clone();
        wide.preset = Some(12);
        wide.sleep_timer.as_mut().unwrap().ends_at_unix_s = 1_000_000 + 120 * 60;
        assert_eq!(compose_full(&wide, None, Some(1_000_000))[0], "▶ RADIO  12/12");
    }

    #[test]
    fn a_playing_frame_without_the_new_fields_renders_as_before() {
        let st = frame(r#"{"source":"radio","preset":3,"preset_count":12,"preset_name":"France Inter"}"#);
        assert_eq!(compose_full(&st, None, Some(1_000_000)), compose(&st));
        assert_eq!(compose(&st)[0], "■ RADIO  3/12");
    }

    #[test]
    fn the_countdown_reaches_the_tty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tty");
        std::fs::write(&path, b"").unwrap();
        let mut display = ConsoleDisplay::open(&path).unwrap();
        let mut st = frame(PLAYING_WITH_TIMER);
        st.sleep_timer.as_mut().unwrap().ends_at_unix_s = unix_now().unwrap() + 600;
        display.show(&st).unwrap();
        let out = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
        assert!(out.contains("▶ RADIO  3/12  Z 10m\r\n"), "{out:?}");
    }
}
