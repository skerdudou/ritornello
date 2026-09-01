//! Splitting an ICY string: two pure functions, no network, no state. This is
//! the only place where we decide **how** to cut; deciding *which* split is
//! the right one belongs to validation.

/// Recognized separators, by order of priority — which is also the order in
/// which the candidates are probed.
///
/// `" - "` first: it is the de facto convention of the `StreamTitle` field, the
/// default of most broadcast automation systems. The surrounding spaces are
/// part of the pattern, and that is no detail: without them, `Jean-Michel Jarre`
/// would get cut in two.
pub const SEPARATORS: [&str; 5] = [" - ", " – ", " — ", " / ", " : "];

/// Cap on the candidates probed for one station.
///
/// Each candidate costs one request, spaced by `MIN_INTERVAL`: four make a
/// four-second probe, once per station, that nobody waits for. Without a cap, a
/// string carrying several kinds of separators would produce ten.
pub const MAX_CANDIDATES: usize = 4;

/// A possible split, and what it takes to replay it later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub artist: String,
    pub title: String,
    pub separator: &'static str,
    pub artist_first: bool,
    /// The title is the **middle** field — the `Artiste - Titre - Album` form.
    ///
    /// This flag exists because `Pattern` must be able to **replay** this
    /// candidate. A first version did not carry it, and the defect was an
    /// infinite loop rather than a mere wrong title: the middle candidate
    /// validated, the recorded pattern only retained the separator and the
    /// order, so `apply` glued the album back onto the title on the next track,
    /// validation failed, three failures triggered a reprobe, the same
    /// candidate validated again — and so on forever.
    pub title_in_middle: bool,
}

/// Strips the noise a station tacks onto what it announces.
///
/// **Before** any split, and the order is what matters: a station that tacks
/// on its jingle would make *all* the candidates fail, hence would be
/// classified "do not split" — that is, permanently, since nothing reprobes a
/// station so classified.
///
/// Deliberately **conservative**. Three forms only, those that cannot belong
/// to a title:
///
/// * everything following a vertical bar — it does not appear in a title;
/// * a bracketed group at the end of the string (durations, control-room
///   markers);
/// * leading/trailing spaces and repeated spaces.
///
/// What we do **not** do, and why: stripping a suffix of the kind
/// `" - Radio X"` would be indistinguishable from a real separator, hence would
/// break as many stations as it would repair. And parentheses stay: `(Radio
/// Edit)`, `(Live)`, `(feat. …)` are part of the title, and removing them would
/// prevent validation instead of helping it. A station this cleaning is not
/// enough to handle will end up "do not split", and the admin page is the
/// remedy planned for it.
pub fn clean(raw: &str) -> String {
    let without_bar = raw.split('|').next().unwrap_or(raw);
    let mut s = without_bar.trim();
    // A single group removed, at the end of the string: looping would cut a
    // title that legitimately ended with brackets.
    if let Some(opening) = s.rfind('[') {
        if s.trim_end().ends_with(']') {
            s = s[..opening].trim_end();
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Derives the plausible splits of the **cleaned** string.
///
/// The candidates derive from the string and not from a fixed list: we only
/// build for the separators actually present. A string contains only one kind
/// in practice, hence two candidates — both orders —, and the cap only bites
/// on chatty strings.
///
/// For a separator present at least twice — the `Artiste - Titre - Album`
/// form, a real one — a third candidate takes the **middle** field as title.
/// Without it, the title would carry the album glued on and would never
/// validate.
///
/// An empty half produces no candidate: a request with an empty field is a
/// request for nothing.
pub fn candidates(cleaned: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for separator in SEPARATORS {
        let parts: Vec<&str> = cleaned.split(separator).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }
        let head = parts[0];
        let rest = parts[1..].join(separator);
        let mut push_candidate =
            |artist: &str, title: &str, artist_first: bool, title_in_middle: bool| {
                if artist.is_empty() || title.is_empty() || out.len() >= MAX_CANDIDATES {
                    return;
                }
                out.push(Candidate {
                    artist: artist.to_string(),
                    title: title.to_string(),
                    separator,
                    artist_first,
                    title_in_middle,
                });
            };
        push_candidate(head, &rest, true, false);
        push_candidate(&rest, head, false, false);
        if parts.len() >= 3 {
            push_candidate(head, parts[1], true, true);
        }
    }
    out
}

/// Replays a learned pattern on a cleaned string.
///
/// **No network**: that is the whole point of remembering. Once a station's
/// pattern is known, separating artist and title is a local operation, and
/// only the cover still requires a request.
///
/// `None` when the pattern does not apply: the string does not carry this
/// separator, one half is empty, or the pattern is `DoNotSplit`. This `None`
/// **is** the validation failure the three-consecutive-failures rule speaks
/// of — not an error, a track that does not fit the form.
pub fn apply(pattern: &crate::patterns::Pattern, cleaned: &str) -> Option<(String, String)> {
    let crate::patterns::Pattern::Split { separator, artist_first, title_in_middle } = pattern
    else {
        return None;
    };
    // `title_in_middle`: the `Artiste - Titre - Album` form, where the title is
    // the **second** field and the rest is ignored. Without this branch, the
    // pattern learned from a middle candidate glued the album back onto the
    // title — and as validation then failed on every track, the station got
    // reprobed endlessly. See `Candidate::title_in_middle`.
    if *title_in_middle {
        let parts: Vec<&str> = cleaned.split(separator.as_str()).map(str::trim).collect();
        let (artist, title) = (parts.first()?, parts.get(1)?);
        if artist.is_empty() || title.is_empty() {
            return None;
        }
        return Some((artist.to_string(), title.to_string()));
    }
    let (head, rest) = cleaned.split_once(separator.as_str())?;
    let (head, rest) = (head.trim(), rest.trim());
    if head.is_empty() || rest.is_empty() {
        return None;
    }
    Some(if *artist_first {
        (head.to_string(), rest.to_string())
    } else {
        (rest.to_string(), head.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::Pattern;

    #[test]
    fn cleaning_removes_the_jingle_after_a_bar() {
        assert_eq!(clean("Miles Davis - So What | Radio X"), "Miles Davis - So What");
        assert_eq!(clean("  Miles Davis - So What  "), "Miles Davis - So What");
    }

    #[test]
    fn cleaning_removes_a_bracketed_duration_at_the_end() {
        assert_eq!(clean("Miles Davis - So What [00:09:22]"), "Miles Davis - So What");
    }

    #[test]
    fn cleaning_keeps_a_parenthesis_that_is_part_of_the_title() {
        // `(Radio Edit)`, `(Live)`, `(feat. X)` belong to the title. Removing
        // them would break validation instead of helping it.
        let s = "Daft Punk - Around the World (Radio Edit)";
        assert_eq!(clean(s), s);
    }

    #[test]
    fn cleaning_precedes_splitting_so_the_jingle_does_not_break_the_candidates() {
        // The regression this step exists to prevent: without cleaning, the
        // title of the last candidate carries "| Radio X", no candidate
        // validates, and the station is classified "do not split" — that is,
        // permanently, since nothing reprobes a station so classified.
        let c = candidates(&clean("Miles Davis - So What | Radio X"));
        assert!(
            c.iter().any(|c| c.artist == "Miles Davis" && c.title == "So What"),
            "candidates obtained: {c:?}"
        );
    }

    #[test]
    fn two_candidates_for_a_single_separator_both_orders() {
        let c = candidates("Miles Davis - So What");
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].artist.as_str(), c[0].title.as_str()), ("Miles Davis", "So What"));
        assert!(c[0].artist_first, "the standard comes first");
        assert_eq!((c[1].artist.as_str(), c[1].title.as_str()), ("So What", "Miles Davis"));
        assert!(!c[1].artist_first);
    }

    #[test]
    fn the_en_dash_is_recognized_as_a_separator() {
        let c = candidates("Miles Davis – So What");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].separator, " – ");
    }

    #[test]
    fn three_fields_also_give_the_middle_candidate() {
        // The `Artiste - Titre - Album` form, a real one. Without this
        // candidate, the title would carry "So What - Kind of Blue" and would
        // never validate.
        let c = candidates("Miles Davis - So What - Kind of Blue");
        assert!(
            c.iter().any(|c| c.artist == "Miles Davis" && c.title == "So What"),
            "candidates obtained: {c:?}"
        );
    }

    #[test]
    fn the_candidate_cap_holds() {
        // Several kinds of separators in the same string: the cap must bite,
        // otherwise a probe turns into ten requests.
        let c = candidates("A - B / C : D – E");
        assert!(c.len() <= MAX_CANDIDATES, "{} candidates", c.len());
    }

    #[test]
    fn without_a_separator_there_is_no_candidate() {
        // A slogan, a show name: nothing to constrain on the artist side,
        // hence nothing to validate. The caller concludes "do not split".
        assert!(candidates("Vous ecoutez Radio X").is_empty());
        assert!(candidates("").is_empty());
    }

    #[test]
    fn an_empty_half_produces_no_candidate() {
        // A request with an empty field is a request for nothing.
        //
        // **The edge spaces are the essence of these two fixtures**, and a
        // first version had forgotten them (`"- So What"`, `"Miles Davis -"`):
        // the separator being `" - "`, those strings contained none, the guard
        // was never reached, and the test passed for a wrong reason — removing
        // the guard did not make it fail. Found by mutation testing, which is
        // made for that.
        //
        // These two forms do **not** come out of `clean`, which trims the
        // edges: this test therefore exercises the contract of `candidates`,
        // which is a public function and must not depend on who calls it, and
        // not a production path. The distinction is worth knowing before
        // touching it.
        assert!(candidates(" - So What").is_empty(), "empty artist");
        assert!(candidates("Miles Davis - ").is_empty(), "empty title");
    }

    #[test]
    fn applying_a_pattern_gives_back_the_pair() {
        let m =
            Pattern::Split { separator: " - ".into(), artist_first: false, title_in_middle: false };
        assert_eq!(
            apply(&m, "So What - Miles Davis"),
            Some(("Miles Davis".to_string(), "So What".to_string())),
            "reverse order: the artist comes second"
        );
    }

    /// **The property that ties the two halves of the work together**, and
    /// that nothing proved: replaying a candidate's pattern on the string it
    /// came from must give back that candidate, identically.
    ///
    /// Its interest is not theoretical. The probe retains the candidate that
    /// MusicBrainz validated, then all the following tracks are split by
    /// `apply` without any further request. If the two functions diverged on
    /// any form, the device would show a wrong artist and title **after a
    /// successful probe** — the worst combination, since validation did take
    /// place and nothing in the log would report it.
    ///
    /// Exercised on all the forms the other tests handle one by one, **plus**
    /// those that combine them: several separators in the same string, three
    /// fields, rare separators, and a hyphenated name.
    #[test]
    fn applying_a_candidates_pattern_gives_back_that_candidate() {
        let forms = [
            "Miles Davis - So What",
            "So What - Miles Davis",
            "Miles Davis – So What",
            "Miles Davis — So What",
            "Miles Davis / So What",
            "Miles Davis : So What",
            "Miles Davis - So What - Kind of Blue",
            "A - B / C : D – E",
            "Daft Punk - Around the World (Radio Edit)",
            "Jean-Michel Jarre - Oxygene Pt. 4",
        ];
        for form in forms {
            let cleaned = clean(form);
            let cands = candidates(&cleaned);
            assert!(!cands.is_empty(), "\"{form}\" must produce at least one candidate");
            for c in cands {
                let pattern = crate::patterns::Pattern::from_candidate(&c);
                assert_eq!(
                    apply(&pattern, &cleaned),
                    Some((c.artist.clone(), c.title.clone())),
                    "pattern {pattern:?} replayed on \"{cleaned}\" must give back {c:?}"
                );
            }
        }
    }

    #[test]
    fn applying_a_pattern_absent_from_the_string_returns_none() {
        // The track where the station changes form: not a lopsided pair,
        // nothing at all. This `None` is what counts as a validation failure.
        let m =
            Pattern::Split { separator: " - ".into(), artist_first: true, title_in_middle: false };
        assert_eq!(apply(&m, "Vous ecoutez Radio X"), None);
    }

    #[test]
    fn do_not_split_never_produces_a_pair() {
        assert_eq!(apply(&Pattern::DoNotSplit, "Miles Davis - So What"), None);
    }
}
