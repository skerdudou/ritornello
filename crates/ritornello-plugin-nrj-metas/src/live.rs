//! Querying one NRJ group station, and reading its answer.
//!
//! Parsing is a pure function, tested on real responses; only `query` touches
//! the network, and **no test calls it**.
//!
//! Unlike Radio France, which tells us itself when to call back, NRJ answers a
//! snapshot and an `end_timestamp` it is **measurably late on** — 40 to 70
//! seconds past its own deadline. The rhythm is therefore driven by the ICY
//! cart code the core already hands us, not by that deadline; see `main`.

use anyhow::{bail, Result};
use serde_json::Value;
use std::time::Duration;

/// What one response tells us about a station.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub artist: Option<String>,
    pub title: Option<String>,
    /// Detached from the end of the title when it carries one. Measured on
    /// Nostalgie, which glues it there.
    pub year: Option<u16>,
    /// Full-size cover, `/600x/`. `None` when the announced image is a station
    /// wallpaper — see `is_wallpaper`.
    pub cover: Option<String>,
    /// The same image at `/200x/`, offered so the appliance does not re-encode
    /// what is already the right size. Only an indication: measured on one
    /// image, so the full size stays the one that counts.
    pub cover_thumb: Option<String>,
    /// End of the current item, in seconds since the Unix epoch, as the server
    /// announces it. Raw: it is `main` that turns it into a deadline, so this
    /// module stays clock-free and testable on captures.
    pub ends_at: Option<f64>,
}

/// Initial wait before retrying after a failure, then doubled.
pub const BACKOFF_BASE: Duration = Duration::from_secs(2);

/// Backoff cap. A device that runs unattended for months must not hammer a
/// third party's server.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Metadata URL of one station.
///
/// One station and not the whole brand: measured, `/onair.json` weighs 53 KB
/// to 400 KB depending on the brand, this endpoint 1.3 KB for the same answer.
pub fn station_url(brand: &str, id: u32) -> String {
    format!("https://www.{brand}.fr/api/webradios/get-by-ids?ids[]={id}")
}

/// Non-empty text of a field, `None` otherwise.
fn text(v: &Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Comparison form for "is the station naming itself": lowercase, `&` read as
/// `et`, everything else non-alphanumeric removed.
///
/// The normalization is not cosmetic. Measured, the station `NRJ HITS MIXES`
/// announces itself as `NRJ HITSMIXES` — a raw comparison would have missed
/// it, and the screen would have shown the station's own name as an artist.
///
/// **Accented letters are kept, not folded.** `char::is_alphanumeric` is
/// Unicode-aware and accepts them as-is, so `"Chérie"` normalizes to
/// `"chérie"`, not `"cherie"`. A station whose name and announced artist
/// differ only by a diacritic would therefore not be recognized as a filler.
/// Not observed across the 365 stations measured — Chérie FM itself announces
/// the unaccented `CHERIE FM` on both sides — so proper diacritic folding
/// (Unicode normalization, a new dependency) is deliberately not built for a
/// case nothing measured requires.
fn normalized(s: &str) -> String {
    s.to_lowercase()
        .replace('&', "et")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// True when the announced image is a station wallpaper rather than this
/// item's artwork.
///
/// Measured: six stations out of 365 carry one, and **four of them announced a
/// perfectly valid artist at the same moment**. Text and cover are two
/// independent problems; an earlier rule that conflated them classified real
/// tracks as filler. Announcing the wallpaper would also block `musicbrainz`
/// from finding a real cover, a filled field being a filled field.
fn is_wallpaper(img_url: &str) -> bool {
    img_url.rsplit('/').next().is_some_and(|f| f.to_lowercase().contains("default"))
}

/// Splits a trailing `(YYYY)` off a title.
///
/// Measured on Nostalgie's main station, across a 30-sample poll: two distinct
/// real tracks, both carrying a trailing year — `TOMBE POUR LA FRANCE (1986)`
/// and `ANDY (1986)`. The format is systematic on that station, not an
/// isolated exception. Leaving it in the title costs twice — the year shows
/// glued to the title, and `musicbrainz` receives a polluted title to search
/// for, failing a lookup that would have succeeded.
///
/// Only a **plausible** year is detached, so that a remix suffix or a track
/// literally named with four digits keeps its parentheses.
fn split_year(title: &str) -> (String, Option<u16>) {
    let trimmed = title.trim_end();
    let Some(open) = trimmed.strip_suffix(')').and_then(|s| s.rfind('(')) else {
        return (title.to_string(), None);
    };
    let inner = &trimmed[open + 1..trimmed.len() - 1];
    let Some(year) = ritornello_proto::valid_year(inner) else {
        return (title.to_string(), None);
    };
    let head = trimmed[..open].trim_end();
    if head.is_empty() {
        // A title that is nothing but a year is not a title with a year.
        return (title.to_string(), None);
    }
    (head.to_string(), Some(year))
}

/// Parses a response, for the station we asked about.
///
/// `None` for anything unusable — the endpoint is private and undocumented, a
/// redesign must translate into silence, not a wrong display.
///
/// **The identifier is checked, not assumed.** Reading "the first entry" would
/// display another station's titles the day the endpoint answers with more
/// than what was asked.
pub fn parse_station(payload: &str, id: u32) -> Option<Meta> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let station = v.get(id.to_string())?;
    let name = text(station, "name")?;
    let entry = station.get("playlist")?.as_array()?.first()?;
    let ends_at = entry.get("end_timestamp").and_then(Value::as_f64);
    let song = entry.get("song")?;
    let artist = text(song, "artist");
    let title = text(song, "title");
    if artist.is_none() && title.is_none() {
        return None;
    }
    let logos = station.get("logos");
    let logo = |size: &str| logos.and_then(|l| text(l, size));

    // The station naming itself: nothing is playing that the server can name.
    // Staying silent is not neutral here — an empty enrichment is a
    // withdrawal, and the display falls back to the raw ICY, i.e. the cart
    // code. The station's own name is the least wrong of the three options.
    if artist.as_deref().is_some_and(|a| normalized(a) == normalized(&name)) {
        return Some(Meta {
            artist: None,
            title: Some(name),
            year: None,
            cover: logo("640"),
            cover_thumb: logo("173"),
            ends_at,
        });
    }

    let (title, year) = match title {
        Some(t) => {
            let (t, y) = split_year(&t);
            (Some(t), y)
        }
        None => (None, None),
    };
    let img = text(song, "img_url").filter(|u| !is_wallpaper(u));
    Some(Meta {
        artist,
        title,
        year,
        cover_thumb: img.as_deref().map(|u| u.replace("/600x/", "/200x/")),
        cover: img,
        ends_at,
    })
}

/// Queries one station once.
pub async fn query(client: &reqwest::Client, brand: &str, id: u32) -> Result<Meta> {
    let resp = client.get(station_url(brand, id)).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let body = resp.text().await?;
    let Some(meta) = parse_station(&body, id) else {
        bail!("unreadable response ({} bytes)", body.len());
    };
    Ok(meta)
}

/// Next backoff after a failure, given the current backoff.
pub fn next_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(BACKOFF_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIRE: &str = include_str!("../tests/fixtures/rire-200.json");

    /// Builds a one-station response with the given song fields. Keeps each
    /// test to the one thing it is about.
    fn response(name: &str, artist: &str, title: &str, img: &str) -> String {
        serde_json::json!({
            "200": {
                "id": "200",
                "name": name,
                "playlist": [{
                    "song": {
                        "id": 0,
                        "title": title,
                        "artist": artist,
                        "img_url": img,
                    },
                    "end_timestamp": 1788704410.735,
                }],
                "logos": {
                    "173": "https://x/173.png",
                    "640": "https://x/640.png",
                },
            },
        })
        .to_string()
    }

    #[test]
    fn reads_a_real_response() {
        let m = parse_station(RIRE, 200).expect("a real capture parses");
        assert_eq!(m.artist.as_deref(), Some("TOM VILLA"));
        assert_eq!(m.title.as_deref(), Some("On se juge"));
        assert_eq!(
            m.cover.as_deref(),
            Some("https://players.nrjaudio.fm/live-metadata/player/img/600x/C203-ID_RIRE-NGV4-14.JPG")
        );
        assert_eq!(
            m.cover_thumb.as_deref(),
            Some("https://players.nrjaudio.fm/live-metadata/player/img/200x/C203-ID_RIRE-NGV4-14.JPG"),
            "the thumbnail is derived by swapping the size segment"
        );
        assert_eq!(m.ends_at, Some(1788704410.735));
        assert_eq!(m.year, None, "no year on this one");
    }

    #[test]
    fn a_song_id_of_zero_is_not_a_filler() {
        // Measured and corrected: Rire & Chansons uses id 0 for all of its
        // real content. An earlier rule keyed on it classified real tracks as
        // filler.
        let m = parse_station(RIRE, 200).unwrap();
        assert_eq!(m.artist.as_deref(), Some("TOM VILLA"));
    }

    #[test]
    fn the_station_announcing_itself_is_a_filler() {
        // The four cases measured across the 365 stations, verbatim.
        for (name, artist, title) in [
            ("CHERIE FM", "CHERIE FM", "Chérie FM La Playlist 2000"),
            ("NOSTALGIE", "Nostalgie", "Nostalgie, tous les tubes 80 !"),
            ("NOSTALGIE MIX 80", "NOSTALGIE MIX 80", "Mix 80 N°13"),
            // Punctuation and spacing differ on this one, which is why the
            // comparison normalizes rather than compares raw.
            ("NRJ HITS MIXES", "NRJ HITSMIXES", "10h20 Playlist Mixee"),
        ] {
            let body = response(name, artist, title, "https://x/img/600x/v4_1-2.JPG");
            let m = parse_station(&body, 200).unwrap();
            assert_eq!(m.title.as_deref(), Some(name), "{name}: the station names itself");
            assert_eq!(m.artist, None, "{name}: no artist on a filler");
            assert_eq!(m.cover.as_deref(), Some("https://x/640.png"), "{name}: the logo");
            assert_eq!(m.cover_thumb.as_deref(), Some("https://x/173.png"), "{name}");
        }
    }

    #[test]
    fn accents_are_kept_so_a_diacritic_only_difference_is_not_recognized() {
        // Pins a known, deliberate gap: `normalized` folds case, spacing and
        // `&`/`et`, but not diacritics — accent folding needs Unicode
        // normalization, a dependency nothing measured across the 365
        // stations justifies (see `normalized`'s doc comment). So this one is
        // NOT recognized as the station naming itself, unlike the spacing-only
        // case right below, which is.
        assert_ne!(normalized("Chérie"), normalized("Cherie"));
        // The measured case this rule does have to catch still works.
        assert_eq!(normalized("NRJ HITS MIXES"), normalized("NRJ HITSMIXES"));
    }

    #[test]
    fn a_real_artist_is_not_a_filler_even_with_a_default_image() {
        // Measured: four of the six stations carrying a default image had a
        // perfectly valid artist. The text and the cover are two independent
        // problems, and conflating them silenced real tracks.
        let body = response(
            "NRJ HITS REMIX",
            "TEDDY SWIMS",
            "Bad Dreams (Hugel Remix)",
            "https://x/img/600x/v4_421_nrj-default.png",
        );
        let m = parse_station(&body, 200).unwrap();
        assert_eq!(m.artist.as_deref(), Some("TEDDY SWIMS"));
        assert_eq!(m.title.as_deref(), Some("Bad Dreams (Hugel Remix)"));
        assert_eq!(m.cover, None, "a station wallpaper is not this track's cover");
        assert_eq!(m.cover_thumb, None);
    }

    #[test]
    fn a_trailing_year_is_detached_from_the_title() {
        // Measured on Nostalgie's main station across a 30-sample poll: two
        // distinct real tracks, both carrying a trailing year —
        // `TOMBE POUR LA FRANCE (1986)` and `ANDY (1986)`. The format is
        // systematic on that station, not an isolated exception. Leaving it
        // in the title costs twice — the year shows glued to the title, and
        // `musicbrainz` receives a polluted title to search for.
        let body = response(
            "NOSTALGIE",
            "ETIENNE DAHO",
            "TOMBE POUR LA FRANCE (1986)",
            "https://x/img/600x/v4_1-2.JPG",
        );
        let m = parse_station(&body, 200).unwrap();
        assert_eq!(m.title.as_deref(), Some("TOMBE POUR LA FRANCE"));
        assert_eq!(m.year, Some(1986));
    }

    #[test]
    fn parentheses_that_are_not_a_year_stay_in_the_title() {
        // A remix suffix looks the same to a careless rule.
        let body = response(
            "NRJ",
            "TEDDY SWIMS",
            "Bad Dreams (Hugel Remix)",
            "https://x/img/600x/v4_1-2.JPG",
        );
        let m = parse_station(&body, 200).unwrap();
        assert_eq!(m.title.as_deref(), Some("Bad Dreams (Hugel Remix)"));
        assert_eq!(m.year, None);
        // A four-digit number that is not a plausible year is not one either.
        let body = response("NRJ", "X", "Track (3000)", "https://x/img/600x/v4_1-2.JPG");
        let m = parse_station(&body, 200).unwrap();
        assert_eq!(m.title.as_deref(), Some("Track (3000)"));
        assert_eq!(m.year, None);
    }

    #[test]
    fn an_unexpected_shape_is_discarded_without_noise() {
        // The endpoint is private and undocumented: a redesign must translate
        // into silence, not a wrong display.
        assert!(parse_station("not json at all", 200).is_none());
        assert!(parse_station("{}", 200).is_none(), "the station is absent");
        assert!(parse_station(r#"{"200":{"name":"X","playlist":[]}}"#, 200).is_none());
        assert!(
            parse_station(r#"{"200":{"name":"X","playlist":[{"song":{}}]}}"#, 200).is_none(),
            "neither artist nor title is not an answer"
        );
    }

    #[test]
    fn the_answer_of_another_station_is_refused() {
        // A host only serves its own stations; asking for a foreign id returns
        // an empty object. Reading "the first entry" would one day display the
        // wrong station.
        assert!(parse_station(RIRE, 158).is_none());
    }

    #[test]
    fn the_url_carries_the_brand_and_the_identifier() {
        assert_eq!(
            station_url("rireetchansons", 200),
            "https://www.rireetchansons.fr/api/webradios/get-by-ids?ids[]=200"
        );
        assert_eq!(
            station_url("cheriefm", 190),
            "https://www.cheriefm.fr/api/webradios/get-by-ids?ids[]=190"
        );
    }
}
