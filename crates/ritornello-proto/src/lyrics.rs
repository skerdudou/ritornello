//! Lyrics of a track: the types every component shares, and the **only** LRC
//! parser.
//!
//! One parser, here, because several components read LRC (the files plugin
//! reads `.lrc` next to a track, the LRCLIB plugin receives it over HTTP, the
//! core turns it into lines for the page) and two parsers would disagree on a
//! real-world file sooner or later. Everything below is deliberately lenient:
//! a lyrics file is written by hand and abused by every editor that ever
//! touched it, and showing the text imperfectly beats showing nothing.

use serde::{Deserialize, Serialize};

/// Ceiling on a set of lyrics: `plain` and `synced` together.
///
/// A song is a few kilobytes; anything near this is not lyrics (a book, a
/// binary read as text) and would ride every state frame and every cache
/// entry for nothing.
pub const LYRICS_MAX_BYTES: usize = 64 * 1024;

/// The lyrics a source or a plugin found, as raw text.
///
/// Both forms may be present: `synced` is LRC (timestamped), `plain` is the
/// same words without times. A consumer that cannot use `synced` falls back on
/// `plain`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced: Option<String>,
}

impl Lyrics {
    /// The one gate lyrics pass through before being kept or shown: trimmed,
    /// non-empty, under the ceiling, and with `synced` only when it really is
    /// LRC. `None` means "there are no lyrics here".
    ///
    /// A `synced` text that does not parse is not thrown away: a `.lrc` with
    /// no timestamps is plain text under another name, so it moves to `plain`
    /// (unless a `plain` is already there, which is then the better source).
    pub fn validated(self) -> Option<Self> {
        let keep = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let mut plain = keep(self.plain);
        let mut synced = keep(self.synced);
        let size = plain.as_ref().map_or(0, String::len) + synced.as_ref().map_or(0, String::len);
        if size > LYRICS_MAX_BYTES {
            return None;
        }
        if synced.as_deref().is_some_and(|text| parse_lrc(text).is_none()) {
            let text = synced.take();
            if plain.is_none() {
                plain = text;
            }
        }
        if plain.is_none() && synced.is_none() {
            return None;
        }
        Some(Self { plain, synced })
    }

    /// Whether there are timestamped lines (meaningful on validated lyrics).
    pub fn is_synced(&self) -> bool {
        self.synced.is_some()
    }

    /// The text to show or measure when time does not matter: the plain
    /// words, else the LRC itself, else nothing.
    pub fn best_text(&self) -> &str {
        self.plain.as_deref().or(self.synced.as_deref()).unwrap_or("")
    }
}

/// Most lines `parse_lrc` will produce. A real song has a few hundred; the
/// cap exists because one input line may carry thousands of timestamps.
pub const MAX_LYRIC_LINES: usize = 10_000;

/// Most bytes of line text `parse_lrc` will produce, in total. Several stamps
/// ahead of one text clone that text once each, so a small input could expand
/// into tens of megabytes; a chorus repeated by multi-stamps stays far below
/// this. Past either cap the input is treated as unparseable, not truncated.
pub const MAX_LYRIC_TEXT_BYTES: usize = 4 * LYRICS_MAX_BYTES;

/// One timed line. An empty `text` is a pause, kept so the highlight leaves
/// the previous line when the singer stops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    pub t_ms: u32,
    pub text: String,
}

/// Which song a set of lyrics was found for: what is compared with the current
/// track so lyrics that arrive late are not shown under the next song.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricsSubject {
    pub artist: String,
    pub title: String,
}

impl LyricsSubject {
    /// Same song, as far as names can tell: equal after normalization. Never
    /// true when the track lacks an artist or a title, since two unknowns
    /// are not the same song.
    pub fn matches(&self, artist: Option<&str>, title: Option<&str>) -> bool {
        match (artist, title) {
            (Some(artist), Some(title)) => {
                let same = |ours: &str, theirs: &str| {
                    let ours = normalize_for_match(ours);
                    // Two names that fold to nothing ("!!!" and "???") are
                    // not the same name, only two names with no letters.
                    !ours.is_empty() && ours == normalize_for_match(theirs)
                };
                same(&self.artist, artist) && same(&self.title, title)
            }
            _ => false,
        }
    }
}

/// Folds a name so that "Beyoncé", "BEYONCE" and "Halo (Live)" / "Halo" meet.
///
/// Lowercase, accents folded, a trailing parenthesised or bracketed suffix
/// ("(Live)", "[Radio Edit]") and a featuring ("feat. X", "ft. X") removed,
/// punctuation reduced to single spaces.
pub fn normalize_for_match(s: &str) -> String {
    // Only a *trailing* group is a suffix, and only when something precedes
    // it: "Song (Part 1) Name" keeps its middle, and "(What's the Story)
    // Morning Glory?" does not lose its head.
    let mut cut = s.trim_end();
    loop {
        let open = match cut.chars().next_back() {
            Some(')') => '(',
            Some(']') => '[',
            _ => break,
        };
        match cut.rfind(open) {
            Some(i) if !cut[..i].trim().is_empty() => cut = cut[..i].trim_end(),
            _ => break,
        }
    }
    let folded: String = cut.chars().flat_map(char::to_lowercase).map(fold_accent).collect();
    let mut words: Vec<&str> = Vec::new();
    for word in folded.split_whitespace() {
        let bare = word.trim_end_matches('.');
        // A featuring only counts after the first word: a band called "Ft"
        // is still a band.
        if !words.is_empty() && matches!(bare, "feat" | "ft" | "featuring") {
            break;
        }
        words.push(word);
    }
    let spaced: String =
        words.join(" ").chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Folds the common Latin accents (lowercase input). A table rather than a
/// Unicode normalization crate, as in the files companion's `roots.rs`:
/// everything else keeps its own letter, which still compares equal to itself.
fn fold_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

/// Reads `mm:ss`, `mm:ss.x`, `mm:ss.xx` or `mm:ss.xxx` into milliseconds.
fn parse_timestamp(s: &str) -> Option<u32> {
    let (min, rest) = s.split_once(':')?;
    let (sec, frac) = match rest.split_once('.') {
        Some((sec, frac)) => (sec, Some(frac)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(min) || !digits(sec) || sec.len() > 2 {
        return None;
    }
    let ms = match frac {
        None => 0,
        Some(f) if digits(f) && f.len() <= 3 => {
            // "5" is half a second, "05" a twentieth, "005" five thousandths.
            f.parse::<u32>().ok()? * 10u32.pow(3 - f.len() as u32)
        }
        Some(_) => return None,
    };
    min.parse::<u32>().ok()?.checked_mul(60_000)?.checked_add(sec.parse::<u32>().ok()? * 1000 + ms)
}

/// Removes enhanced-LRC word tags (`<mm:ss.xx>`) and leaves any other `<`
/// alone: lyrics do contain angle brackets.
fn strip_word_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('>') {
            Some(close) if parse_timestamp(&after[..close]).is_some() => rest = &after[close + 1..],
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parses LRC into timed lines, sorted by time. `None` when no line carries a
/// timestamp: headers alone, or plain text, are not synced lyrics.
///
/// Tolerates a BOM and CRLF; several timestamps ahead of one text (one line
/// per timestamp); the headers `[ar:]`, `[ti:]`, `[al:]`, `[by:]`,
/// `[length:]`, which are ignored; and `[offset:±N]` in milliseconds, applied
/// to every line wherever in the file it stands. By the LRC convention a
/// positive offset shows the lyrics **earlier** (`t - N`, saturating at 0) and
/// a negative one later.
pub fn parse_lrc(text: &str) -> Option<Vec<LyricLine>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines: Vec<LyricLine> = Vec::new();
    let mut offset_ms: i64 = 0;
    let mut text_bytes = 0usize;
    for raw in text.lines() {
        let mut rest = raw.trim_start();
        let mut stamps: Vec<u32> = Vec::new();
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(close) = inner.find(']') else { break };
            let tag = &inner[..close];
            if let Some(t) = parse_timestamp(tag) {
                stamps.push(t);
            } else if let Some(value) = tag.strip_prefix("offset:") {
                if let Ok(n) = value.trim().parse::<i64>() {
                    offset_ms = n;
                }
            } else if !tag.contains(':') {
                // Not a tag at all: the text starts here.
                break;
            }
            // Any other `key:value` is a header; it is skipped, not read.
            rest = &inner[close + 1..];
        }
        if stamps.is_empty() {
            continue;
        }
        let text = strip_word_tags(rest).trim().to_string();
        for t_ms in stamps {
            text_bytes += text.len();
            if lines.len() >= MAX_LYRIC_LINES || text_bytes > MAX_LYRIC_TEXT_BYTES {
                return None;
            }
            lines.push(LyricLine { t_ms, text: text.clone() });
        }
    }
    if lines.is_empty() {
        return None;
    }
    if offset_ms != 0 {
        for l in &mut lines {
            l.t_ms = (i64::from(l.t_ms) - offset_ms).clamp(0, i64::from(u32::MAX)) as u32;
        }
    }
    // `sort_by_key` is stable: two lines at the same instant keep file order.
    lines.sort_by_key(|l| l.t_ms);
    Some(lines)
}

/// What a consumer shows: the parsed lines when `synced` parses, else the
/// plain text. Both keys are always serialized (as `null` when empty): the
/// page reads `synced: [...] | null` and `plain: string | null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedLyrics {
    pub synced: Option<Vec<LyricLine>>,
    pub plain: Option<String>,
}

impl RenderedLyrics {
    pub fn from_lyrics(l: &Lyrics) -> Self {
        if let Some(lines) = l.synced.as_deref().and_then(parse_lrc) {
            return Self { synced: Some(lines), plain: None };
        }
        // Unparseable synced text is still words worth showing.
        Self { synced: None, plain: l.plain.clone().or_else(|| l.synced.clone()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t_ms: u32, text: &str) -> LyricLine {
        LyricLine { t_ms, text: text.into() }
    }

    /// A real LRCLIB answer (`/api/get`, "Get Lucky"), not a hand-written LRC.
    fn captured_synced() -> String {
        let capture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/lrclib-get-synced.json")).unwrap();
        capture["syncedLyrics"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_real_lrclib_lrc_parses_into_timed_lines() {
        let lines = parse_lrc(&captured_synced()).unwrap();
        assert_eq!(lines[0], line(31_480, "Like the legend of the phoenix, huh"));
        assert!(lines.len() > 20, "{} lines", lines.len());
        assert!(lines.windows(2).all(|w| w[0].t_ms <= w[1].t_ms), "lines must be sorted");
    }

    #[test]
    fn crlf_and_a_bom_do_not_change_the_lines() {
        let plain = "[00:01.00]a\n[00:02.50]b\n";
        let mangled = "\u{feff}[00:01.00]a\r\n[00:02.50]b\r\n";
        assert_eq!(parse_lrc(mangled), parse_lrc(plain));
        assert_eq!(parse_lrc(plain).unwrap(), vec![line(1000, "a"), line(2500, "b")]);
    }

    #[test]
    fn several_timestamps_on_one_line_yield_one_line_each() {
        // Written out of order on purpose: the output must be sorted.
        assert_eq!(
            parse_lrc("[00:05.00][00:01.00]x").unwrap(),
            vec![line(1000, "x"), line(5000, "x")]
        );
    }

    #[test]
    fn timestamps_accept_one_to_three_fraction_digits_or_none() {
        let lines = parse_lrc("[00:01]a\n[00:02.5]b\n[00:03.25]c\n[01:04.125]d").unwrap();
        let times: Vec<u32> = lines.iter().map(|l| l.t_ms).collect();
        assert_eq!(times, vec![1000, 2500, 3250, 64_125]);
    }

    #[test]
    fn the_offset_tag_shifts_every_line_and_never_below_zero() {
        // LRC convention: a positive offset shows the lyrics earlier.
        let shifted = parse_lrc("[offset:+500]\n[00:01.00]a\n[00:02.00]b").unwrap();
        assert_eq!(shifted, vec![line(500, "a"), line(1500, "b")]);
        // The offset may also come after the lines it applies to.
        let late = parse_lrc("[00:01.00]a\n[offset:+500]").unwrap();
        assert_eq!(late, vec![line(500, "a")]);
        let clamped = parse_lrc("[offset:+2000]\n[00:01.00]a").unwrap();
        assert_eq!(clamped, vec![line(0, "a")]);
        let delayed = parse_lrc("[offset:-500]\n[00:01.00]a").unwrap();
        assert_eq!(delayed, vec![line(1500, "a")]);
    }

    #[test]
    fn an_amplifying_lrc_is_refused_rather_than_expanded() {
        // 3000 stamps before a 1 KiB text: 3 MB of output from 33 KB of input.
        let stamps: String = (0..3000).map(|i| format!("[{:02}:{:02}.00]", i / 60, i % 60)).collect();
        assert_eq!(parse_lrc(&format!("{stamps}{}", "x".repeat(1024))), None);
        // Many empty lines: the line cap alone refuses it.
        let many = "[00:01.00]".repeat(MAX_LYRIC_LINES + 1);
        assert_eq!(parse_lrc(&many), None);
        assert_eq!(parse_lrc(&"[00:01.00]".repeat(MAX_LYRIC_LINES)).map(|l| l.len()), Some(MAX_LYRIC_LINES));
        // A normal chorus repeated by multi-stamps still parses.
        let chorus = parse_lrc("[00:10.00][00:50.00][01:30.00][02:10.00]La la la\n[00:20.00]verse").unwrap();
        assert_eq!(chorus.len(), 5);
        assert_eq!(chorus[1], line(20_000, "verse"));
    }

    #[test]
    fn names_that_normalize_to_nothing_never_match() {
        let punctuation = LyricsSubject { artist: "!!!".into(), title: "???".into() };
        assert!(!punctuation.matches(Some("..."), Some("---")));
        assert!(!punctuation.matches(Some("!!!"), Some("???")));
        let empty = LyricsSubject { artist: "".into(), title: "x".into() };
        assert!(!empty.matches(Some(""), Some("x")));
    }

    #[test]
    fn only_a_trailing_bracketed_suffix_is_stripped() {
        assert_eq!(normalize_for_match("Song (Part 1) Name"), "song part 1 name");
        assert_eq!(normalize_for_match("Song [Remastered 2011]"), "song");
        assert_eq!(normalize_for_match("Song (Live) [Remastered]"), "song");
        assert_eq!(normalize_for_match("Song (Live)"), "song");
        assert_eq!(normalize_for_match("(Intro)"), "intro");
    }

    #[test]
    fn word_tags_are_removed_from_the_text() {
        let lines = parse_lrc("[00:01.00]<00:01.00>Hello <00:01.50>world").unwrap();
        assert_eq!(lines, vec![line(1000, "Hello world")]);
    }

    #[test]
    fn headers_alone_are_not_synced_lyrics() {
        assert_eq!(parse_lrc("[ar:X]\n[ti:Y]\nplain line"), None);
    }

    #[test]
    fn headers_are_ignored_and_empty_timed_lines_are_kept_as_pauses() {
        let lrc = "[ar:A]\n[ti:T]\n[al:Al]\n[by:me]\n[length:03:30]\n[00:01.00]a\n[00:02.00]\n[00:03.00]b";
        assert_eq!(
            parse_lrc(lrc).unwrap(),
            vec![line(1000, "a"), line(2000, ""), line(3000, "b")]
        );
    }

    #[test]
    fn sorting_is_stable_for_equal_timestamps() {
        let lines = parse_lrc("[00:01.00]first\n[00:01.00]second\n[00:00.50]zero").unwrap();
        assert_eq!(lines, vec![line(500, "zero"), line(1000, "first"), line(1000, "second")]);
    }

    #[test]
    fn validated_moves_unparseable_synced_text_to_plain() {
        let l = Lyrics { plain: None, synced: Some("just words\nno stamps".into()) }.validated().unwrap();
        assert_eq!(l.synced, None);
        assert_eq!(l.plain.as_deref(), Some("just words\nno stamps"));
        assert!(!l.is_synced());
        // A plain text already there wins; the unparseable synced text is dropped.
        let kept = Lyrics { plain: Some("real".into()), synced: Some("junk".into()) }.validated().unwrap();
        assert_eq!(kept, Lyrics { plain: Some("real".into()), synced: None });
    }

    #[test]
    fn validated_keeps_parseable_synced_text() {
        let l = Lyrics { plain: Some("a".into()), synced: Some("[00:01.00]a".into()) }.validated().unwrap();
        assert!(l.is_synced());
        assert_eq!(l.best_text(), "a");
    }

    #[test]
    fn validated_refuses_blank_and_oversized_lyrics() {
        assert_eq!(Lyrics::default().validated(), None);
        assert_eq!(Lyrics { plain: Some("  \n ".into()), synced: Some("".into()) }.validated(), None);
        let exact = Lyrics { plain: Some("a".repeat(LYRICS_MAX_BYTES)), synced: None };
        assert!(exact.validated().is_some());
        let over = Lyrics { plain: Some("a".repeat(LYRICS_MAX_BYTES + 1)), synced: None };
        assert_eq!(over.validated(), None);
        // The cap is on the sum of the two texts, not on each.
        let split = Lyrics {
            plain: Some("a".repeat(LYRICS_MAX_BYTES / 2)),
            synced: Some(format!("[00:01.00]{}", "a".repeat(LYRICS_MAX_BYTES / 2))),
        };
        assert_eq!(split.validated(), None);
    }

    #[test]
    fn the_subject_matches_across_case_accents_and_featurings() {
        let subject = LyricsSubject { artist: "Beyoncé".into(), title: "Halo".into() };
        assert!(subject.matches(Some("beyonce"), Some("HALO (Live)")));
        assert!(!subject.matches(Some("beyonce"), Some("Crazy in Love")));
        assert!(!subject.matches(None, Some("Halo")));
        assert!(!subject.matches(Some("Beyoncé"), None));

        let daft = LyricsSubject { artist: "Daft Punk".into(), title: "Get Lucky".into() };
        assert!(daft.matches(Some("Daft Punk feat. Pharrell Williams"), Some("Get Lucky")));
        assert!(daft.matches(Some("Daft Punk ft. Pharrell"), Some("Get Lucky [Radio Edit]")));
        assert!(!daft.matches(Some("Pharrell Williams"), Some("Get Lucky")));
    }

    #[test]
    fn normalization_keeps_a_title_that_starts_with_a_bracket() {
        assert_eq!(normalize_for_match("(What's the Story) Morning Glory?"), "what s the story morning glory");
        assert_eq!(normalize_for_match("  AC/DC  "), "ac dc");
    }

    #[test]
    fn rendered_lyrics_prefer_the_parsed_lines() {
        let both = Lyrics { plain: Some("a".into()), synced: Some("[00:01.00]a".into()) };
        let r = RenderedLyrics::from_lyrics(&both);
        assert_eq!(r.synced, Some(vec![line(1000, "a")]));
        assert_eq!(r.plain, None);
        // Unparseable synced text falls back to plain text, whichever field held it.
        let broken = Lyrics { plain: None, synced: Some("words".into()) };
        let r = RenderedLyrics::from_lyrics(&broken);
        assert_eq!((r.synced, r.plain.as_deref()), (None, Some("words")));
        // Both keys are always on the wire: the page reads `null`, not absence.
        let json = serde_json::to_value(RenderedLyrics::from_lyrics(&both)).unwrap();
        assert!(json.get("plain").is_some_and(|v| v.is_null()), "{json}");
        let json = serde_json::to_value(RenderedLyrics::from_lyrics(&broken)).unwrap();
        assert!(json.get("synced").is_some_and(|v| v.is_null()), "{json}");
    }
}
