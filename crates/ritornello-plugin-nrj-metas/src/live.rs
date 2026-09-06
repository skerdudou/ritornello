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
use tokio::sync::mpsc;

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
    /// what is already the right size. Measured on 51 images across the four
    /// brands — `/600x/` and `/200x/` both present on all 51, averaging
    /// 75,497 and 12,694 bytes — which is why this derivation is kept despite
    /// resting on a sample rather than a documented contract: the pair saves
    /// six times the bytes the core's cover cache is budgeted in.
    /// **If a `/200x/` variant is ever missing, the core does not fall
    /// back to the full size** (`cover.rs`: a failed thumbnail fetch is not a
    /// reason to try the full size) — that track then shows no cover at all.
    /// This is why the derivation rests on measurement, not on assumption.
    pub cover_thumb: Option<String>,
    /// End of the current item, in seconds since the Unix epoch, as the server
    /// announces it. Raw: it is `main` that turns it into a deadline, so this
    /// module stays clock-free and testable on captures.
    pub ends_at: Option<f64>,
}

/// Initial wait before retrying after a failure, then doubled.
const BACKOFF_BASE: Duration = Duration::from_secs(2);

/// Backoff cap. A device that runs unattended for months must not hammer a
/// third party's server.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Wait after a poke before querying — a filter, not a politeness.
///
/// Measured: a four-second jingle aired between two tracks on NRJ. A poke
/// restarts this wait, so anything shorter than the delay never causes a
/// request. It has to clear a jingle (4 s) without approaching the server's
/// own lag (30 to 70 s), which dominates the total anyway — so the wait costs
/// nothing in perceived reactivity.
///
/// **A cart code that changes faster than this starves the station.** The
/// coalescing loop keeps restarting the wait on every poke and never reaches
/// a query — and the very same restart is what discards whatever wait the
/// safety net had set, so neither a query nor the net ever fires. Safe: no
/// request rate results. But the display then stays on a stale title until
/// the code finally holds still for a whole `DEBOUNCE`.
const DEBOUNCE: Duration = Duration::from_secs(6);

/// Wait before the very first query on a freshly created task, instead of at
/// once.
///
/// Small on purpose, not zero: presets and the +10 grid are first-class
/// features of this appliance, and scrolling through several of them within
/// a few seconds is an ordinary gesture. Each switch aborts the previous
/// station's task (see `main`'s `follows`), so without this wait a ten-preset
/// scroll in ten seconds would fire ten requests instead of collapsing to
/// the one for the preset actually landed on. Kept short enough to stay
/// imperceptible next to the measured 30 s connection advert either way, and
/// well under it: the first real cart code only arrives at t+61 s (see the
/// module doc), so waiting for a *change* instead of a short fixed delay
/// would leave the screen blank for a minute after every station change.
const INITIAL_WAIT: Duration = Duration::from_secs(2);

/// Spacing of the retries when the server has not caught up yet.
///
/// Measured twice: the JSON followed the ICY within 33 seconds, and it was 40
/// to 70 seconds past its own `end_timestamp`. Three retries cover the
/// measured window; beyond that we stop rather than keep asking.
///
/// **The ladder cannot tell the two apart.** "The server has not caught up
/// yet" and "the content genuinely did not change" look identical here — both
/// are a fresh reading equal to the last one emitted (see `verdict`). A poke
/// on a station stuck on filler therefore costs four requests, not one: the
/// query the poke triggered, plus all three retries before giving up. Filler
/// is not the exception on some stations either — a 30-sample poll of
/// Nostalgie's main station found it filler on 24 of 30.
const RETRIES: [Duration; 3] =
    [Duration::from_secs(10), Duration::from_secs(20), Duration::from_secs(40)];

/// Margin added to the announced deadline before the safety net fires.
///
/// Measured, the server is late on its own `end_timestamp`; a net that fired
/// on the dot would query for the previous item. Wide on purpose: this path
/// exists for a station whose cart code does not move, not for the common
/// case.
const NET_MARGIN: Duration = Duration::from_secs(90);

/// Ceiling for the safety net, so an absurd deadline cannot park the station
/// for hours.
const NET_MAX: Duration = Duration::from_secs(900);

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
    // `ritornello_proto::valid_year` is prefix-based (it accepts
    // "19860101"): `inner` only has to *start* with four digits, not equal
    // them. Without this length check, a title ending "(2011 Remaster)"
    // would have its remaster's year sliced off and shown, since "2011
    // Remaster" starts with a plausible year. Only a parenthesized group
    // that is *exactly* four characters may denote one.
    if inner.chars().count() != 4 {
        return (title.to_string(), None);
    }
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
fn next_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(BACKOFF_MAX)
}

/// Copy of `meta` with `ends_at` cleared, for the "is this the same item as
/// last time" comparison.
///
/// `Meta` derives a plain, total `PartialEq` that includes `ends_at`,
/// deliberately so elsewhere. But `end_timestamp` drifts by a fraction of a
/// second between two queries for the very same item — measured, it is
/// otherwise stable per item — so comparing whole `Meta`s here would read
/// every safety-net wake-up as a new item and re-emit the same track forever,
/// one display write and one SSE frame per station, silently.
fn without_ends_at(meta: &Meta) -> Meta {
    Meta { ends_at: None, ..meta.clone() }
}

/// What to do with a fresh reading, given the last one emitted and how many
/// retries have already been spent waiting for the JSON to catch up.
///
/// Extracted so decision 1 (the `ends_at`-blind comparison) and the retry
/// ladder are both provable without a clock or a socket: a regression on
/// either — comparing whole `Meta`s again, or losing the ladder — would fail
/// a test here rather than only show up as a silent re-emission in
/// production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// A genuinely new item: emit it.
    Emit,
    /// The server has not caught up yet: wait this long, then ask again.
    Retry(Duration),
    /// The server never caught up within the ladder: stop asking until the
    /// next poke or the safety net.
    GiveUp,
}

fn verdict(last_seen: Option<&Meta>, fresh: &Meta, attempt: usize) -> Verdict {
    if last_seen.map(without_ends_at) != Some(without_ends_at(fresh)) {
        return Verdict::Emit;
    }
    // The server has not caught up yet: measured, it can be a full minute
    // behind the stream.
    match RETRIES.get(attempt) {
        Some(delay) => Verdict::Retry(*delay),
        None => Verdict::GiveUp,
    }
}

/// The announced deadline, read from the caller's own clock.
///
/// Kept distinct from a bare `Option<Duration>` so "already past" cannot
/// collapse onto "not announced at all" the way it did before this fix: both
/// used to feed `net_delay_from` as a plain `None`, which parked a station
/// whose deadline had merely elapsed behind the same `NET_MAX` (15 min) as
/// one that never announced one at all — the worst outcome on exactly the
/// station the net exists for, one whose cart code does not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deadline {
    /// Still ahead, by this much.
    Remaining(Duration),
    /// Already in the past.
    Elapsed,
    /// No `end_timestamp` in the response at all.
    Absent,
}

/// How long to sleep before the safety net fires, given the announced
/// deadline. Pure, so the clamping is testable without a clock.
fn net_delay_from(deadline: Deadline) -> Duration {
    match deadline {
        Deadline::Remaining(r) => (r + NET_MARGIN).min(NET_MAX),
        // Measured, the server is late on its own deadline — this is the
        // ordinary case for a station whose cart code has stopped moving,
        // not a fault. `NET_MARGIN` alone, not `NET_MAX`: a fresh wake-up is
        // due soon, not in fifteen minutes.
        Deadline::Elapsed => NET_MARGIN,
        // Nothing else to go on: the ceiling applies.
        Deadline::Absent => NET_MAX,
    }
}

fn net_delay(ends_at: Option<f64>) -> Duration {
    let deadline = match ends_at.zip(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs_f64()),
    ) {
        // No `end_timestamp`, or a clock that reads before the Unix epoch:
        // either way nothing reliable to compare against.
        None => Deadline::Absent,
        Some((end, now)) if end > now => Deadline::Remaining(Duration::from_secs_f64(end - now)),
        Some(_) => Deadline::Elapsed,
    };
    net_delay_from(deadline)
}

/// Follows a station until the task is aborted.
///
/// Returns only when the plugin drops its sender (`tx`) or the channel it
/// reads pokes from is itself dropped — the ordinary way this task ends is
/// the caller aborting it (`abort`) when the station changes, not this
/// function returning on its own. Each reading is tagged with the `(brand,
/// id)` pair (see `main`'s `same_station`): a reading already queued at the
/// moment of the stop must be discardable, and discardable unambiguously —
/// `id` alone is not unique across an operator's override file.
///
/// **Only changes are emitted.** The first reading always goes out: this task
/// is born with the station, so its "last seen" is empty, and the display
/// fills in from the first response rather than at the next item.
///
/// **Poked, not polled.** Unlike Radio France, which tells us when to call
/// back, this endpoint's own deadline is measurably late (see `NET_MARGIN`).
/// The rhythm therefore comes from `poke`, fed by the core's ICY cart code
/// (see `main`'s `now_playing`), with the announced deadline kept only as a
/// safety net for a station whose cart code stops moving.
///
/// **A query failure sets the next wait to the backoff, not to the safety
/// net.** Falling through to `net_delay` on error would strand a station
/// behind a transient blip for up to `NET_MAX` (15 min) — and for a station
/// whose cart code never moves, the net is the *only* thing that would ever
/// wake it up again, so a poke could not rescue it either.
///
/// **A poke cannot shrink that backoff below `DEBOUNCE`.** The debounce loop
/// below restarts on every poke with `wait = DEBOUNCE`, discarding whatever
/// longer wait a prior failure had set — see the hard floor right after it,
/// which is precisely what stops a station whose cart code changes faster
/// than the backoff from sustaining a steady query rate against a host that
/// is failing every time.
pub async fn follows(
    brand: String,
    id: u32,
    mut poke: mpsc::Receiver<()>,
    tx: mpsc::Sender<(String, u32, Meta)>,
) {
    let client = match reqwest::Client::builder()
        // Measured: cheriefm.fr answers 403 without a full browser
        // User-Agent. This is a condition of access, not a courtesy.
        //
        // The stack itself was measured too, separately from the header:
        // `reqwest` + `rustls`, with this exact User-Agent, gets HTTP 200
        // from all four brands, cheriefm included, and all four bodies
        // parse. `scripts/fetch-stations.mjs` shells out to `curl` instead
        // (see its own header comment) because Node's stack is refused by
        // cheriefm regardless of headers — that refusal does not apply
        // here.
        .user_agent(
            "Mozilla/5.0 (X11; Linux armv7l) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/128.0 Safari/537.36 ritornello",
        )
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("HTTP client unavailable, station {id} will stay silent: {e}");
            return;
        }
    };
    let mut backoff = BACKOFF_BASE;
    let mut last_seen: Option<Meta> = None;
    let mut last_query: Option<tokio::time::Instant> = None;
    // The first query fires after a short, fixed wait rather than at once —
    // see `INITIAL_WAIT`'s own doc comment for why it is small but not zero.
    let mut wait = INITIAL_WAIT;
    loop {
        // Coalesce pokes: any poke arriving during the wait restarts it, so a
        // jingle shorter than DEBOUNCE never causes a request.
        loop {
            match tokio::time::timeout(wait, poke.recv()).await {
                Ok(Some(())) => wait = DEBOUNCE,
                // The plugin dropped its sender: the station changed.
                Ok(None) => return,
                Err(_) => break,
            }
        }
        // Hard floor: the coalescing above can only ever shrink `wait` down
        // to `DEBOUNCE`, discarding a longer failure backoff on the way (see
        // `follows`'s own doc comment). Measured from the last query actually
        // *sent*, not from the last poke, so a cart code that keeps changing
        // cannot repeatedly reset the clock and hold the spacing at bare
        // `DEBOUNCE` forever — the very thing that let a permanently failing
        // host be queried at the stream's own rate. The happy path is
        // untouched: `backoff` sits at `BACKOFF_BASE` after every success, so
        // the floor there is `DEBOUNCE`, exactly what the debounce loop above
        // already guaranteed.
        let floor = DEBOUNCE.max(backoff);
        if let Some(last) = last_query {
            let elapsed = last.elapsed();
            if elapsed < floor {
                tokio::time::sleep(floor - elapsed).await;
            }
        }
        last_query = Some(tokio::time::Instant::now());
        let mut attempt = 0usize;
        // The inner loop resolves directly to the next `wait`, so an error
        // and a give-up both set it explicitly rather than falling through
        // to a shared `net_delay(ends_at)` line that an error path could
        // silently reuse.
        wait = loop {
            match query(&client, &brand, id).await {
                Ok(meta) => {
                    backoff = BACKOFF_BASE;
                    match verdict(last_seen.as_ref(), &meta, attempt) {
                        Verdict::Retry(delay) => {
                            attempt += 1;
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        Verdict::GiveUp => break net_delay(meta.ends_at),
                        Verdict::Emit => {
                            let ends_at = meta.ends_at;
                            last_seen = Some(meta.clone());
                            if tx.send((brand.clone(), id, meta)).await.is_err() {
                                return;
                            }
                            break net_delay(ends_at);
                        }
                    }
                }
                Err(e) => {
                    // Every failure is logged: without that, a station that
                    // stops answering would leave no trace in `/api/logs` and
                    // nobody would ever see anything.
                    tracing::info!("metadata query failed for station {id} ({brand}): {e}");
                    // No sleep here: the current `backoff` becomes the next
                    // `wait`, and unlike before this fix, a poke can no
                    // longer make the next query happen any sooner than
                    // `backoff` allows — the hard floor at the top of the
                    // outer loop enforces that regardless of how `wait` is
                    // spent.
                    let this_backoff = backoff;
                    backoff = next_backoff(backoff);
                    break this_backoff;
                }
            }
        };
    }
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
    fn a_remaster_suffix_is_not_read_as_a_year() {
        // `ritornello_proto::valid_year` is prefix-based (it accepts
        // "19860101"), so a careless rule would slice "2011" off the end of
        // "(2011 Remaster)" — the parenthesized content merely *starts* with
        // a plausible year — and display the remaster's year as if it were
        // the track's.
        let body = response(
            "NRJ",
            "PIXIES",
            "Where Is My Mind (2011 Remaster)",
            "https://x/img/600x/v4_1-2.JPG",
        );
        let m = parse_station(&body, 200).unwrap();
        assert_eq!(m.title.as_deref(), Some("Where Is My Mind (2011 Remaster)"));
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
    fn the_net_waits_past_the_announced_deadline() {
        // Measured: the server updates 40 to 70 seconds after its own
        // end_timestamp. A net firing on the dot would query for the item
        // that just ended.
        assert_eq!(
            net_delay_from(Deadline::Remaining(Duration::from_secs(30))),
            Duration::from_secs(120)
        );
        // An absurdly distant deadline must not park the station past the
        // ceiling either.
        assert_eq!(net_delay_from(Deadline::Remaining(Duration::from_secs(9999))), NET_MAX);
        // Never announced at all: nothing else to go on, so the ceiling
        // applies.
        assert_eq!(net_delay_from(Deadline::Absent), NET_MAX);
        // Already elapsed — the exact case the net exists for, a station
        // whose cart code has stopped moving — must NOT be parked for
        // NET_MAX (15 min) the way an absent deadline is: a regression
        // collapsing this back onto `Absent` would starve that station of
        // its only remaining wake-up for a quarter of an hour.
        assert_eq!(net_delay_from(Deadline::Elapsed), NET_MARGIN);
    }

    #[test]
    fn the_ends_at_comparison_ignores_the_deadline() {
        // Decision: the "same item" comparison used by `follows` must ignore
        // `ends_at`, since it drifts by a fraction of a second between two
        // queries for the very same item. `Meta`'s own `PartialEq` stays
        // total (it does include `ends_at`) for everyone else.
        let a = Meta { title: Some("X".into()), ends_at: Some(100.0), ..Default::default() };
        let b = Meta { title: Some("X".into()), ends_at: Some(100.735), ..Default::default() };
        assert_ne!(a, b, "whole-Meta equality still sees the drift");
        assert_eq!(without_ends_at(&a), without_ends_at(&b), "but the filtered form does not");
    }

    /// The pure decision behind `follows`'s inner loop, covering both
    /// decision 1 (the `ends_at`-blind comparison) and the retry ladder,
    /// without a clock or a socket. A regression on either — comparing whole
    /// `Meta`s again, or losing the ladder — fails here rather than only
    /// showing up as a silent re-emission or an inert backoff in production.
    #[test]
    fn a_first_reading_is_always_emitted() {
        let fresh = Meta { title: Some("X".into()), ends_at: Some(100.0), ..Default::default() };
        assert_eq!(verdict(None, &fresh, 0), Verdict::Emit);
    }

    #[test]
    fn a_genuinely_different_reading_is_emitted() {
        let last = Meta { title: Some("X".into()), ends_at: Some(100.0), ..Default::default() };
        let fresh = Meta { title: Some("Y".into()), ends_at: Some(200.0), ..Default::default() };
        assert_eq!(verdict(Some(&last), &fresh, 0), Verdict::Emit);
    }

    #[test]
    fn the_same_reading_up_to_ends_at_drift_is_retried_then_given_up_on() {
        // `ends_at` alone differing must NOT count as "genuinely different":
        // this is the property decision 1 exists for. Comparing whole
        // `Meta`s here (a regression) would make this assert `Verdict::Emit`
        // instead, re-emitting the same track on every safety-net wake-up.
        let last = Meta { title: Some("X".into()), ends_at: Some(100.0), ..Default::default() };
        let fresh = Meta { title: Some("X".into()), ends_at: Some(100.735), ..Default::default() };
        assert_eq!(verdict(Some(&last), &fresh, 0), Verdict::Retry(Duration::from_secs(10)));
        assert_eq!(verdict(Some(&last), &fresh, 1), Verdict::Retry(Duration::from_secs(20)));
        assert_eq!(verdict(Some(&last), &fresh, 2), Verdict::Retry(Duration::from_secs(40)));
        // The ladder has three rungs: past it, we stop asking rather than
        // keep retrying forever.
        assert_eq!(verdict(Some(&last), &fresh, 3), Verdict::GiveUp);
        assert_eq!(verdict(Some(&last), &fresh, 100), Verdict::GiveUp);
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

    #[test]
    fn next_backoff_doubles_then_caps_at_backoff_max() {
        // Pure and trivially testable, and the cap is a third-party-courtesy
        // invariant nothing else guards: deleting `.min(BACKOFF_MAX)` from
        // `next_backoff` would fail no other test in this module.
        assert_eq!(next_backoff(BACKOFF_BASE), Duration::from_secs(4));
        assert_eq!(
            next_backoff(Duration::from_secs(45)),
            BACKOFF_MAX,
            "doubling past the cap must clamp, not overshoot it"
        );
        assert_eq!(
            next_backoff(BACKOFF_MAX),
            BACKOFF_MAX,
            "the cap does not creep upward under its own repeated input"
        );
    }
}
