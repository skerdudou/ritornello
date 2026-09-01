use anyhow::{bail, Context, Result};
use serde_json::Value;

/// What a recognized disc tells us: the artist, the album, and the titles in
/// track order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscInfo {
    pub artist: String,
    pub album: String,
    pub tracks: Vec<String>,
    /// Release year of the disc.
    ///
    /// The year of the **album's first release**
    /// (`release-group.first-release-date`), not that of the recognized
    /// pressing (`release.date`), which only serves as a fallback: a 1987
    /// repress displayed 1987 for a 1959 disc, whereas it is the disc's year
    /// a listener is looking for.
    ///
    /// Measured: this field is sometimes `"1987"`, sometimes `"2017-06-23"`,
    /// hence the pass through `valid_year`, which only keeps the numeric head.
    pub year: Option<u16>,
    /// Cover URL, **already resolved**: that of the recognized pressing when
    /// it announces a front cover, that of the album (`release-group`)
    /// otherwise.
    ///
    /// A URL and not an MBID, because the response carries what is needed to
    /// choose between two levels, and that choice is made here, once, at the
    /// place that sees the `cover-art-archive`. An MBID would force the caller
    /// to decide again — and it would no longer have the information to do so.
    ///
    /// `None` means **"do not ask for an image"**, not "unknown disc": that
    /// case only remains if the response denies the front cover *and* carries
    /// no release-group.
    pub cover_url: Option<String>,
}

/// Puts the raw TOC (`NTRACKS OFF1 … OFFN LEADOUT`, as the cd plugin places
/// it in the identity) into the format MusicBrainz expects:
/// `1+NTRACKS+LEADOUT+OFF1+…+OFFN`.
///
/// This conversion lives here, with the only code that knows MusicBrainz: the
/// cd plugin describes a disc, it has no business knowing the request format
/// of one particular metadata provider.
///
/// Validation is redone in full, without assuming the emitter did its job:
/// the identity comes from another process, in an opaque JSON the core does
/// not re-read.
pub fn mb_toc_param(raw: &str) -> Result<String> {
    let nums: Vec<u64> = raw
        .split_whitespace()
        .map(|s| s.parse::<u64>())
        .collect::<Result<_, _>>()
        .context("non-numeric TOC")?;
    if nums.len() < 3 {
        bail!("TOC too short: {raw:?}");
    }
    let ntracks = nums[0] as usize;
    if nums.len() != ntracks + 2 {
        bail!("inconsistent TOC ({} fields for {} tracks)", nums.len(), ntracks);
    }
    let leadout = nums[nums.len() - 1];
    let offsets: Vec<String> = nums[1..nums.len() - 1].iter().map(u64::to_string).collect();
    Ok(format!("1+{}+{}+{}", ntracks, leadout, offsets.join("+")))
}

/// What the `cover-art-archive` block of a release says about its front cover.
///
/// Three states and not a boolean: the absence of the block ("this response
/// says nothing") must not be confused with `Absent` ("the archive asserts
/// there is none"). Confusing the two would silence the cover on every
/// response that does not carry the block, which would be a silent regression
/// for zero gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrontCover {
    /// The archive announces a typed front cover: `/front-500` serves it.
    Present,
    /// No typed front cover for this pressing. Measured on 2026-08-26 on
    /// `82ebb36b-0a0f-3608-9c7d-743d9003fbf8`: four images (a back, a
    /// booklet, a disc face, and one with no type at all) for a `front: false`,
    /// and `/front-500` does return 404 there — the endpoint follows the
    /// **typing**, not the presence of images. The fallback is the album's
    /// cover, never a guessed image: see [`caa_group_url`].
    Absent,
    /// The block is not in the response: we do not know.
    Unknown,
}

/// Reads the `cover-art-archive` block of a release.
///
/// `darkened` counts as `Absent`: the archive is then hiding the images for
/// legal reasons, and asking for them returns nothing.
fn front_cover(release: &Value) -> FrontCover {
    let Some(caa) = release.get("cover-art-archive").and_then(Value::as_object) else {
        return FrontCover::Unknown;
    };
    let Some(front) = caa.get("front").and_then(Value::as_bool) else {
        return FrontCover::Unknown;
    };
    let darkened = caa.get("darkened").and_then(Value::as_bool).unwrap_or(false);
    if front && !darkened {
        FrontCover::Present
    } else {
        FrontCover::Absent
    }
}

/// Preference between two candidates, from best to worst. Smaller = better.
fn rank(face: FrontCover) -> u8 {
    match face {
        FrontCover::Present => 0,
        // Before `Absent`: without the block we do not know, and optimism is
        // the historical behavior — at worst a 404 the core swallows.
        FrontCover::Unknown => 1,
        // Ranked last, but not lost for all that: it falls back on the
        // album's cover (see `extract`).
        FrontCover::Absent => 2,
    }
}

/// Extracts artist / album / titles from a release **whose track count
/// matches**. `None` if it does not match.
fn extract(release: &Value, ntracks: usize) -> Option<DiscInfo> {
    let media = release.get("media").and_then(Value::as_array)?;
    for m in media {
        let Some(tracks) = m.get("tracks").and_then(Value::as_array) else { continue };
        if tracks.len() != ntracks {
            continue;
        }
        let titles: Vec<String> = tracks
            .iter()
            .filter_map(|t| t.get("title").and_then(Value::as_str).map(String::from))
            .collect();
        if titles.len() != ntracks {
            continue;
        }
        // The level is chosen here, once, at the place that sees the
        // `cover-art-archive`: this very pressing if it has a front cover,
        // the album otherwise.
        let cover_url = match front_cover(release) {
            FrontCover::Present | FrontCover::Unknown => {
                release.get("id").and_then(Value::as_str).map(url_caa)
            }
            FrontCover::Absent => {
                release.pointer("/release-group/id").and_then(Value::as_str).map(caa_group_url)
            }
        };
        return Some(DiscInfo {
            artist: release
                .pointer("/artist-credit/0/name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            album: release.get("title").and_then(Value::as_str).unwrap_or("?").to_string(),
            tracks: titles,
            // The **album's** first release first, the date of this very
            // pressing as fallback. Measured on 2026-08-27 on release
            // e32a3f0b-1c19-3170-bb1c-650893774744: `date` = "1987",
            // `release-group.first-release-date` = "1959-08-17" — a 1987
            // repress therefore displayed 1987 for a 1959 disc, and it is the
            // disc's year a listener is looking for. The lookup already asks
            // for `inc=…+release-groups` (the same block that serves
            // `caa_group_url`), so the field costs no extra request; it
            // remains optional on the MusicBrainz side, hence the fallback.
            //
            // The `and_then(as_str).filter(…)` comes **before** the fallback,
            // and that is the whole point: "present but empty" is not
            // "present and readable". A `first-release-date` of `""` (or
            // `null`) passed the `or_else` since the key existed, then failed
            // on `valid_year` — and the pressing's year was lost even though
            // it was right there, next to it. An unreadable field must lead
            // back to the same case as an absent field.
            year: release
                .pointer("/release-group/first-release-date")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .or_else(|| release.get("date").and_then(Value::as_str))
                .and_then(ritornello_proto::valid_year),
            cover_url,
        });
    }
    None
}

/// Searches the releases for a medium whose track count matches the inserted
/// disc, and extracts artist / album / titles from it.
///
/// **The track count remains the only filter**; among the candidates that
/// pass it, the presence of a front cover breaks the tie. Measured on
/// 2026-08-26 on the lookup this module builds: 25 releases returned, 10 with
/// a front cover, and the **first** one — the one the previous version kept —
/// had none. The disc therefore left without an image while an acceptable
/// candidate carried one.
///
/// The text always comes from the chosen release. So does the image,
/// **unless** that release has no front cover: it then comes from the album,
/// hence possibly from another pressing (see [`caa_group_url`]). The
/// compromise is accepted in that direction only — the right cover from
/// another edition beats no cover, whereas the reverse (titles borrowed from
/// another pressing) would display falsehoods.
pub fn parse_lookup(json: &str, ntracks: usize) -> Option<DiscInfo> {
    let v: Value = serde_json::from_str(json).ok()?;
    let releases = v.get("releases")?.as_array()?;
    let mut best: Option<(u8, DiscInfo)> = None;
    for release in releases {
        let Some(info) = extract(release, ntracks) else { continue };
        let r = rank(front_cover(release));
        if r == 0 {
            return Some(info);
        }
        if best.as_ref().is_none_or(|(seen, _)| r < *seen) {
            best = Some((r, info));
        }
    }
    best.map(|(_, info)| info)
}

/// Minimum interval between two requests to MusicBrainz.
///
/// The service asks for one request per second per client, and does not
/// enforce it softly. 1100 ms rather than 1000 so as not to play on the edge:
/// the margin costs a hundred milliseconds on detached tasks nobody waits for.
pub const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1100);

/// Serializes requests and spaces the next one by `MIN_INTERVAL`.
///
/// The lock is **held during the wait**, and that is the very mechanism: two
/// detached tasks started at the same time end up queued instead of
/// machine-gunning. Without it, probing four candidates emitted four requests
/// in the same millisecond, which MusicBrainz refuses with 503s — so a probe
/// that failed for a reason that had nothing to do with the split.
///
/// A struct rather than a bare static: this is what lets a test have its own
/// instance. The static is the layer next door.
pub struct Throttler(tokio::sync::Mutex<Option<tokio::time::Instant>>);

impl Throttler {
    pub fn new() -> Self {
        Self(tokio::sync::Mutex::new(None))
    }

    pub async fn wait(&self) {
        let mut guard = self.0.lock().await;
        if let Some(previous) = *guard {
            let elapsed = previous.elapsed();
            if elapsed < MIN_INTERVAL {
                tokio::time::sleep(MIN_INTERVAL - elapsed).await;
            }
        }
        *guard = Some(tokio::time::Instant::now());
    }
}

/// The process's throttler. Every path of the plugin goes through it — disc,
/// release, recording — because the rate is counted per client, not per
/// feature.
fn throttler() -> &'static Throttler {
    static E: std::sync::OnceLock<Throttler> = std::sync::OnceLock::new();
    E.get_or_init(Throttler::new)
}

/// GET request common to the two MusicBrainz endpoints used here (lookup by
/// TOC, search by artist/album). `Ok(None)` = offline or failed response: both
/// callers treat that as silence, never as an error to propagate.
/// Total number of attempts for one request.
///
/// Three, decided with the owner. MusicBrainz returns 503s under its own load
/// even when its cadence is respected — measured eight times in one working
/// session on 2026-08-27. A single attempt turned each of those hiccups into
/// "this album has no cover", remembered until the plugin restarted.
const ATTEMPTS: u32 = 3;

/// Wait before the second attempt, doubled before the third.
///
/// Same pattern as the progressive backoff of the Radio France and OUI FM
/// plugins. Bounded to three tries because a device that runs for months
/// unattended must not insist indefinitely on a third party: beyond that, the
/// next frame will relaunch the search anyway, the failure no longer being
/// remembered.
const BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(2);

/// Cap applied to a `Retry-After` received from the service.
///
/// The service may ask for an arbitrarily long wait; honoring it would block
/// a task for nothing, whereas the plugin's deferred retry (see
/// `COVER_RETRIES_S` on the `main.rs` side) already covers long outages. Ten
/// seconds bound the wait without betraying the service's intent.
const RETRY_AFTER_MAX: std::time::Duration = std::time::Duration::from_secs(10);

/// The wait the service explicitly asks for, as carried by a `Retry-After`
/// header.
///
/// **MusicBrainz sends `Retry-After` on its 503s**, and ignoring it was
/// rudeness compounded by inefficiency: our fixed backoff (2 s then 4 s) can
/// land right inside the window where it still refuses. Only the
/// seconds form is read — the HTTP-date form exists in the standard but no
/// service used here employs it, and guessing it wrong would be worth less
/// than ignoring it.
///
/// Takes the **raw value** and not the response: the rule is then a pure
/// function, testable without standing up a server or fabricating a
/// `reqwest::Response` — hence without one more test dependency.
fn requested_wait(raw: Option<&str>) -> Option<std::time::Duration> {
    let seconds: u64 = raw?.trim().parse().ok()?;
    Some(std::time::Duration::from_secs(seconds).min(RETRY_AFTER_MAX))
}

/// What a failed attempt asks to wait before the next one, in addition to its
/// message.
struct Failure {
    reason: anyhow::Error,
    /// The wait requested by the service, if it requested one.
    requested: Option<std::time::Duration>,
}

/// One attempt: the request, its status, its body.
async fn attempt(client: &reqwest::Client, url: &str) -> Result<String, Failure> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Failure { reason: anyhow::Error::new(e).context("unreachable"), requested: None })?;
    let status = resp.status();
    if !status.is_success() {
        // **The status is named.** Without this line a 503 left no trace:
        // neither in `/api/logs` nor anywhere else. That silence is what made
        // the outage undiagnosable after the fact.
        let requested = requested_wait(
            resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()),
        );
        return Err(Failure { reason: anyhow::anyhow!("HTTP {status}"), requested });
    }
    resp.text()
        .await
        .map_err(|e| Failure {
            reason: anyhow::Error::new(e).context("response read interrupted"),
            requested: None,
        })
}

/// GET request common to the MusicBrainz entry points used here, with a
/// bounded number of attempts.
///
/// **`Err` means "no response", never "nothing found".** The distinction is
/// the heart of this module: an earlier version returned `Ok(None)` in both
/// cases, and the caller, unable to tell them apart, remembered a transient
/// outage as a definitive absence.
async fn request_text(url: &str) -> Result<String> {
    // Version pulled from Cargo.toml, like the radio plugin's directory: a
    // frozen user-agent would lie at the first version bump.
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "ritornello/",
            env!("CARGO_PKG_VERSION"),
            " (https://github.com/skerdudou/ritornello)"
        ))
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut backoff = BACKOFF_BASE;
    let mut last = None;
    for attempt_no in 1..=ATTEMPTS {
        // Inside the loop: a new attempt is a new request, and must therefore
        // be spaced like the others. The throttler remains the sole guarantor
        // of the cadence promised to the service.
        throttler().wait().await;
        match attempt(&client, url).await {
            Ok(body) => {
                if attempt_no > 1 {
                    tracing::info!("MusicBrainz answered on attempt {attempt_no}");
                }
                return Ok(body);
            }
            Err(failure) => {
                tracing::info!(
                    "MusicBrainz attempt {attempt_no}/{ATTEMPTS}: {}",
                    failure.reason
                );
                if attempt_no < ATTEMPTS {
                    // **The requested wait wins over ours when it is longer.**
                    // Backing off less than the service demands makes it
                    // refuse again: a request lost for us and useless load for
                    // it. When shorter, it does not exempt us from our own
                    // progressive backoff.
                    //
                    // The backoff lives **in this arm** and not after the
                    // `match`: success returns higher up, so there is never a
                    // wait to perform on that path.
                    let delay = failure.requested.map_or(backoff, |d| d.max(backoff));
                    tokio::time::sleep(delay).await;
                    backoff *= 2;
                }
                last = Some(failure.reason);
            }
        }
    }
    Err(last.expect("the loop runs at least once, so an error was retained"))
}

/// MusicBrainz "fuzzy" TOC lookup. `Ok(None)` = not found or offline: the
/// plugin then stays silent, and the display keeps what the Source showed.
pub async fn lookup(toc: &str, ntracks: usize) -> Result<Option<DiscInfo>> {
    let body = request_text(&url_lookup(toc)).await?;
    Ok(parse_lookup(&body, ntracks))
}

/// URL of the lookup by TOC. A separate function, and tested: the `inc`s
/// decide what the response will carry, hence what the parsing will be able
/// to draw from it, and a lost `inc` would translate into a silent loss of
/// function.
///
/// `release-groups` serves the cover fallback when the release has no typed
/// front cover (see [`parse_lookup`]). Measured on 2026-08-26: it is returned
/// on the 25 releases of the response with no extra round trip — this is what
/// makes the fallback free on the MusicBrainz side.
///
/// The TOC is not escaped: it is validated digit by digit upstream
/// (`mb_toc_param`), so it only contains numbers and `+`.
fn url_lookup(toc: &str) -> String {
    format!(
        "https://musicbrainz.org/ws/2/discid/-?toc={toc}&fmt=json&inc=recordings+artist-credits+release-groups"
    )
}

/// URL of a release's front cover.
///
/// `front-500` and not `front`: measured on 2026-08-24, 75,249 bytes versus
/// 2,670,705 for the original — the core caps its download at 2 MiB, so a
/// bare `front` would be refused silently. A 404 is the common case — many
/// releases have no image — and the core handles it silently.
pub fn url_caa(mbid: &str) -> String {
    format!("https://coverartarchive.org/release/{mbid}/front-500")
}

/// URL of the front cover of a **release-group**: the album's cover, taken
/// from one of its pressings.
///
/// This is the fallback when the recognized pressing has no typed front
/// cover. Two reference projects do exactly this, and it is no coincidence:
/// Picard — the MusicBrainz team's tagger — has offered it as an option since
/// its 1.3, and `beets` queries both levels, marking the second as a fallback.
/// Neither of them guesses from an untyped image.
///
/// Measured on 2026-08-26: on the 1997 pressing of *Kind of Blue*, whose
/// response announces `front: false`, this URL returns 200 and a JPEG of
/// 50,220 bytes — a real front cover, where the release URL returns 404.
///
/// The image may come from **another pressing** than the recognized one. This
/// is accepted: for a listening device, it is the album's cover, and the right
/// cover from another edition beats no cover at all.
pub fn caa_group_url(rgid: &str) -> String {
    format!("https://coverartarchive.org/release-group/{rgid}/front-500")
}

/// Escapes a value for the **Lucene phrase** that hosts it.
///
/// Inside a quoted phrase, only the double quote and the backslash are
/// significant to the analyzer: an unescaped quote closes the phrase and what
/// follows becomes syntax.
fn escape_lucene(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Percent-encodes a value: everything that is not "unreserved" in the sense
/// of RFC 3986 goes through it.
///
/// Byte by byte and not character by character: that is the form of a correct
/// percent-encoding for UTF-8, and album titles are full of it.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for o in s.as_bytes() {
        if o.is_ascii_alphanumeric() || matches!(*o, b'-' | b'.' | b'_' | b'~') {
            out.push(*o as char);
        } else {
            out.push_str(&format!("%{o:02X}"));
        }
    }
    out
}

/// Search request for a release by artist and album.
///
/// Both values come from **arbitrary file tags**, hence from input we do not
/// choose: they are escaped for the two stacked languages they cross, Lucene
/// inside the quotes then the URL on top. An earlier version only handled the
/// space and the double quote, on the grounds that the rest does not appear in
/// music metadata — that is false, and the failure was silent: an album
/// containing `#` truncated the request at the fragment, an `&` injected a
/// parameter into it. The host, for its part, cannot change (it is hard-coded
/// below), so the worst case remains a wrong or empty search, never a request
/// elsewhere.
pub fn request_release(artist: &str, album: &str) -> String {
    let escape = |s: &str| percent_encode(&escape_lucene(s));
    format!(
        "https://musicbrainz.org/ws/2/release/?query=artist:%22{}%22%20AND%20release:%22{}%22&fmt=json&limit=1",
        escape(artist),
        escape(album)
    )
}

/// Minimum score of a release search to be believed.
///
/// The MusicBrainz search almost always returns **something** plausible:
/// without a threshold, `first_cover` believed the first result whatever it
/// was, and an album misspelled in a file's tags confidently received a wrong
/// cover. 85 rather than 90 for the release, because the query constrains two
/// fields (artist and album) one of which comes from arbitrary tags: a bit
/// more tolerance than for a recording title, which the station writes with a
/// single hand.
pub const RELEASE_THRESHOLD: u64 = 85;

/// Cover URL for a release coming from a **search**.
///
/// The album level, not the pressing — and that is a choice, not a shortcut.
/// A search response **never** carries a `cover-art-archive` block (measured
/// on 2026-08-26 on both searches: release and recording), so the arbitration
/// of [`parse_lookup`] is impossible here. It is also pointless: these paths
/// search by text, they never aimed at a precise edition. Now the
/// release-group endpoint answers as soon as **one** pressing of the group has
/// a front cover, and the pressing pulled by the search is itself in that
/// group: the album level is therefore strictly more available, never less.
///
/// The fallback on the pressing only serves a response without a
/// release-group. Measured, those do not exist (5/5 and 2/2), but counting on
/// that would be assuming a schema rather than reading it.
fn release_cover(release: &Value) -> Option<String> {
    release
        .pointer("/release-group/id")
        .and_then(Value::as_str)
        .map(caa_group_url)
        .or_else(|| release.get("id").and_then(Value::as_str).map(url_caa))
}

/// Cover of the first result, **if it is confident enough**. `None` = nothing
/// found, unreadable response, or best result too uncertain.
pub fn first_cover(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let first = v.get("releases")?.as_array()?.first()?;
    // Absent score = refusal, and a `warn` rather than a `debug`: this is a
    // field the API always returns, so its absence is a schema change.
    // Refusing keeps correctness (no wrong cover) and the log level makes the
    // failure diagnosable, where assuming "confident enough" would restore
    // the defect without a single line.
    let Some(score) = first.get("score").and_then(Value::as_u64) else {
        tracing::warn!("release search: no score field, refusing rather than guessing");
        return None;
    };
    if score < RELEASE_THRESHOLD {
        tracing::debug!("release search: best match scored {score}, under the {RELEASE_THRESHOLD} needed");
        return None;
    }
    release_cover(first)
}

/// Searches for a release by artist and album, and returns its cover URL.
///
/// This is the generic path (file without cover, radio stream whose textual
/// metadata is enough): unlike the disc path, it holds no TOC and must guess
/// the release from text. `Ok(None)` = nothing found or offline, exactly like
/// [`lookup`]: the plugin stays silent, it knows nothing more than what it was
/// given.
pub async fn search_release(artist: &str, album: &str) -> Result<Option<String>> {
    let url = request_release(artist, album);
    let body = request_text(&url).await?;
    Ok(first_cover(&body))
}

/// Minimum score of a recording search to be believed.
///
/// Higher than `RELEASE_THRESHOLD`: here both constrained fields come from the
/// **same** string written with a single hand by the station, so a true pair
/// gets a clear-cut score. And the validation serves to *choose* between two
/// splits: the higher the threshold, the less chance the reverse order has of
/// sneaking above.
pub const RECORDING_THRESHOLD: u64 = 90;

/// What a recording returned by the search tells us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub score: u64,
    /// The title **as MusicBrainz writes it**. This is what is compared to the
    /// candidate after normalization, and that comparison carries the
    /// validation: the score alone is too generous.
    pub title: String,
    /// Cover URL, taken from the **best ranked** release among the recordings
    /// tied at the top score. See [`cover_rank`].
    ///
    /// An earlier version took the first release of the first recording, and
    /// declined to rank on the grounds that "MusicBrainz does not rank them by
    /// relevance". The premise is right, the conclusion was not: taking the
    /// first one lets an arbitrary order decide, and on a widely covered title
    /// that order falls on a compilation almost every time. **Measured on
    /// 2026-09-01** on `U2 / I Still Haven't Found What I'm Looking For`: 418
    /// recordings match, all those at the top score 100, and the first one
    /// listed only appears on two compilations — the first being German. That
    /// is the cover the device displayed. `The Joshua Tree` came seventh.
    ///
    /// A URL and not an MBID, for the same reason as `DiscInfo::cover_url`:
    /// the level to aim at (album or pressing) is decided here, where the
    /// response is visible, and never at the caller. This is what prevents a
    /// third path from rebuilding a URL blindly — the defect this module
    /// carried on all three of its paths at once.
    pub cover_url: Option<String>,
}

/// Search request for a recording by artist and title.
///
/// Both values come from a **station**, hence from input we do not choose:
/// escaped for the two stacked languages they cross, Lucene then the URL. See
/// the doc of `request_release`, which spells out what an earlier version had
/// missed there.
pub fn request_recording(artist: &str, title: &str) -> String {
    let escape = |s: &str| percent_encode(&escape_lucene(s));
    format!(
        "https://musicbrainz.org/ws/2/recording/?query=artist:%22{}%22%20AND%20recording:%22{}%22&fmt=json&limit={RECORDING_SEARCH_LIMIT}",
        escape(artist),
        escape(title)
    )
}

/// How many recordings the search asks for.
///
/// **One was not enough, and asking for more costs nothing**: it stays a
/// single request, 50 kB of response instead of 3 kB on the measured case.
/// With `limit=1` the cover could only come from whatever recording the API
/// happened to list first — for U2 (see `Recording::cover_url`), one that
/// exists only on compilations. The studio album was seventh, hence out of
/// reach.
const RECORDING_SEARCH_LIMIT: usize = 25;

/// Ranking of a release as a source of cover art, **lower is better**.
///
/// Three criteria, in this order — and the order is the whole point:
///
/// 1. **status**: `Official`, then `Promotion`, then anything unnamed, and
///    `Bootleg` last. A bootleg's artwork is at best a fan montage.
/// 2. **nature**: an `Album` with **no** secondary type — the studio album —
///    then anything with no secondary type (a single, an EP: real artwork for
///    this very song), and everything carrying one (`Compilation`, `Live`,
///    `Soundtrack`, …) last.
/// 3. **date**: the oldest first, the original pressing rather than a
///    remaster. An absent date sorts last (`"9999"`), and ISO-8601 compares
///    correctly as a string.
///
/// The **nature** carries most of the discrimination, and that is not our
/// invention: Picard weights `releasetype` at 14 among its match preferences,
/// against 2 for `format` and 2 for `releasecountry`. beets, for its part,
/// prefers the original year. Neither of the two takes the first release
/// returned.
///
/// The country is deliberately **not** a criterion. It is tempting — the
/// defect that prompted all this displayed a German cover — but it would be
/// the wrong lesson: what makes that release wrong is that it is a
/// *compilation*, not that it is German. Ranking by country would take a
/// preferred-country list, which is a setting and not a heuristic, and it
/// would leave the compilation problem untouched.
fn cover_rank(release: &Value) -> (u8, u8, &str) {
    let status_rank = match release.get("status").and_then(Value::as_str).unwrap_or("") {
        "Official" => 0,
        "Promotion" => 1,
        "Bootleg" | "Pseudo-Release" => 3,
        _ => 2,
    };
    let group = release.get("release-group");
    let primary = group.and_then(|g| g.get("primary-type")).and_then(Value::as_str).unwrap_or("");
    let secondary =
        group.and_then(|g| g.get("secondary-types")).and_then(Value::as_array).map_or(0, Vec::len);
    let kind_rank = match (primary, secondary) {
        ("Album", 0) => 0,
        (_, 0) => 1,
        _ => 2,
    };
    let date =
        release.get("date").and_then(Value::as_str).filter(|d| !d.is_empty()).unwrap_or("9999");
    (status_rank, kind_rank, date)
}

/// What the response says about the searched recording. `None` = nothing,
/// unreadable, or without a score — see `first_cover` for the reasoning about
/// the absent score.
///
/// **The score and the title come from the first recording, the cover does
/// not**, and the two halves answer two different questions. The score and the
/// title serve the *validation* — is this really the track the station
/// announced — where the first answer is the best match, and it is what the
/// caller compares against its candidate. The cover only has to be the right
/// image of the right album; the recording that best matches the *text* is not
/// the one that best carries the *artwork*, U2 above existing only on
/// compilations.
///
/// The cover is therefore chosen among the releases of **all** the recordings
/// tied at the top score, ranked by [`cover_rank`]. "Tied at the top" is read
/// as "scoring like the first one": MusicBrainz sorts its results by
/// decreasing score. Should it ever stop doing so, this filter would merely
/// consider fewer releases — a degradation, never a wrong pick.
pub fn first_recording(json: &str) -> Option<Recording> {
    let v: Value = serde_json::from_str(json).ok()?;
    let recordings = v.get("recordings")?.as_array()?;
    let first = recordings.first()?;
    let Some(score) = first.get("score").and_then(Value::as_u64) else {
        tracing::warn!("recording search: no score field, refusing rather than guessing");
        return None;
    };
    // `min_by` keeps the **first** minimum, so the order the API chose remains
    // the last tiebreaker: two releases equal on all three criteria are
    // separated by nothing of ours, and the pick stays deterministic.
    let cover_url = recordings
        .iter()
        .filter(|r| r.get("score").and_then(Value::as_u64) == Some(score))
        .filter_map(|r| r.get("releases").and_then(Value::as_array))
        .flatten()
        .min_by(|a, b| cover_rank(a).cmp(&cover_rank(b)))
        .and_then(release_cover);
    Some(Recording { score, title: first.get("title")?.as_str()?.to_string(), cover_url })
}

/// Comparable form of a title: lowercase, diacritics removed, and everything
/// that is neither letter nor digit collapsed to a single space.
///
/// **Not** a full Unicode normalization, and that is accepted: a decomposition
/// crate for some sixty Latin characters is not justified in this repository,
/// and a title in a non-Latin script has no diacritic to remove — it goes
/// through this function unchanged, which is exactly the intended behavior.
pub fn normalize(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in s.chars() {
        let c = without_diacritics(c).to_lowercase().next().unwrap_or(c);
        if c.is_alphanumeric() {
            current.push(c);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.join(" ")
}

/// The base Latin character of an accented character, otherwise itself.
///
/// A table rather than an algorithm: it covers French, Spanish, German and
/// Portuguese, which is the actual fleet of a European living-room device.
/// Whatever is not in it passes through unchanged.
fn without_diacritics(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' | 'í' | 'ì' => 'i',
        'ô' | 'ö' | 'ó' | 'õ' | 'ò' => 'o',
        'ù' | 'û' | 'ü' | 'ú' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ÿ' | 'ý' => 'y',
        'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => 'A',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Î' | 'Ï' | 'Í' | 'Ì' => 'I',
        'Ô' | 'Ö' | 'Ó' | 'Õ' | 'Ò' => 'O',
        'Ù' | 'Û' | 'Ü' | 'Ú' => 'U',
        'Ç' => 'C',
        'Ñ' => 'N',
        other => other,
    }
}

/// Searches for a recording, and returns what is known about it. `Ok(None)` =
/// nothing found or offline, as everywhere in this module.
pub async fn search_recording(artist: &str, title: &str) -> Result<Option<Recording>> {
    let url = request_recording(artist, title);
    let body = request_text(&url).await?;
    Ok(first_recording(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_requested_by_the_service_is_read_and_capped() {
        // **Measured on 2026-08-28 on the device**: six 503s out of nine
        // requests in one minute, cadence nonetheless compliant (1.1 s between
        // requests). MusicBrainz sends `Retry-After` on its 503s, and backing
        // off less than it demands makes it refuse again — a request lost for
        // us, useless load for it.
        assert_eq!(requested_wait(Some("3")), Some(std::time::Duration::from_secs(3)));
        assert_eq!(requested_wait(Some("  3  ")), Some(std::time::Duration::from_secs(3)));
        // Capped: an arbitrarily long wait would pin a task for nothing, the
        // plugin's deferred retry already covering outages that last.
        assert_eq!(requested_wait(Some("600")), Some(RETRY_AFTER_MAX));
        // Absent, or in the HTTP-date form the standard allows and no service
        // used here employs: nothing, and the plugin's own backoff applies.
        assert_eq!(requested_wait(None), None);
        assert_eq!(requested_wait(Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
        assert_eq!(requested_wait(Some("")), None);
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/mb_discid.json");

    // The "id" field of this fixture was added by hand for this task, not
    // captured with the rest of the response: it is a valid MBID, but borrowed
    // from another measurement (the Kind of Blue release measured on
    // 2026-08-24 for the front-500 URL, cf. url_caa below), so it is almost
    // certainly not the release this fixture was originally captured against.
    // Of no consequence for the test that uses it
    // (parse_lookup_keeps_the_mbid_for_the_cover): it only checks the shape of
    // the field (36 characters), never its value. Whoever wants to draw a
    // stronger conclusion from this fixture (e.g. check that the MBID does
    // match this precise recording) must recapture it first.

    #[test]
    fn parse_extracts_artist_album_tracks() {
        let info = parse_lookup(FIXTURE, 3).unwrap();
        assert_eq!(info.artist, "Miles Davis");
        assert_eq!(info.album, "Kind of Blue");
        assert_eq!(info.tracks, vec!["So What", "Freddie Freeloader", "Blue in Green"]);
    }

    /// A minimal release, just enough for `extract` to keep it: one medium
    /// with one titled track. `date` is added by the caller, in the form to
    /// be tested.
    fn minimal_release(date: Option<&str>) -> String {
        let date_field = date.map(|d| format!(r#""date":"{d}","#)).unwrap_or_default();
        format!(
            r#"{{"releases":[{{"title":"Kind of Blue",{date_field}"artist-credit":[{{"name":"Miles Davis"}}],"media":[{{"tracks":[{{"title":"So What"}}]}}]}}]}}"#
        )
    }

    #[test]
    fn the_year_comes_from_the_albums_first_release_not_the_pressing() {
        // Measured on 2026-08-27 on release e32a3f0b-1c19-3170-bb1c-650893774744
        // (Kind of Blue): `date` = "1987", `release-group.first-release-date`
        // = "1959-08-17". It is the **disc's** year a listener is looking for,
        // not that of the repress they hold in their hands. The lookup already
        // asks for `inc=...+release-groups`, so the field is there with no
        // extra request (measured: present on the 25 release-groups of the
        // response to the fixtures' toc).
        let json = format!(
            r#"{{"releases":[{{"title":"Kind of Blue","date":"1987","release-group":{{"id":"{}","first-release-date":"1959-08-17"}},"artist-credit":[{{"name":"Miles Davis"}}],"media":[{{"tracks":[{{"title":"So What"}}]}}]}}]}}"#,
            "0b1b0b1b-0b1b-0b1b-0b1b-0b1b0b1b0b1b"
        );
        assert_eq!(parse_lookup(&json, 1).unwrap().year, Some(1959));
    }

    #[test]
    fn an_empty_first_release_lets_the_pressing_date_take_over() {
        // The field present but **empty**, which is not the field absent: an
        // `or_else` placed after the sole `pointer` let the empty string
        // through, which then failed on `valid_year`. The pressing's year was
        // therefore lost even though it was right there, next to it.
        let json = r#"{"releases":[{"title":"Kind of Blue","date":"1987","release-group":{"id":"0b1b0b1b-0b1b-0b1b-0b1b-0b1b0b1b0b1b","first-release-date":""},"artist-credit":[{"name":"Miles Davis"}],"media":[{"tracks":[{"title":"So What"}]}]}]}"#;
        assert_eq!(parse_lookup(json, 1).unwrap().year, Some(1987));
    }

    #[test]
    fn a_first_release_that_is_not_a_string_does_not_mask_the_pressing() {
        // Same family: `null` (or any other form) instead of a date. It is the
        // `and_then(as_str)` placed **before** the fallback that brings it
        // back to the same case as absence.
        let json = r#"{"releases":[{"title":"Kind of Blue","date":"1987","release-group":{"first-release-date":null},"artist-credit":[{"name":"Miles Davis"}],"media":[{"tracks":[{"title":"So What"}]}]}]}"#;
        assert_eq!(parse_lookup(json, 1).unwrap().year, Some(1987));
    }

    #[test]
    fn without_a_first_release_the_pressing_date_takes_over() {
        // A `release-group` present but without `first-release-date` (the
        // field is optional on the MusicBrainz side): the pressing's year
        // beats no year at all.
        let json = r#"{"releases":[{"title":"Kind of Blue","date":"1987","release-group":{"id":"0b1b0b1b-0b1b-0b1b-0b1b-0b1b0b1b0b1b"},"artist-credit":[{"name":"Miles Davis"}],"media":[{"tracks":[{"title":"So What"}]}]}]}"#;
        assert_eq!(parse_lookup(json, 1).unwrap().year, Some(1987));
    }

    #[test]
    fn the_year_comes_from_the_release_date() {
        // No measured fixture carries `date`: without this test, reading the
        // year from the wrong key (or not reading it at all) woke nothing up.
        // Hence a minimal hand-written JSON, rather than a measured fixture
        // that would have to be doctored.
        let info = parse_lookup(&minimal_release(Some("1959-08-17")), 1).unwrap();
        assert_eq!(info.year, Some(1959));
        // The short form is just as common in MusicBrainz.
        let info = parse_lookup(&minimal_release(Some("1959")), 1).unwrap();
        assert_eq!(info.year, Some(1959));
    }

    #[test]
    fn without_a_date_the_release_promises_no_year() {
        // The control: many releases have no `date`, and the rest of the
        // parsing must succeed anyway — an unknown year is not a disc
        // recognition failure.
        let info = parse_lookup(&minimal_release(None), 1).unwrap();
        assert_eq!(info.year, None);
        assert_eq!(info.album, "Kind of Blue");
    }

    #[test]
    fn parse_rejects_if_track_count_inconsistent() {
        assert!(parse_lookup(FIXTURE, 12).is_none());
    }

    #[test]
    fn parse_rejects_empty_or_invalid_json() {
        assert!(parse_lookup("{}", 3).is_none());
        assert!(parse_lookup("not json", 3).is_none());
        assert!(parse_lookup("{\"releases\":[]}", 3).is_none());
    }

    #[test]
    fn musicbrainz_toc_well_formed() {
        // 3 tracks, offsets 150/22767/41887, leadout 63000
        assert_eq!(mb_toc_param("3 150 22767 41887 63000\n").unwrap(), "1+3+63000+150+22767+41887");
    }

    #[test]
    fn invalid_toc_rejected_without_network_call() {
        // The identity comes from another process: a dubious TOC must be
        // refused here, not sent to a third-party service.
        assert!(mb_toc_param("").is_err());
        assert!(mb_toc_param("3 150 22767").is_err());
        assert!(mb_toc_param("abc def").is_err());
    }

    #[test]
    fn the_cover_art_archive_url_asks_for_a_bounded_size() {
        // A bare `front` returns a PNG of 2,670,705 bytes; `front-500`, 75,249.
        assert_eq!(
            url_caa("e32a3f0b-1c19-3170-bb1c-650893774744"),
            "https://coverartarchive.org/release/e32a3f0b-1c19-3170-bb1c-650893774744/front-500"
        );
    }

    /// Reduction of a real capture of the lookup this module builds
    /// (2026-08-26, 25 releases returned). Three candidates kept, in an order
    /// that reproduces the trap exactly: first one with 3 tracks **without** a
    /// front cover — the one the previous version kept — then a decoy that has
    /// a front cover but 11 tracks, then the right one. Each release is
    /// reduced to the fields the parsing reads, plus its `cover-art-archive`
    /// block copied verbatim.
    const COVERS_FIXTURE: &str = include_str!("../tests/fixtures/mb_discid_pochettes.json");

    #[test]
    fn the_front_cover_breaks_the_tie_between_acceptable_candidates() {
        let info = parse_lookup(COVERS_FIXTURE, 3).unwrap();
        // Not the first that fits (Hellfire, `front: false`): the one that
        // has an image. Without this sort, the disc left without a cover while
        // an acceptable candidate carried one.
        assert_eq!(info.album, "Kiss You Off");
        assert_eq!(info.artist, "Scissor Sisters");
        // Front cover announced: the URL aims at the pressing, not the album.
        assert_eq!(
            info.cover_url.as_deref(),
            Some("https://coverartarchive.org/release/2de62a1b-0401-4569-bfe4-7bac2a61dea2/front-500")
        );
        // The text follows the image: both come from the same release, without
        // which the displayed cover would not match the titles.
        assert_eq!(info.tracks[0], "Kiss You Off");
    }

    #[test]
    fn the_track_count_remains_the_filter_and_an_image_does_not_bypass_it() {
        // The fixture's decoy ("Connectivity!") does have a front cover, but
        // 11 tracks. Preferring it would mean announcing another disc.
        let info = parse_lookup(COVERS_FIXTURE, 11).unwrap();
        assert_eq!(info.album, "Connectivity!");
        // And for 3 tracks, it must never come out.
        assert_eq!(parse_lookup(COVERS_FIXTURE, 3).unwrap().album, "Kiss You Off");
        // A track count no candidate carries: nothing.
        assert!(parse_lookup(COVERS_FIXTURE, 7).is_none());
    }

    #[test]
    fn without_a_front_cover_the_albums_cover_takes_over() {
        // Measured on 2026-08-26: `/front-500` on a release whose response
        // says `front: false` returns 404, even with four images — the
        // endpoint follows the typing. The fallback is Picard's and beets':
        // the release-group's cover, which is a real typed front cover.
        let without = r#"{"releases":[
            {"id":"11111111-1111-1111-1111-111111111111","title":"No image","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":false,"count":4,"darkened":false},
             "release-group":{"id":"33333333-3333-3333-3333-333333333333"},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        let info = parse_lookup(without, 1).unwrap();
        assert_eq!(info.album, "No image", "the text always comes from the pressing");
        assert_eq!(
            info.cover_url.as_deref(),
            Some("https://coverartarchive.org/release-group/33333333-3333-3333-3333-333333333333/front-500"),
            "the image, on the other hand, comes from the album"
        );
    }

    #[test]
    fn without_a_front_cover_or_a_release_group_nothing_is_promised() {
        // The only case that stays silent. Announcing the pressing's URL would
        // make the core issue a request already known to return 404.
        let nothing = r#"{"releases":[
            {"id":"11111111-1111-1111-1111-111111111111","title":"No image","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":false,"count":0,"darkened":false},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        let info = parse_lookup(nothing, 1).unwrap();
        assert_eq!(info.album, "No image", "the text remains useful");
        assert_eq!(info.cover_url, None, "nothing to ask the archive for");
    }

    #[test]
    fn a_darkened_release_falls_back_like_a_release_without_a_front_cover() {
        // `darkened`: the archive hides the images for legal reasons. The
        // `front: true` that comes with it then no longer means anything, and
        // asking for the pressing would return nothing.
        let dark = r#"{"releases":[
            {"id":"22222222-2222-2222-2222-222222222222","title":"Hidden","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":true,"count":4,"darkened":true},
             "release-group":{"id":"44444444-4444-4444-4444-444444444444"},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        assert_eq!(
            parse_lookup(dark, 1).unwrap().cover_url.as_deref(),
            Some("https://coverartarchive.org/release-group/44444444-4444-4444-4444-444444444444/front-500")
        );
    }

    #[test]
    fn an_absent_block_does_not_mean_absence_of_cover() {
        // Guard against a silent regression: treating "the response says
        // nothing" as "no image" would silence the cover on every response not
        // carrying this block, for zero gain. The historical fixture has none,
        // and its URL must keep aiming at the pressing — the behavior from
        // before this work.
        assert!(!FIXTURE.contains("cover-art-archive"), "test precondition");
        let url = parse_lookup(FIXTURE, 3).unwrap().cover_url.unwrap();
        assert!(url.starts_with("https://coverartarchive.org/release/"), "{url}");
    }

    #[test]
    fn a_pressings_front_cover_wins_over_the_albums_cover() {
        // Both candidates fit the track count. The first has no front cover
        // but has an album; the second has one. The second is the one wanted,
        // because its image is that of this very pressing.
        let two = r#"{"releases":[
            {"id":"11111111-1111-1111-1111-111111111111","title":"Sans","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":false,"count":0,"darkened":false},
             "release-group":{"id":"33333333-3333-3333-3333-333333333333"},
             "media":[{"tracks":[{"title":"un"}]}]},
            {"id":"22222222-2222-2222-2222-222222222222","title":"Avec","artist-credit":[{"name":"B"}],
             "cover-art-archive":{"front":true,"count":1,"darkened":false},
             "release-group":{"id":"44444444-4444-4444-4444-444444444444"},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        let info = parse_lookup(two, 1).unwrap();
        assert_eq!(info.album, "Avec");
        assert_eq!(
            info.cover_url.as_deref(),
            Some("https://coverartarchive.org/release/22222222-2222-2222-2222-222222222222/front-500")
        );
    }

    #[test]
    fn the_album_url_follows_the_measured_pattern() {
        // Measured on 2026-08-26: 200 and a JPEG of 50,220 bytes on the
        // release-group of "Kind of Blue", where the URL of the 1997 pressing
        // returns 404.
        assert_eq!(
            caa_group_url("8e8a594f-2175-38c7-a871-abb68ec363e7"),
            "https://coverartarchive.org/release-group/8e8a594f-2175-38c7-a871-abb68ec363e7/front-500"
        );
    }

    #[test]
    fn parse_lookup_keeps_the_mbid_for_the_cover() {
        // The MBID is the key to the image, and it was being thrown away. The
        // real fixture in the tests/fixtures directory is `mb_discid.json`
        // (3 tracks).
        let url = parse_lookup(FIXTURE, 3).unwrap().cover_url.expect("a cover URL");
        let mbid = url
            .strip_prefix("https://coverartarchive.org/release/")
            .and_then(|r| r.strip_suffix("/front-500"))
            .unwrap_or("");
        assert_eq!(mbid.len(), 36, "an MBID is 36 characters long, got {url:?}");
    }

    #[test]
    fn the_lookup_asks_for_the_release_group() {
        // Without `release-groups` in the `inc`, the cover fallback has no
        // identifier to aim at and vanishes silently. Measured on 2026-08-26:
        // this parameter is returned on the 25 releases with no extra round
        // trip.
        let url = url_lookup("1+3+63000+150+22767+41887");
        assert!(url.contains("inc=recordings+artist-credits+release-groups"), "{url}");
    }

    #[test]
    fn the_release_request_escapes_quotes() {
        // Measured on 2026-08-24: this request returns "Kind of Blue" at score 100.
        let q = request_release("Miles Davis", "Kind of Blue");
        assert!(q.contains("artist:%22Miles%20Davis%22"), "{q}");
        assert!(q.contains("release:%22Kind%20of%20Blue%22"), "{q}");
        assert!(q.contains("fmt=json"), "{q}");
        assert!(q.contains("limit=1"), "{q}");
    }

    #[test]
    fn the_release_request_survives_hostile_tags() {
        // These values come from arbitrary file tags. With the original
        // minimal escaping, the `#` truncated the request at the fragment and
        // the `&` injected a parameter into it — a wrong or empty search,
        // silently.
        let q = request_release("AC/DC & Co", "Drum #1 = 100%");
        let params: Vec<&str> = q.split('&').collect();
        assert_eq!(params.len(), 3, "no injected parameter: query, fmt, limit — {q}");
        assert!(q.contains("fmt=json"), "{q}");
        assert!(q.contains("limit=1"), "{q}");
        assert!(!q.contains('#'), "a fragment would truncate everything that follows — {q}");
        assert!(q.contains("artist:%22AC%2FDC%20%26%20Co%22"), "{q}");
        assert!(q.contains("release:%22Drum%20%231%20%3D%20100%25%22"), "{q}");

        // Lucene stage: an unescaped quote would close the phrase, and what
        // follows would become syntax.
        let q = request_release("Say \"Yes\"", "a\\b");
        assert!(q.contains("artist:%22Say%20%5C%22Yes%5C%22%22"), "{q}");
        assert!(q.contains("release:%22a%5C%5Cb%22"), "{q}");
    }

    #[test]
    fn percent_encoding_handles_utf8_byte_by_byte() {
        // A non-ASCII character spans several bytes, and each must be
        // encoded: "é" is %C3%A9, never a single %E9.
        assert_eq!(percent_encode("Café"), "Caf%C3%A9");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e", "unreserved characters pass through as is");
    }

    #[test]
    fn the_cover_comes_from_the_first_result() {
        let json = r#"{"count":135,"releases":[
            {"id":"e32a3f0b-1c19-3170-bb1c-650893774744","score":100,
             "release-group":{"id":"8e8a594f-2175-38c7-a871-abb68ec363e7"}},
            {"id":"other"}]}"#;
        assert_eq!(
            first_cover(json).as_deref(),
            Some("https://coverartarchive.org/release-group/8e8a594f-2175-38c7-a871-abb68ec363e7/front-500")
        );
        assert_eq!(first_cover(r#"{"releases":[]}"#), None);
        assert_eq!(first_cover("not json"), None);
    }

    #[test]
    fn a_search_aims_at_the_album_not_the_pressing() {
        // Measured on 2026-08-26: a search response NEVER carries a
        // `cover-art-archive` block (checked on both searches), so the
        // arbitration of `parse_lookup` is impossible here. It is also
        // pointless: the search was by text, no precise pressing was aimed
        // at, and the group endpoint answers as soon as a single one of its
        // pressings has a front cover.
        let json = r#"{"releases":[{"id":"11111111-1111-1111-1111-111111111111","score":100,
            "release-group":{"id":"22222222-2222-2222-2222-222222222222"}}]}"#;
        let url = first_cover(json).unwrap();
        assert!(url.contains("/release-group/22222222"), "{url}");
        assert!(!url.contains("/release/11111111"), "the pressing must not be aimed at — {url}");
    }

    #[test]
    fn without_a_release_group_the_search_falls_back_on_the_pressing() {
        // Measured: 5/5 and 2/2 of the responses carry one. But relying on
        // that would be assuming a schema rather than reading it.
        let json = r#"{"releases":[{"id":"11111111-1111-1111-1111-111111111111","score":100}]}"#;
        assert_eq!(
            first_cover(json).as_deref(),
            Some("https://coverartarchive.org/release/11111111-1111-1111-1111-111111111111/front-500")
        );
    }

    /// Release search response **as MusicBrainz emits it**: the `score` field
    /// is always present, and it is what was being ignored. The
    /// `release-group` is there too — measured present on 5 results out of 5,
    /// without any `inc`.
    fn release_response(score: u64) -> String {
        format!(
            r#"{{"created":"2026-08-26T12:00:00.000Z","count":1,"offset":0,
            "releases":[{{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","score":{score},
            "release-group":{{"id":"ffffffff-eeee-dddd-cccc-bbbbbbbbbbbb"}},
            "title":"Kind of Blue","status":"Official"}}]}}"#
        )
    }

    #[test]
    fn a_confident_enough_release_is_kept() {
        assert_eq!(
            first_cover(&release_response(RELEASE_THRESHOLD)).as_deref(),
            Some("https://coverartarchive.org/release-group/ffffffff-eeee-dddd-cccc-bbbbbbbbbbbb/front-500"),
            "exactly the threshold must pass"
        );
    }

    #[test]
    fn a_too_uncertain_release_is_refused() {
        // The latent defect: today a misspelled album confidently receives a
        // wrong cover, because the search always returns something plausible.
        assert_eq!(first_cover(&release_response(RELEASE_THRESHOLD - 1)), None);
    }

    #[test]
    fn an_absent_score_is_refused_and_not_assumed_good() {
        // A missing score means "I don't know". Assuming it good would bring
        // back the previous defect, silently; assuming it bad cuts the
        // feature, but visibly (see the `warn`).
        let without = r#"{"releases":[{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","title":"X"}]}"#;
        assert_eq!(first_cover(without), None);
    }

    #[test]
    fn a_response_without_release_stays_none() {
        assert_eq!(first_cover(r#"{"releases":[]}"#), None);
        assert_eq!(first_cover("not json"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_throttler_spaces_two_consecutive_requests() {
        // Virtual clock: `sleep` advances time without waiting, so this test
        // lasts a microsecond while testing a 1.1 s interval.
        // The throttler is **built here** and not taken from a static: two
        // tests sharing the instance would pollute each other.
        let e = Throttler::new();
        let start = tokio::time::Instant::now();
        e.wait().await;
        assert_eq!(start.elapsed(), std::time::Duration::ZERO, "the first must not wait");
        e.wait().await;
        assert!(
            start.elapsed() >= MIN_INTERVAL,
            "the second must be spaced by {MIN_INTERVAL:?}, measured {:?}",
            start.elapsed()
        );
    }

    /// Recording search response **as MusicBrainz emits it**: `score`,
    /// `title`, and the releases the cover will come from.
    fn recording_response(score: u64, title: &str, with_release: bool) -> String {
        let releases = if with_release {
            // The nested `release-group` is measured present on 2 releases
            // out of 2 in a real response, without any `inc`.
            r#","releases":[{"id":"11111111-2222-3333-4444-555555555555","title":"Kind of Blue",
              "release-group":{"id":"66666666-7777-8888-9999-aaaaaaaaaaaa"}}]"#
        } else {
            ""
        };
        format!(
            r#"{{"created":"2026-08-26T12:00:00.000Z","count":1,"offset":0,
            "recordings":[{{"id":"99999999-8888-7777-6666-555555555555","score":{score},
            "title":"{title}","length":545000{releases}}}]}}"#
        )
    }

    #[test]
    fn the_recording_request_escapes_both_languages() {
        // Lucene inside the quotes, then the URL on top: the same requirement
        // as `request_release`, for the same reason — these values come from
        // a station, hence from input we do not choose.
        let url = request_recording(r#"AC"DC"#, "Back in Black & Co");
        assert!(url.starts_with("https://musicbrainz.org/ws/2/recording/?query="), "{url}");
        // Only two structural ampersands (before fmt, before limit): the brief
        // expected `== 1`, but the URL always carries `&fmt=json&limit=…` in
        // addition to `?query=`, hence two literal '&' at minimum, never a
        // single one — see the task report for details. The one from the
        // title is percent-encoded (%26) and therefore does not add to the
        // count.
        assert_eq!(
            url.matches('&').count(),
            2,
            "only fmt and limit may introduce an &; none from the title: {url}"
        );
        assert!(url.contains("%5C%22"), "the quote must be escaped twice: {url}");
    }

    #[test]
    fn a_recording_is_read_with_its_score_and_its_release() {
        let e = first_recording(&recording_response(100, "So What", true)).unwrap();
        assert_eq!(e.score, 100);
        assert_eq!(e.title, "So What");
        // The album, not the pressing: a search carries no `cover-art-archive`
        // block, and the group level is strictly more available (see
        // `release_cover`).
        assert_eq!(
            e.cover_url.as_deref(),
            Some("https://coverartarchive.org/release-group/66666666-7777-8888-9999-aaaaaaaaaaaa/front-500")
        );
    }

    #[test]
    fn a_recording_without_release_remains_usable() {
        // The split is secured even without an image: the artist/title pair
        // stands on its own, and the core already handles an absent cover
        // silently.
        let e = first_recording(&recording_response(100, "So What", false)).unwrap();
        assert_eq!(e.cover_url, None);
        assert_eq!(e.title, "So What");
    }

    /// The real MusicBrainz response, reduced to the fields this module reads.
    ///
    /// **Measured on 2026-09-01** with the very query the plugin builds for
    /// `U2 / I Still Haven't Found What I'm Looking For`: 418 recordings
    /// match, and the 25 returned **all score 100** — the score separates
    /// nothing here. Kept: the recording the API lists first (which exists
    /// only on two compilations, the first of them German), a bootleg concert,
    /// the studio album — seventh in the full response, hence unreachable with
    /// `limit=1` — and a compilation of singles.
    const U2_FIXTURE: &str = include_str!("../tests/fixtures/mb_recording_u2.json");

    /// `The Joshua Tree`, the studio album.
    const JOSHUA_TREE_GROUP: &str = "6f3e9fa6-be7a-3de8-a2b2-2072ece8a54d";
    /// `Super Power: Die heißen Hits`, the German compilation the device showed.
    const GERMAN_COMPILATION_GROUP: &str = "25e45f24-78fa-4beb-bcbb-c9a2c9bfbc9b";

    #[test]
    fn the_cover_of_a_much_covered_title_comes_from_the_album_not_from_a_compilation() {
        // **The defect itself, verbatim.** The owner saw a German cover under
        // a U2 track; this fixture is the response that produced it.
        let e = first_recording(U2_FIXTURE).expect("a readable response");
        let url = e.cover_url.expect("a cover URL");
        assert!(url.contains(JOSHUA_TREE_GROUP), "the album's cover was expected, got {url}");
        assert!(!url.contains(GERMAN_COMPILATION_GROUP), "the compilation won again: {url}");
    }

    #[test]
    fn the_score_and_the_title_keep_coming_from_the_best_match() {
        // The other half of the contract: taking the cover elsewhere must not
        // move what carries the *validation*. The fixture spells the title two
        // ways (straight and curly apostrophes) — `normalize` absorbs that,
        // but what is returned must remain the first recording's.
        let e = first_recording(U2_FIXTURE).expect("a readable response");
        assert_eq!(e.score, 100);
        assert_eq!(e.title, "I Still Haven't Found What I'm Looking For");
    }

    #[test]
    fn a_lower_scored_recording_never_lends_its_cover() {
        // The score first, the ranking only afterwards: an official studio
        // album hanging off a badly matched recording must not beat the
        // compilation of the top-scoring one. Otherwise the image would no
        // longer be that of the track at all — a worse defect than the one
        // being fixed here.
        let json = r#"{"recordings":[
            {"score":100,"title":"So What","releases":[
              {"id":"aaaaaaaa-0000-0000-0000-000000000001","status":"Official",
               "release-group":{"id":"11111111-0000-0000-0000-000000000001",
                                "primary-type":"Album","secondary-types":["Compilation"]}}]},
            {"score":70,"title":"So What","releases":[
              {"id":"aaaaaaaa-0000-0000-0000-000000000002","status":"Official",
               "release-group":{"id":"22222222-0000-0000-0000-000000000002",
                                "primary-type":"Album"}}]}]}"#;
        let url = first_recording(json).unwrap().cover_url.expect("a cover URL");
        assert!(
            url.contains("11111111-0000-0000-0000-000000000001"),
            "the top score must keep its cover: {url}"
        );
    }

    /// A release reduced to what [`cover_rank`] reads. Empty string = absent
    /// field, which is a case of its own for both the date and the secondary
    /// types.
    fn ranked_release(status: &str, primary: &str, secondary: &str, date: &str) -> Value {
        let secondary = if secondary.is_empty() {
            String::new()
        } else {
            format!(r#","secondary-types":["{secondary}"]"#)
        };
        let date = if date.is_empty() { String::new() } else { format!(r#","date":"{date}""#) };
        serde_json::from_str(&format!(
            r#"{{"id":"x","status":"{status}"{date},
                 "release-group":{{"id":"g","primary-type":"{primary}"{secondary}}}}}"#
        ))
        .expect("a valid fixture")
    }

    #[test]
    fn the_ranking_puts_the_official_studio_album_first_and_the_bootleg_last() {
        let album = ranked_release("Official", "Album", "", "1987");
        let remaster = ranked_release("Official", "Album", "", "2017");
        let compilation = ranked_release("Official", "Album", "Compilation", "1987");
        let single = ranked_release("Official", "Single", "", "1987");
        let bootleg = ranked_release("Bootleg", "Album", "", "1987");
        let undated = ranked_release("Official", "Album", "", "");
        assert!(cover_rank(&album) < cover_rank(&single), "the album beats the single");
        assert!(cover_rank(&single) < cover_rank(&compilation), "the single beats the compilation");
        assert!(
            cover_rank(&compilation) < cover_rank(&bootleg),
            "anything official beats a bootleg, even a compilation"
        );
        assert!(cover_rank(&album) < cover_rank(&remaster), "the oldest pressing wins");
        assert!(cover_rank(&album) < cover_rank(&undated), "an absent date sorts last");
    }

    #[test]
    fn an_unreadable_or_empty_response_returns_none() {
        assert!(first_recording(r#"{"recordings":[]}"#).is_none());
        assert!(first_recording("not json").is_none());
        // Absent score: refusal, as for the release.
        assert!(first_recording(r#"{"recordings":[{"id":"x","title":"y"}]}"#).is_none());
    }

    #[test]
    fn normalization_makes_two_spellings_of_the_same_title_comparable() {
        assert_eq!(normalize("So What"), normalize("so  what"));
        assert_eq!(normalize("Où es-tu ?"), normalize("ou es tu"));
        assert_eq!(normalize("Café/Crème"), normalize("cafe creme"));
    }

    #[test]
    fn normalization_does_not_confuse_two_different_titles() {
        // The control: an overly aggressive normalization would accept
        // anything, and the validation would no longer validate anything.
        assert_ne!(normalize("So What"), normalize("So What Else"));
        assert_ne!(normalize("Naima"), normalize("Nauma"));
    }
}
