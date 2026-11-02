//! Which LRCLIB record, if any, belongs to the playing track.
//!
//! Measured on 2026-11-02: a search for "Get Lucky" returns 20 records with
//! the same artist and title, from 193 s to 370 s. The names cannot tell the
//! radio edit from the album version, only the duration can — and lyrics of
//! the wrong version drift off the music within a verse.

use ritornello_proto::{normalize_for_match, LyricsSubject};

use crate::client::{Answer, Query, Record};

/// How far a record's duration may be from the track's. LRCLIB itself
/// tolerates about ±2 s on `/api/get` (369 asked, 367 answered); 3 s leaves
/// room for a file whose header rounds differently, and stays far below the
/// gap between two versions of a song.
pub const DURATION_TOLERANCE_S: f64 = 3.0;

/// The record for `q`, or `None` when nothing fits.
///
/// Kept: records whose artist and title equal the track's after the shared
/// normalization, that carry lyrics or say the song is instrumental. With a
/// known duration, only those within [`DURATION_TOLERANCE_S`], nearest
/// first (a record without a duration cannot be checked, so it is out).
/// Ties, and every choice without a duration, go to synced lyrics, then to
/// the same album; past that, LRCLIB's own order.
pub fn choose<'a>(q: &Query, records: &'a [Record]) -> Option<&'a Record> {
    let subject = LyricsSubject { artist: q.artist.clone(), title: q.title.clone() };
    let album = q.album.as_deref().map(normalize_for_match).filter(|a| !a.is_empty());
    // Higher is better: (synced lyrics, same album).
    let rank = |r: &Record| {
        let synced = r.lyrics().is_some_and(|l| l.is_synced());
        let same_album = album.is_some() && r.album.as_deref().map(normalize_for_match) == album;
        (synced, same_album)
    };
    let candidates = records
        .iter()
        .filter(|r| subject.matches(Some(&r.artist), Some(&r.title)))
        .filter(|r| r.instrumental || r.lyrics().is_some());
    // `min_by` keeps the first of equals, which is LRCLIB's order.
    match q.duration_s {
        Some(wanted) => candidates
            .filter_map(|r| {
                let gap = (r.duration_s? - f64::from(wanted)).abs();
                (gap <= DURATION_TOLERANCE_S).then_some((gap, r))
            })
            .min_by(|(gap_a, a), (gap_b, b)| gap_a.total_cmp(gap_b).then_with(|| rank(b).cmp(&rank(a))))
            .map(|(_, r)| r),
        None => candidates.min_by(|a, b| rank(b).cmp(&rank(a))),
    }
}

/// The answer a set of records gives to `q`.
pub fn answer_for(q: &Query, records: &[Record]) -> Answer {
    match choose(q, records) {
        None => Answer::Absent,
        Some(r) if r.instrumental => Answer::Instrumental,
        Some(r) => match r.lyrics() {
            Some(lyrics) => Answer::Found {
                lyrics,
                duration_s: r.duration_s.filter(|d| d.is_finite() && *d >= 0.0).map(|d| d.round() as u32),
            },
            // Unreachable through `choose`, which keeps only records with
            // lyrics or instrumental; said rather than unwrapped.
            None => Answer::Absent,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::parse_search;

    const SEARCH: &str = include_str!("../tests/fixtures/search.json");
    const SEARCH_INSTRUMENTAL: &str = include_str!("../tests/fixtures/search-instrumental.json");

    fn search() -> Vec<Record> {
        parse_search(SEARCH).expect("the captured search parses")
    }

    fn query(artist: &str, title: &str, album: Option<&str>, duration_s: Option<u32>) -> Query {
        Query {
            artist: artist.into(),
            title: title.into(),
            album: album.map(Into::into),
            duration_s,
        }
    }

    #[test]
    fn the_record_nearest_the_known_duration_wins() {
        // Review focus 3: the file lasts 254 s, LRCLIB offers twenty
        // versions with no album to tell them apart. Never the 370 s one.
        let records = search();
        let chosen = choose(&query("Daft Punk", "Get Lucky", None, Some(254)), &records)
            .expect("the 254 s version is in the capture");
        assert_eq!(chosen.duration_s, Some(254.0));
        assert_eq!(chosen.album.as_deref(), Some("Random Access Memories"));
        // Nearest, not merely within tolerance: at 251 s, 248 s and 254 s
        // are 3 s off but 249 s only 2 s.
        let near = choose(&query("Daft Punk", "Get Lucky", None, Some(251)), &records).unwrap();
        assert_eq!(near.duration_s, Some(249.0));
        let edit = choose(&query("Daft Punk", "Get Lucky", None, Some(242)), &records).unwrap();
        assert_eq!(edit.duration_s, Some(242.0), "241 s and 242 s are both in tolerance; 242 s is exact");
    }

    #[test]
    fn nothing_within_three_seconds_is_nothing() {
        // 300 s: the nearest record (254 s) is 46 s off — another version,
        // whose lyrics would run ahead of the music.
        let records = search();
        assert_eq!(choose(&query("Daft Punk", "Get Lucky", None, Some(300)), &records), None);
        // The edge: 3 s is in, just past it is out.
        assert!(choose(&query("Daft Punk", "Get Lucky", None, Some(373)), &records).is_some());
        assert_eq!(choose(&query("Daft Punk", "Get Lucky", None, Some(374)), &records), None);
        assert_eq!(answer_for(&query("Daft Punk", "Get Lucky", None, Some(300)), &records), Answer::Absent);
    }

    #[test]
    fn another_artist_with_the_same_title_is_never_taken() {
        let records = search();
        assert_eq!(choose(&query("Pharrell Williams", "Get Lucky", None, Some(254)), &records), None);
        assert_eq!(choose(&query("Pharrell Williams", "Get Lucky", None, None), &records), None);
        // The comparison is the shared normalization, not byte equality.
        assert!(choose(&query("DAFT PUNK", "Get Lucky (Radio Edit)", None, Some(254)), &records).is_some());
    }

    #[test]
    fn another_title_by_the_same_artist_is_never_taken() {
        let records = search();
        assert_eq!(choose(&query("Daft Punk", "Lose Yourself to Dance", None, Some(254)), &records), None);
    }

    #[test]
    fn without_a_duration_a_synced_record_is_preferred() {
        let mut records = search();
        for r in &mut records[..3] {
            r.synced = None;
        }
        let chosen = choose(&query("Daft Punk", "Get Lucky", None, None), &records).unwrap();
        assert!(std::ptr::eq(chosen, &records[3]), "the first synced record, got {chosen:?}");
    }

    #[test]
    fn without_a_duration_the_same_album_breaks_the_tie() {
        let records = search();
        let chosen =
            choose(&query("Daft Punk", "Get Lucky", Some("Face to Face"), None), &records).unwrap();
        assert_eq!(chosen.album.as_deref(), Some("Face to Face"));
    }

    #[test]
    fn at_equal_distance_the_same_album_wins() {
        // Six records last 370 s; the first one's album is "Random Access
        // Memory (flac)". Asked for the album, the first record whose album
        // normalizes to it wins ("[2023 10th anv] [HD]" is a suffix).
        let records = search();
        let chosen = choose(
            &query("Daft Punk", "Get Lucky", Some("Random Access Memories"), Some(370)),
            &records,
        )
        .unwrap();
        assert_eq!(chosen.album.as_deref(), Some("Random Access Memories [2023 10th anv] [HD]"));
        let anonymous = choose(&query("Daft Punk", "Get Lucky", None, Some(370)), &records).unwrap();
        assert_eq!(anonymous.album.as_deref(), Some("Random Access Memory (flac)"));
    }

    #[test]
    fn a_record_without_lyrics_is_skipped() {
        let mut records = search();
        let exact = records.iter_mut().find(|r| r.duration_s == Some(248.0)).unwrap();
        exact.plain = None;
        exact.synced = Some("   ".into());
        let chosen = choose(&query("Daft Punk", "Get Lucky", None, Some(248)), &records).unwrap();
        assert_eq!(chosen.duration_s, Some(247.0), "the next nearest, 1 s off");
    }

    #[test]
    fn an_instrumental_match_is_reported_as_such() {
        let records = parse_search(SEARCH_INSTRUMENTAL).expect("the captured search parses");
        let q = query("Aphex Twin", "Avril 14th", None, Some(125));
        assert!(choose(&q, &records).is_some_and(|r| r.instrumental));
        assert_eq!(answer_for(&q, &records), Answer::Instrumental);
    }

    #[test]
    fn a_found_answer_carries_the_lyrics_and_the_rounded_duration() {
        let records = search();
        match answer_for(&query("Daft Punk", "Get Lucky", None, Some(254)), &records) {
            Answer::Found { lyrics, duration_s } => {
                assert_eq!(duration_s, Some(254));
                assert!(lyrics.is_synced() && lyrics.plain.is_some());
            }
            other => panic!("expected lyrics, got {other:?}"),
        }
    }
}
