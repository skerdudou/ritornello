//! Querying the live feed of a Radio France station.
//!
//! Parsing is a pure function, tested on real responses; only `follows`
//! touches the network, and **no test calls it**.
//!
//! Unlike OUI FM, which pushes its metadata over a `text/event-stream`,
//! Radio France answers a one-off query — but tells us itself when to call
//! back (`delayToRefresh`). The polling rhythm is therefore dictated by the
//! server, not by us: that is what makes it possible to follow a
//! three-minute track without hammering a third party, and to leave an
//! hour-long air segment alone.

use anyhow::{bail, Result};
use ritornello_proto::Link;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

/// What a response tells us about the live feed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    /// Year and links come from the **schedule**, like the album, so they are
    /// filled in at the same moment (see `follows`). The live feed does not
    /// carry them.
    pub year: Option<u16>,
    pub links: Vec<Link>,
    pub duration_s: Option<u32>,
    /// Start of the track, in seconds since the Unix epoch, as announced by
    /// the live feed. Raw: it is the enrichment emission that derives the
    /// elapsed time from it, so that this module stays clock-free and
    /// testable on captures.
    pub start_time: Option<u64>,
    /// Raw UUID of the cover, copied from `Direct.cover` in `follows` — this
    /// field is what crosses the channel to the plugin, which turns it into a
    /// URL (see `cover_url`). `None` includes the case where this is not a
    /// track: that rule is already settled upstream, in `Direct.cover`.
    pub cover: Option<String>,
}

/// A parsed response: what is on air, and how long until we should call back.
#[derive(Debug, Clone, PartialEq)]
pub struct Direct {
    /// `None` when the response carries neither title nor artist — the case
    /// of a broadcast handover. The delay, however, remains usable.
    pub meta: Option<Meta>,
    /// Identifier of the current track, when there is one. It is never
    /// displayed: it is used to find in the schedule what the live feed does
    /// not carry — album, year, link (see `supplement_in_schedule`).
    pub song_uuid: Option<String>,
    /// UUID of the cover, **only when a real track is playing**.
    ///
    /// The station fills in a `cover` even for « Le direct » and for its
    /// shows: it is the generic image of the channel. Announcing it would
    /// silence the generic fallback, since a filled field is a filled field
    /// and no upper layer can know that it is filled wrongly.
    pub cover: Option<String>,
    pub recontact_at: Duration,
}

/// Initial wait before retrying after a failure, then doubled.
const BACKOFF_BASE: Duration = Duration::from_secs(2);

/// Backoff cap. A device that runs unattended for months must not hammer a
/// third party's server; conversely, capping avoids an overnight network
/// outage turning into hours of waiting once it comes back.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Floor for the delay announced by the server. Measured: it goes down to
/// 10 s on stations that switch often. So this floor does not exist to
/// correct the server but to bound what an aberrant response — or a proxy
/// rewriting the JSON — could make us do.
const RECHECK_MIN: Duration = Duration::from_secs(5);

/// Cap for the announced delay. Measured: the local stations announce up to
/// 51 min, i.e. the end of the current segment. Taking their word for it
/// would leave the display frozen that long if the schedule changes along the
/// way; ten minutes costs at worst six requests per hour per station.
const RECHECK_MAX: Duration = Duration::from_secs(600);

/// Delay used when the server announces none.
const RECHECK_DEFAULT: Duration = Duration::from_secs(60);

/// Number of consecutive tracks for which the schedule teaches **nothing**
/// beyond which we stop querying it for this station.
///
/// The schedule often publishes the track **one behind**: measured, it stops
/// exactly at the start of what is on air. On some stations it catches up
/// within seconds; on others — the 45 local stations, notably — it never has
/// anything for the whole duration of a track. Continuing to ask would double
/// the number of requests to a third party for an answer that never comes,
/// which this cap avoids.
///
/// The criterion covers the **whole supplement** (album, year, links) and not
/// the album alone since 2026-08-27: the schedule returns the year far more
/// often than the album — 9 items out of 9 measured, versus 3 out of 9 for
/// the YouTube link — and a request that brings back the year is not a
/// request for nothing.
const MAX_MISSES: u32 = 5;

/// Maximum plausible duration for an on-air item. Beyond that, the duration
/// comes from an aberrant bound and is better ignored than displayed.
const MAX_DURATION_S: u64 = 24 * 3600;

/// Live-feed URL of a station, for a given rendering profile.
///
/// The last segment does not identify the station but the **rendering
/// profile** the server applies to its response, and it changes what we
/// receive — to the point that a wrong choice makes the plugin silent.
/// Measured at the same instant on Mouv': `webrf_fip_player` answers
/// « Le direct » / « Mouv' » (the slogan), while `webrf_mouv_player` answers
/// « La Playlist » / « SOOLKING - Bye Bye (feat. TAYC) », which is indeed
/// what was on air. Each station therefore carries its profile in the table.
fn live_url(id: u32, profile: &str) -> String {
    format!("https://api.radiofrance.fr/livemeta/live/{id}/{profile}")
}

/// Schedule URL of a station: the list of broadcast items, where each track
/// carries its album. No rendering profile here, the shape is unique.
fn schedule_url(id: u32) -> String {
    format!("https://api.radiofrance.fr/livemeta/pull/{id}")
}

/// Cover URL of a track.
///
/// `preset` is not optional: without it, the API returns a 400. With it, it
/// returns a 301 to the CDN, which the core follows. `400x400` is a measured
/// compromise — 31,887 bytes, versus an original of unbounded size.
pub fn cover_url(uuid: &str) -> String {
    format!("https://api.radiofrance.fr/v1/services/embed/image/{uuid}?preset=400x400")
}

/// Non-empty text of a field, `None` otherwise.
fn text(v: &Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Parses a live-feed response. `None` for anything that is not usable
/// JSON — the endpoint is undocumented, a redesign must translate into
/// silence, not a wrong display.
///
/// The field names are the measured ones. `now.firstLine` and
/// `now.secondLine` carry the pair to display, but **what it contains depends
/// on the profile** (see `live_url`), and the response says so itself:
///
/// - with `firstLineSongUuid`, `firstLine` **is** the track and `secondLine`
///   its artist — the pair is already split, and the bounds delimit the
///   track, so their gap really is its duration;
/// - without it, `firstLine` is the **show** and `secondLine` carries what is
///   playing within it, as a single string « ARTIST - Title ». The bounds are
///   then those of the show: measured on Mouv', they covered an hour. Taking
///   them for the duration of a track would display a wrong progress bar, so
///   the duration is discarded in that case.
pub fn parse_direct(payload: &str) -> Option<Direct> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let recontact_at = v
        .get("delayToRefresh")
        .and_then(Value::as_u64)
        .map(|ms| Duration::from_millis(ms).clamp(RECHECK_MIN, RECHECK_MAX))
        .unwrap_or(RECHECK_DEFAULT);
    let Some(now) = v.get("now") else {
        // Well-formed response but no live feed: nothing to say, we'll come back.
        return Some(Direct { meta: None, song_uuid: None, cover: None, recontact_at });
    };
    let is_a_track = now.get("firstLineSongUuid").is_some_and(|u| !u.is_null());
    let duration = match (now.get("startTime").and_then(Value::as_u64), now.get("endTime").and_then(Value::as_u64)) {
        (Some(start), Some(end)) if end > start => Some(end - start),
        _ => None,
    };
    let title = text(now, "firstLine");
    let artist = text(now, "secondLine");
    // Two identical lines teach nothing twice: that is what a local station
    // returns outside music (« Le 18/19, ICI Picardie » on both sides), and
    // displaying it would give « X — X ».
    let artist = artist.filter(|a| !title.as_ref().is_some_and(|t| t.trim().eq_ignore_ascii_case(a.trim())));
    // "It is a track AND the duration is plausible": a single expression,
    // used for `duration_s` as well as `start_time`. Written twice, it could
    // drift; `start_time` would then leave without `duration_s`, and the
    // position capping on the core side — which needs both — would vanish
    // silently, the bar crossing past the end of the track.
    let plausible_track = is_a_track && duration.is_some_and(|d| d <= MAX_DURATION_S);
    let meta = Meta {
        title,
        artist,
        // The live feed carries neither album, nor year, nor link: all of
        // that is read from the schedule, separately.
        album: None,
        year: None,
        links: Vec::new(),
        duration_s: duration.filter(|_| plausible_track).map(|d| d as u32),
        start_time: now.get("startTime").and_then(Value::as_u64).filter(|_| plausible_track),
        // Filled in later, in `follows`, from `Direct.cover`: at this stage,
        // the pure parsing only knows the track, not yet the channel that
        // carries it to the plugin.
        cover: None,
    };
    // A duration alone is not displayable: it is not an answer.
    let meta = (meta.artist.is_some() || meta.title.is_some()).then_some(meta);
    let song_uuid = text(now, "songUuid");
    // The `songUuid` is the only reliable discriminant between a track and a
    // show — measured on four stations.
    let cover = song_uuid.as_ref().and_then(|_| text(now, "cover"));
    Some(Direct { meta, song_uuid, cover, recontact_at })
}

/// What the schedule item teaches beyond the album.
///
/// Grouped because they are read from the **same** item as `titreAlbum`:
/// looking them up separately would re-read the schedule three times for a
/// single response already in hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supplement {
    pub album: Option<String>,
    /// `anneeEditionMusique`, a JSON **number** in the measured responses.
    pub year: Option<u16>,
    /// `lienYoutube`. Validated by `Link::validated` on the core side, but
    /// already filtered here on its host: no point transmitting what will be
    /// refused.
    pub links: Vec<Link>,
}

impl Supplement {
    pub fn is_empty(&self) -> bool {
        self.album.is_none() && self.year.is_none() && self.links.is_empty()
    }
}

/// Album of track `song_uuid` in a schedule response, if it appears there.
///
/// The match is on `songId`, **not** on `uuid`: `uuid` identifies the
/// schedule item, `songId` the track, and the latter is what the live feed
/// returns in `songUuid`. Verified on four stations, all agreeing on `songId`
/// and none on `uuid`.
///
/// `None` is the common case, not an anomaly: the schedule often publishes
/// the track one behind, and the album is then simply not there yet.
/// Everything the track's schedule item teaches: album, year, links.
///
/// One single pass for all three. `Supplement::default()` when the schedule
/// does not know the track — the common case, it is often one track behind,
/// and that is not an anomaly.
pub fn supplement_in_schedule(payload: &str, song_uuid: &str) -> Supplement {
    let Ok(v) = serde_json::from_str::<Value>(payload) else { return Supplement::default() };
    let Some(steps) = v.get("steps").and_then(Value::as_object) else {
        return Supplement::default();
    };
    let Some(step) = steps.values().find(|s| s.get("songId").and_then(Value::as_str) == Some(song_uuid))
    else {
        return Supplement::default();
    };
    // `anneeEditionMusique` is a number in the measured responses, but the
    // text form is accepted too: the field comes from a third party that can
    // change shape without notice, exactly like `durationInSeconds` at OUI FM.
    let year = match step.get("anneeEditionMusique") {
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
    .as_deref()
    .and_then(ritornello_proto::valid_year);
    let links = text(step, "lienYoutube")
        .map(|url| Link::Youtube { url })
        .and_then(Link::validated)
        .into_iter()
        .collect();
    Supplement { album: text(step, "titreAlbum"), year, links }
}

/// Queries the schedule for what the current track gains from it: album,
/// year, links. Any error counts as "nothing found": these are supplements,
/// they must never prevent the title from going out.
async fn fetch_supplement(client: &reqwest::Client, id: u32, song_uuid: &str) -> Supplement {
    let Ok(resp) = client.get(schedule_url(id)).send().await else { return Supplement::default() };
    if !resp.status().is_success() {
        tracing::debug!("schedule query for station {id}: HTTP {}", resp.status());
        return Supplement::default();
    }
    let Ok(body) = resp.text().await else { return Supplement::default() };
    supplement_in_schedule(&body, song_uuid)
}

/// Queries a station's live feed once.
async fn query(client: &reqwest::Client, id: u32, profile: &str) -> Result<Direct> {
    let resp = client.get(live_url(id, profile)).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let body = resp.text().await?;
    let Some(direct) = parse_direct(&body) else {
        bail!("unreadable response ({} bytes)", body.len());
    };
    Ok(direct)
}

/// Next backoff after a failure, given the current backoff.
pub fn next_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(BACKOFF_MAX)
}

/// Follows a station until the task is aborted: queries the live feed, waits
/// for the announced delay, starts over.
///
/// Never returns. The caller stops this task (`abort`) when what is playing
/// changes — hence the tagging of each reading with the `id`: a reading
/// already queued at the moment of the stop must be discardable.
///
/// **Only changes are emitted.** The server repeats the same thing on every
/// query; re-emitting would make the core write a line every ten seconds for
/// nothing. The first reading, however, always goes out: this task is born
/// with the station, so its "last seen" is empty, and the display fills in
/// from the first response rather than at the next track change.
pub async fn follows(id: u32, profile: String, tx: mpsc::Sender<(u32, Meta)>) {
    let client = match reqwest::Client::builder()
        .user_agent("ritornello/0.1 (https://github.com/skerdudou/ritornello)")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        // Client construction failure (missing TLS stack): unrecoverable, and
        // staying silent beats looping on it.
        Err(e) => {
            tracing::warn!("HTTP client unavailable, station {id} will stay silent: {e}");
            return;
        }
    };
    let mut backoff = BACKOFF_BASE / 2;
    // Last emitted reading, **without its album**: the comparison is done on
    // this form, so that an album found (or not) once does not change the
    // verdict "same track as before" on the next round.
    let mut last_seen: Option<Meta> = None;
    let mut misses = 0u32;
    loop {
        match query(&client, id, &profile).await {
            Ok(direct) => {
                backoff = BACKOFF_BASE / 2;
                if let Some(mut meta) = direct.meta {
                    // `direct.cover` is never rebuilt here: the rule "no cover
                    // outside a track" is already settled in `parse_direct`,
                    // this field is just a baton passed along to the plugin.
                    meta.cover = direct.cover.clone();
                    if last_seen.as_ref() != Some(&meta) {
                        last_seen = Some(meta.clone());
                        // The album is looked up **once per track**, and only
                        // here: across successive queries of the same track,
                        // the answer would not change.
                        let mut to_send = meta;
                        if let Some(uuid) = direct.song_uuid.as_deref()
                            && misses < MAX_MISSES
                        {
                            let s = fetch_supplement(&client, id, uuid).await;
                            // The counter now covers the **whole**
                            // supplement and not the album alone, and this
                            // change of criterion is deliberate: the
                            // schedule returns the year far more often
                            // than the album (measured on 2026-08-27,
                            // 9 items out of 9 versus 3 out of 9 for the
                            // YouTube link). Keeping on querying it when
                            // it gives no album but gives the year is no
                            // longer a request for nothing — which is what
                            // this counter exists to avoid.
                            let empty = s.is_empty();
                            to_send.album = s.album;
                            to_send.year = s.year;
                            to_send.links = s.links;
                            if empty {
                                misses += 1;
                                if misses == MAX_MISSES {
                                    tracing::debug!(
                                        "station {id}: schedule gave nothing for {MAX_MISSES} tracks, no longer asking"
                                    );
                                }
                            } else {
                                misses = 0;
                            }
                        }
                        if tx.send((id, to_send)).await.is_err() {
                            // The plugin no longer listens to us: the station changed.
                            return;
                        }
                    }
                }
                tokio::time::sleep(direct.recontact_at).await;
            }
            Err(e) => {
                // Every failure is logged: without that, a station that stops
                // answering would leave no trace in `/api/logs` and nobody
                // would ever see anything.
                tracing::info!("live query failed for station {id}: {e}");
                backoff = next_backoff(backoff);
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Response **captured verbatim** from the FIP live feed (station 7).
    const FIP_RESPONSE: &str = r#"{"prev":[{"firstLine":"Le direct","secondLine":"La radio la plus éclectique du monde","songUuid":null,"cover":"7eee98cb-3f59-4a3b-b921-6a4be85af542","startTime":null,"endTime":null}],"now":{"firstLine":"I love marijuana","firstLineSongUuid":"1691b015-c8b9-48d2-a296-1f846e13af7b","secondLine":"Linval Thompson","secondLineSongUuid":"1691b015-c8b9-48d2-a296-1f846e13af7b","songUuid":"1691b015-c8b9-48d2-a296-1f846e13af7b","cover":"5b93ce44-3ed6-4409-a2d7-4bd159c061f8","startTime":1786722565,"endTime":1786722762},"next":[],"delayToRefresh":70000}"#;

    /// Response **captured verbatim** from Mouv' (station 6, profile
    /// `webrf_mouv_player`): `firstLine` is the show, `secondLine` the whole
    /// track, and the bounds cover **the show** — one hour.
    const MOUV_RESPONSE: &str = r#"{"prev":[],"now":{"firstLine":"La Playlist","secondLine":"OZUNA - Mi yo de antes","secondLineSongUuid":"c6ed3f57-10a8-435f-b71e-adca48916dce","thirdLine":null,"producers":null,"songUuid":"c6ed3f57-10a8-435f-b71e-adca48916dce","cover":"2df667ba-2852-495c-89a9-9a998daa7c0d","startTime":1786723200,"endTime":1786726800},"next":[],"delayToRefresh":3090000}"#;

    /// Response **captured verbatim** from a local station outside music: the
    /// two lines say the same thing.
    const SILENT_LOCAL_RESPONSE: &str = r#"{"now":{"firstLine":"Le 18/19, ICI Picardie","secondLine":"Le 18/19, ici Picardie","startTime":1786723800,"endTime":1786727400},"delayToRefresh":270000}"#;

    #[test]
    fn parses_a_real_response() {
        let d = parse_direct(FIP_RESPONSE).unwrap();
        let m = d.meta.unwrap();
        // `firstLine` is the title, `secondLine` the artist: the opposite of
        // what the field order suggests at first glance.
        assert_eq!(m.title.as_deref(), Some("I love marijuana"));
        assert_eq!(m.artist.as_deref(), Some("Linval Thompson"));
        // `firstLineSongUuid` is present: the bounds are those of the track.
        assert_eq!(m.duration_s, Some(197));
        assert_eq!(d.recontact_at, Duration::from_secs(70));
    }

    #[test]
    fn a_show_carrying_a_track_does_not_take_the_shows_duration() {
        // The defect this split avoids: without `firstLineSongUuid`, the
        // bounds are those of the show (here one hour). Displaying them as
        // the track's duration would give a wrong progress bar.
        let d = parse_direct(MOUV_RESPONSE).unwrap();
        let m = d.meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("La Playlist"));
        assert_eq!(m.artist.as_deref(), Some("OZUNA - Mi yo de antes"));
        assert_eq!(m.duration_s, None, "3600 s is the segment, not the track");
    }

    #[test]
    fn two_identical_lines_are_not_repeated() {
        // Without this, the display would give « Le 18/19, ici Picardie —
        // Le 18/19, ICI Picardie ». The comparison ignores case: the source
        // itself is not consistent about it (« ICI » versus « ici »).
        let m = parse_direct(SILENT_LOCAL_RESPONSE).unwrap().meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Le 18/19, ICI Picardie"));
        assert_eq!(m.artist, None);
    }

    #[test]
    fn the_announced_delay_is_bounded_on_both_sides() {
        // 3,090,000 ms = 51 min: the end of the segment. We come back sooner.
        assert_eq!(parse_direct(MOUV_RESPONSE).unwrap().recontact_at, RECHECK_MAX);
        let short = r#"{"now":{"firstLine":"t"},"delayToRefresh":10}"#;
        assert_eq!(parse_direct(short).unwrap().recontact_at, RECHECK_MIN);
        let missing = r#"{"now":{"firstLine":"t"}}"#;
        assert_eq!(parse_direct(missing).unwrap().recontact_at, RECHECK_DEFAULT);
    }

    #[test]
    fn a_response_without_live_feed_gives_a_delay_without_metadata() {
        let d = parse_direct(r#"{"prev":[],"next":[],"delayToRefresh":20000}"#).unwrap();
        assert!(d.meta.is_none(), "nothing to display");
        assert_eq!(d.recontact_at, Duration::from_secs(20), "but we know when to come back");
    }

    #[test]
    fn accepts_a_partial_response() {
        // Any available information is displayed: partial beats nothing.
        let m = parse_direct(r#"{"now":{"firstLine":"Téléphone"}}"#).unwrap().meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Téléphone"));
        assert_eq!(m.artist, None);
        assert_eq!(m.duration_s, None);
    }

    #[test]
    fn ignores_what_is_not_usable() {
        assert!(parse_direct("").is_none());
        assert!(parse_direct("not json").is_none());
        assert!(parse_direct(r#"{"errCode":"e400","errMessage":"Bad Request"}"#).unwrap().meta.is_none());
        // Neither title nor artist: nothing to display, so not an answer.
        assert!(parse_direct(r#"{"now":{"startTime":1,"endTime":2}}"#).unwrap().meta.is_none());
        assert!(parse_direct(r#"{"now":{"firstLine":"","secondLine":"  "}}"#).unwrap().meta.is_none());
    }

    #[test]
    fn an_absurd_duration_is_ignored_without_losing_the_title() {
        for (start, end) in [(10u64, 10u64), (10, 5), (0, 90_000)] {
            let raw = format!(
                r#"{{"now":{{"firstLine":"t","firstLineSongUuid":"u","startTime":{start},"endTime":{end}}}}}"#
            );
            let m = parse_direct(&raw).unwrap().meta.unwrap();
            assert_eq!(m.duration_s, None, "{start}->{end}");
            assert_eq!(m.title.as_deref(), Some("t"));
        }
    }

    /// Response **captured verbatim** from the FIP Jazz schedule (station
    /// 65), reduced to two items: the one on air and the previous one. At the
    /// same instant the live feed announced `songUuid`
    /// `2edd8576-0344-4cfc-87ea-b7aaca8e3bb2`.
    const SCHEDULE: &str = r#"{"steps":{"a_65":{"uuid":"11111111-1111-1111-1111-111111111111","stepId":"a_65","title":"Halfway to the Hudson","start":1786823637,"end":1786823881,"stationId":65,"embedType":"song","authors":"Lucky Chops","songId":"9648da4b-ec2c-4c1d-a75c-ba88b6e2a5fb","titreAlbum":"Lucky Chops","label":"MELTED"},"b_65":{"uuid":"8c391d63-ff9d-4f2c-9ca9-4290e6ed88e1","stepId":"8917b609-dfeb-48d8-9e26-8fea1c26a5ff_65","title":"Blakey's mood","start":1786825073,"end":1786825386,"stationId":65,"embedType":"song","authors":"Stephane Huchard","anneeEditionMusique":2008,"songId":"2edd8576-0344-4cfc-87ea-b7aaca8e3bb2","titreAlbum":"African tribute to Art Blakey","label":"HARMONIA","releaseId":"1a098645-6c16-4efd-93d3-473a8708379d"}},"levels":[],"stationId":65}"#;

    #[test]
    fn the_album_is_read_from_the_schedule_by_track_identifier() {
        assert_eq!(
            supplement_in_schedule(SCHEDULE, "2edd8576-0344-4cfc-87ea-b7aaca8e3bb2").album.as_deref(),
            Some("African tribute to Art Blakey")
        );
        // The other item of the same schedule, to prove that the selection
        // really is on the identifier and not on the first one found.
        assert_eq!(
            supplement_in_schedule(SCHEDULE, "9648da4b-ec2c-4c1d-a75c-ba88b6e2a5fb").album.as_deref(),
            Some("Lucky Chops")
        );
    }

    #[test]
    fn the_schedule_also_returns_the_year_and_the_youtube_link() {
        // The fixture is a real capture: `anneeEditionMusique` is a **number**
        // (2008) in it, and that is the shape measured on 2026-08-27 on
        // stations 7 and 65. These two fields were read and thrown away.
        let s = supplement_in_schedule(SCHEDULE, "2edd8576-0344-4cfc-87ea-b7aaca8e3bb2");
        assert_eq!(s.album.as_deref(), Some("African tribute to Art Blakey"));
        assert_eq!(s.year, Some(2008));
        // That item has no link: the schedule gives them less often than
        // years (measured: 3 out of 9 versus 9 out of 9).
        assert!(s.links.is_empty());
        assert!(!s.is_empty(), "album and year are enough not to be empty");
    }

    #[test]
    fn the_youtube_link_is_kept_and_validated_on_its_host() {
        // Pattern measured on 2026-08-27 on stations 7 and 65:
        // `https://www.youtube.com/watch?v=...`.
        let with_link = r#"{"steps":{"a":{"songId":"u","titreAlbum":"X",
            "lienYoutube":"https://www.youtube.com/watch?v=zIqlKJj9IlY"}}}"#;
        assert_eq!(
            supplement_in_schedule(with_link, "u").links,
            vec![Link::Youtube { url: "https://www.youtube.com/watch?v=zIqlKJj9IlY".into() }]
        );
        // A link to another host is discarded right here: no point making the
        // core process what it will refuse.
        let elsewhere = r#"{"steps":{"a":{"songId":"u","lienYoutube":"https://evil.example/x"}}}"#;
        assert!(supplement_in_schedule(elsewhere, "u").links.is_empty());
    }

    #[test]
    fn an_aberrant_year_from_the_schedule_is_ignored_without_losing_the_album() {
        let raw = r#"{"steps":{"a":{"songId":"u","titreAlbum":"X","anneeEditionMusique":0}}}"#;
        let s = supplement_in_schedule(raw, "u");
        assert_eq!(s.year, None);
        assert_eq!(s.album.as_deref(), Some("X"), "the album survives");
        // The text form is accepted too: the field comes from a third party
        // that can change without notice, like `durationInSeconds` at OUI FM.
        let text = r#"{"steps":{"a":{"songId":"u","anneeEditionMusique":"1952"}}}"#;
        assert_eq!(supplement_in_schedule(text, "u").year, Some(1952));
    }

    #[test]
    fn a_supplement_not_found_is_empty_and_does_not_panic() {
        assert!(supplement_in_schedule(SCHEDULE, "00000000-0000-0000-0000-000000000000").is_empty());
        assert!(supplement_in_schedule("not json", "u").is_empty());
        assert!(supplement_in_schedule("", "u").is_empty());
    }

    #[test]
    fn the_match_is_on_songid_and_not_on_uuid() {
        // `uuid` identifies the schedule item, `songId` the track — and
        // `songId` is what the live feed returns. Confusing them would never
        // find anything, silently.
        assert!(supplement_in_schedule(SCHEDULE, "8c391d63-ff9d-4f2c-9ca9-4290e6ed88e1").album.is_none());
    }

    #[test]
    fn a_schedule_that_does_not_know_the_track_gives_no_album() {
        // The most common case: the schedule is one track behind.
        assert!(supplement_in_schedule(SCHEDULE, "00000000-0000-0000-0000-000000000000").album.is_none());
        assert!(supplement_in_schedule("", "whatever").album.is_none());
        assert!(supplement_in_schedule("not json", "whatever").album.is_none());
        assert!(supplement_in_schedule(r#"{"stationId":65}"#, "whatever").album.is_none());
        // Item found but without album: nothing to say either.
        let without = r#"{"steps":{"x":{"songId":"u","titreAlbum":"  "}}}"#;
        assert!(supplement_in_schedule(without, "u").album.is_none());
    }

    #[test]
    fn the_live_feed_exposes_the_track_identifier_for_the_album_lookup() {
        let d = parse_direct(FIP_RESPONSE).unwrap();
        assert_eq!(d.song_uuid.as_deref(), Some("1691b015-c8b9-48d2-a296-1f846e13af7b"));
        // Outside a track, there is nothing to look up.
        assert!(parse_direct(SILENT_LOCAL_RESPONSE).unwrap().song_uuid.is_none());
    }

    #[test]
    fn the_live_feed_never_carries_an_album_itself() {
        // Guard rail: if the endpoint started giving one, the schedule would
        // no longer be the sole source and this test would flag it.
        assert_eq!(parse_direct(FIP_RESPONSE).unwrap().meta.unwrap().album, None);
    }

    #[test]
    fn the_schedule_url_carries_the_identifier() {
        assert_eq!(schedule_url(65), "https://api.radiofrance.fr/livemeta/pull/65");
    }

    #[test]
    fn the_live_url_carries_the_identifier_and_the_profile() {
        assert_eq!(
            live_url(7, "webrf_fip_player"),
            "https://api.radiofrance.fr/livemeta/live/7/webrf_fip_player"
        );
        assert_eq!(
            live_url(6, "webrf_mouv_player"),
            "https://api.radiofrance.fr/livemeta/live/6/webrf_mouv_player"
        );
    }

    /// `startTime` is kept **raw**: the elapsed time is derived from it at
    /// the moment the enrichment is emitted, not when the response is
    /// parsed — parsing stays pure, clock-free, like the whole module.
    #[test]
    fn the_live_feed_keeps_the_track_start() {
        let m = parse_direct(FIP_RESPONSE).unwrap().meta.unwrap();
        assert_eq!(m.start_time, Some(1786722565));
        assert_eq!(m.duration_s, Some(197));
    }

    /// Same filter as the duration: without `firstLineSongUuid`, the bounds
    /// are those of an air segment and not of a track. Deriving a position
    /// from them would display a wrong progress bar — measured at one hour on
    /// Mouv'.
    #[test]
    fn an_air_segment_gives_no_track_start() {
        let m = parse_direct(MOUV_RESPONSE).unwrap().meta.unwrap();
        assert_eq!(m.start_time, None);
        assert_eq!(m.duration_s, None);
    }

    #[test]
    fn the_cover_url_follows_the_measured_pattern() {
        // Measurement of 2026-08-24: this pattern returns a 301 to the CDN,
        // then a 31,887-byte JPEG. `preset` is mandatory — without it, 400.
        assert_eq!(
            cover_url("24abdb92-7220-45c6-8434-a325278efa2b"),
            "https://api.radiofrance.fr/v1/services/embed/image/24abdb92-7220-45c6-8434-a325278efa2b?preset=400x400"
        );
    }

    #[test]
    fn the_cover_of_a_real_track_is_kept() {
        let d = parse_direct(FIP_RESPONSE).unwrap();
        assert_eq!(d.cover.as_deref(), Some("5b93ce44-3ed6-4409-a2d7-4bd159c061f8"));
    }

    #[test]
    fn the_cover_is_dropped_when_it_is_not_a_track() {
        // The station serves a generic image for « Le direct » and for its
        // shows. Announcing it would silence the generic fallback: a filled
        // field is a filled field, no upper layer can know that it is filled
        // wrongly. The criterion is `songUuid`, already extracted.
        let d = parse_direct(SILENT_LOCAL_RESPONSE).unwrap();
        assert_eq!(d.song_uuid, None, "precondition of the test");
        // Precondition, not a proof of the rule: SILENT_LOCAL_RESPONSE
        // carries no "cover" key at all, so this assertion would pass even
        // without the songUuid filter. It is the « Le direct » entry below,
        // with a filled cover next to a null songUuid, that actually
        // exercises the rule.
        assert_eq!(d.cover, None);

        // A « Le direct » entry: null songUuid next to a filled cover.
        // Values taken from the `prev` entry of FIP_RESPONSE, captured above:
        // this is not made up, it is the very shape the live feed actually
        // serves for the generic channel.
        let direct = r#"{"now":{"firstLine":"Le direct","secondLine":"La radio la plus eclectique du monde","songUuid":null,"cover":"7eee98cb-3f59-4a3b-b921-6a4be85af542"},"delayToRefresh":70000}"#;
        assert_eq!(parse_direct(direct).unwrap().cover, None);
    }

    #[test]
    fn the_backoff_grows_up_to_the_cap_and_never_beyond() {
        let mut backoff = BACKOFF_BASE;
        let mut seen = vec![backoff];
        for _ in 0..10 {
            backoff = next_backoff(backoff);
            seen.push(backoff);
        }
        assert_eq!(seen[1], Duration::from_secs(4));
        assert_eq!(seen[2], Duration::from_secs(8));
        assert_eq!(*seen.last().unwrap(), BACKOFF_MAX, "the cap must be reached");
        assert!(seen.windows(2).all(|p| p[1] >= p[0]), "never decreasing");
    }
}
