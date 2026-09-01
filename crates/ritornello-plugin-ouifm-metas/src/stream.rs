//! Consuming the OUI FM metadata `text/event-stream`.
//!
//! Splitting and parsing are pure functions, tested on real frames; only
//! `follows` touches the network, and **no test calls it**.

use anyhow::{bail, Result};
use futures::StreamExt;
use ritornello_proto::Link;
use std::time::Duration;
use tokio::sync::mpsc;

/// Image host, in bare form (no scheme): it is the **authority** of the URL
/// that is compared against this value below, never a prefix of the whole
/// string. A `starts_with` on `"https://{IMAGE_HOST}"` would let
/// `https://www.lesindesradios.fr.evil.example/x.jpg` through — that fake
/// host does have the real domain as a string prefix without being a
/// subdomain of it. `coverUrl` is a field written by a third party, in a
/// stream the device will then go fetch: this is the only barrier against
/// that hijack, the core only validating an https scheme and the absence of
/// a literal IP.
const IMAGE_HOST: &str = "www.lesindesradios.fr";

/// What a frame tells us about the track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub duration_s: Option<u32>,
    /// Final cover URL, already composed: the frame's `coverUrl` if it comes
    /// from the known host, otherwise `coverId` recomposed following the
    /// pattern of OUI FM's own player.
    pub cover: Option<String>,
    /// The listening platforms, composed from the frame's identifiers. See
    /// [`links`].
    pub links: Vec<Link>,
}

/// Initial wait before reconnecting, then doubled on each failure.
const BACKOFF_BASE: Duration = Duration::from_secs(2);

/// Backoff cap. A device that runs unattended for months must not hammer a
/// third party's server; conversely, capping avoids an overnight network
/// outage turning into hours of waiting once it comes back.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Duration beyond which a connection is deemed healthy, hence its drop
/// accidental: the backoff then restarts from scratch.
///
/// The criterion is the **duration** and not the number of frames received.
/// The server pushes one as soon as the connection opens, so "at least one
/// frame" is always true and does not distinguish a four-hour listen from an
/// immediate close. With that criterion, the backoff was reset to 2 s before
/// every wait, the cap was unreachable, and a server that pushes then closes
/// right away — plausible on a private endpoint that grew an anti-abuse
/// protection — would have opened 43,000 requests per day at a third party.
const HEALTHY_DURATION: Duration = Duration::from_secs(60);

/// Silence duration after which we close and reopen.
///
/// `reqwest` detects a vanished peer in about a minute (TCP keepalive), but
/// not a peer that is alive and mute — a frozen proxy would hold the
/// connection indefinitely without sending anything, and the display would
/// stay frozen with it. Ten minutes let the longest of tracks pass without a
/// pointless reconnection.
const MAX_SILENCE: Duration = Duration::from_secs(600);

/// Extracts the complete lines from the buffer, leaving the remainder in it.
///
/// The buffer holds **bytes** and not text: an HTTP chunk can cut in the
/// middle of an accented character, and decoding each chunk separately would
/// replace the « é » of an artist name with a replacement character. Here,
/// only a complete line is decoded.
pub fn split_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(i) = buffer.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buffer.drain(..=i).collect();
        // A badly encoded line is replaced, not thrown away: a dubious
        // character beats a missing title.
        lines.push(String::from_utf8_lossy(&line[..line.len() - 1]).trim_end().to_string());
    }
    lines
}

/// Composes the platform links from the frame's identifiers.
///
/// The stream does not give URLs but **identifiers** (`deezerId`,
/// `appleMusicId`), which therefore have to be shaped. Both patterns were
/// measured on 2026-08-27 on the identifiers of an actually captured frame:
/// Deezer returns 200 then redirects to `/fr/track/…`, and Apple Music
/// returns 301 to `…/song/shes-a-rainbow/1443171670`, i.e. 200 when
/// followed — the *slug* confirms in passing that the identifier really
/// designates the track the frame announced.
///
/// **No storefront in the Apple Music URL**: the form
/// `music.apple.com/song/{id}`, measured on 2026-08-27, redirects by itself
/// to the listener's own. Writing `/us/` pinned the American storefront for a
/// device that listens to nothing there, while nothing in the identifier is
/// American. (The measurement from here still lands on `/us/`, Apple taking
/// neither the IP nor `Accept-Language` into account over bare HTTP; it is a
/// browser with its account that gets the right one, and it can only do so if
/// we do not force it on it.)
///
/// An identifier that is not made only of digits is refused: it goes into a
/// URL the UI will make clickable, and nothing forces a third party to write
/// what we expect. `Link::validated` re-locks the host on the core side, but
/// better not to fabricate a dubious URL here just to have it refused there.
/// A JSON **float** (`9956167.0`) is in that lot: deciding that `.0` can be
/// dropped would make us the author of an identifier the third party did not
/// write, and a link to the wrong track goes unnoticed, whereas a missing
/// link is seen and fixed.
pub fn links(v: &serde_json::Value) -> Vec<Link> {
    let identifier = |key: &str| -> Option<String> {
        let raw = match v.get(key)? {
            serde_json::Value::String(s) => s.trim().to_string(),
            // `is_u64` rather than any number: this is what explicitly rules
            // out the float and the negative, instead of counting on the
            // digit filter below to do it as a side effect.
            serde_json::Value::Number(n) if n.is_u64() => n.to_string(),
            _ => return None,
        };
        (!raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit())).then_some(raw)
    };
    let mut out = Vec::new();
    if let Some(id) = identifier("deezerId") {
        out.push(Link::Deezer { url: format!("https://www.deezer.com/track/{id}") });
    }
    if let Some(id) = identifier("appleMusicId") {
        out.push(Link::AppleMusic { url: format!("https://music.apple.com/song/{id}") });
    }
    out
}

/// Parses one line of the stream. `None` for anything that is not a usable
/// metadata frame: comment lines (`:`), `event:`/`id:` fields, unreadable
/// JSON, or a frame with neither artist **nor** title.
///
/// The field names are those measured on the real stream: `artist` and
/// `title` already split (unlike ICY, which delivers a single string), plus
/// `durationInSeconds`.
pub fn parse_data_line(line: &str) -> Option<Meta> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    fn text(v: &serde_json::Value, key: &str) -> Option<String> {
        let s = v.get(key)?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    }
    // `durationInSeconds` arrives as a **string** on the real stream
    // (`"216"`), not as a number. Measured: reading only numbers silently
    // lost the duration on every track. Both forms are accepted, a third
    // party being free to change its mind without notice.
    let duration = v.get("durationInSeconds").and_then(|d| match d {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    });
    // OUI FM's player does exactly this: `coverUrl` if it is there, otherwise
    // a composed `coverId`. Both cases are real on the stream.
    let cover = text(&v, "coverUrl")
        .filter(|u| {
            // Authority comparison, not a string prefix (see IMAGE_HOST):
            // otherwise "https://www.lesindesradios.fr.evil.example/x" would
            // be accepted, the real domain being just a prefix of the fake.
            u.strip_prefix("https://").and_then(|rest| rest.split(['/', '?', '#']).next()) == Some(IMAGE_HOST)
        })
        .or_else(|| {
            text(&v, "coverId")
                .map(|id| format!("https://{IMAGE_HOST}/servicesimb/images?version=6&iid={id}&width=400"))
        });
    let meta = Meta {
        artist: text(&v, "artist"),
        title: text(&v, "title"),
        // An absurd duration is better ignored than displayed: it comes from
        // a third party.
        duration_s: duration.filter(|d| *d > 0 && *d <= 24 * 3600).map(|d| d as u32),
        cover,
        links: links(&v),
    };
    // A duration alone is not displayable: it is not an answer.
    (meta.artist.is_some() || meta.title.is_some()).then_some(meta)
}

/// Metadata stream URL of a webradio.
fn metas_url(id: &str) -> String {
    format!("https://www.ouifm.fr/ws/metas?id={id}")
}

/// Opens the stream and pushes every received frame. Returns the number of
/// frames read before the end (0 = the connection gave nothing).
async fn listen(id: &str, tx: &mpsc::Sender<(String, Meta)>) -> Result<usize> {
    let client = reqwest::Client::builder()
        .user_agent("ritornello/0.1 (https://github.com/skerdudou/ritornello)")
        // **Connection** timeout only: the stream itself must stay open
        // indefinitely, so no global timeout.
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let resp = client.get(metas_url(id)).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let mut bytes = resp.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut received = 0usize;
    loop {
        let Ok(next) = tokio::time::timeout(MAX_SILENCE, bytes.next()).await else {
            bail!("no data for {} s", MAX_SILENCE.as_secs());
        };
        // Clean end of stream: the server closed.
        let Some(chunk) = next else { break };
        buffer.extend_from_slice(&chunk?);
        // Guard rail: a server that never sends a line ending must not grow
        // this buffer without limit on a 1 GB device.
        if buffer.len() > 64 * 1024 {
            bail!("stream with no line ending, buffer dropped");
        }
        for line in split_lines(&mut buffer) {
            if let Some(meta) = parse_data_line(&line) {
                received += 1;
                if tx.send((id.to_string(), meta)).await.is_err() {
                    // The plugin no longer listens to us: the station changed.
                    return Ok(received);
                }
            }
        }
    }
    Ok(received)
}

/// Next wait before reconnecting, given the current one and how long the
/// connection that just broke lasted.
///
/// A connection that held is deemed healthy: its drop is accidental, we
/// restart quickly (otherwise a listen of several hours would end up waiting
/// a minute after every hiccup). A connection that breaks right away grows
/// the backoff up to the cap.
pub fn next_backoff(backoff: Duration, duration: Duration) -> Duration {
    if duration >= HEALTHY_DURATION {
        BACKOFF_BASE
    } else {
        (backoff * 2).min(BACKOFF_MAX)
    }
}

/// Follows a webradio until the task is aborted: opens the stream, re-reads
/// it after a drop with a progressive backoff.
///
/// Never returns. The caller stops this task (`abort`) when what is playing
/// changes — hence the tagging of each frame with the `id`: a frame already
/// queued at the moment of the stop must be discardable.
pub async fn follows(id: String, tx: mpsc::Sender<(String, Meta)>) {
    // Half the base, because the backoff is recomputed before each wait (see
    // below): the first immediate failure doubles this value and thus waits
    // exactly `BACKOFF_BASE`, as before.
    let mut backoff = BACKOFF_BASE / 2;
    loop {
        let start = tokio::time::Instant::now();
        let result = listen(&id, &tx).await;
        let duration = start.elapsed();
        // Every closure is logged, including one that served frames: without
        // that, a reconnection loop would leave no trace in `/api/logs` and
        // nobody would ever see anything.
        match result {
            Ok(received) => {
                tracing::info!("metadata stream closed after {received} frame(s) and {} s", duration.as_secs())
            }
            Err(e) => {
                tracing::info!("metadata stream interrupted after {} s: {e}", duration.as_secs())
            }
        }
        // The backoff is recomputed **before** sleeping: the drop that just
        // happened says everything there is to know (a connection that held
        // brings it back to the base). The reverse order applied the old
        // backoff one time too many — after a burst of failures then four
        // hours of healthy listening, the first drop still waited the stale
        // 60 s — which is precisely the case the doc of `next_backoff`
        // promises to avoid.
        backoff = next_backoff(backoff, duration);
        tokio::time::sleep(backoff).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame **captured verbatim** from the OUI FM Classic Rock stream, cover
    /// token included. Note: `durationInSeconds` is a **string** in it.
    const FRAME: &str = r#"data: {"coverId":"3134161803443976427/t/th/therollingstones/shesarainbow/214198016_1702973462000","durationInSeconds":"245","artist":"THE ROLLING STONES","deezerId":"9956167","origin":"mds","appleMusicId":"1443171670","custom":"true","mdsId":"3134161803443976427","title":"SHE'S A RAINBOW","type":"song"}"#;

    #[test]
    fn parses_a_real_frame() {
        let m = parse_data_line(FRAME).unwrap();
        assert_eq!(m.artist.as_deref(), Some("THE ROLLING STONES"));
        assert_eq!(m.title.as_deref(), Some("SHE'S A RAINBOW"));
        // The duration was lost before the fix: the stream gives it as a string.
        assert_eq!(m.duration_s, Some(245));
    }

    #[test]
    fn the_cover_id_is_composed_following_the_players_pattern() {
        // Pattern taken from the `_app` bundle of ouifm.fr/player, in the
        // code that reads this very SSE stream. Measurement of 2026-08-24:
        // 35,613-byte JPEG.
        let m = parse_data_line(FRAME).unwrap();
        assert_eq!(
            m.cover.as_deref(),
            Some("https://www.lesindesradios.fr/servicesimb/images?version=6&iid=3134161803443976427/t/th/therollingstones/shesarainbow/214198016_1702973462000&width=400")
        );
    }

    #[test]
    fn a_ready_made_url_in_the_frame_is_preferred_if_the_host_is_known() {
        let known = r#"data: {"title":"t","coverUrl":"https://www.lesindesradios.fr/x.jpg","coverId":"abc"}"#;
        assert_eq!(
            parse_data_line(known).unwrap().cover.as_deref(),
            Some("https://www.lesindesradios.fr/x.jpg")
        );
        // An unknown host is refused: this field is written by a third party,
        // and the core is what would go fetch it.
        let unknown = r#"data: {"title":"t","coverUrl":"https://elsewhere.example/x.jpg","coverId":"abc"}"#;
        let composed = parse_data_line(unknown).unwrap().cover.unwrap();
        assert!(composed.starts_with("https://www.lesindesradios.fr/"), "{composed}");
        assert!(composed.contains("iid=abc"), "{composed}");

        // The real domain as a mere string prefix of a different host: a
        // `starts_with` on the whole string would wrongly accept it, whereas
        // a comparison on the authority refuses it. This is the bypass that
        // `IMAGE_HOST` exists to close.
        let spoofed = r#"data: {"title":"t","coverUrl":"https://www.lesindesradios.fr.evil.example/x.jpg","coverId":"abc"}"#;
        let composed = parse_data_line(spoofed).unwrap().cover.unwrap();
        assert!(composed.starts_with("https://www.lesindesradios.fr/"), "{composed}");
        assert!(composed.contains("iid=abc"), "{composed}");
    }

    #[test]
    fn both_platforms_are_composed_from_the_real_frame() {
        // Patterns measured on 2026-08-27 on these exact identifiers: Deezer
        // returns 200 (redirects to /fr/track/…) and Apple Music returns 200
        // redirecting to …/song/shes-a-rainbow/1443171670 — the slug confirms
        // that the identifier really designates « SHE'S A RAINBOW », which
        // the frame announces elsewhere.
        let m = parse_data_line(FRAME).unwrap();
        assert_eq!(
            m.links,
            vec![
                Link::Deezer { url: "https://www.deezer.com/track/9956167".into() },
                Link::AppleMusic { url: "https://music.apple.com/song/1443171670".into() },
            ]
        );
    }

    #[test]
    fn a_non_numeric_identifier_is_refused() {
        // It goes into a URL the UI will make clickable. Nothing forces a
        // third party to write what we expect, and a `../` or a `@` in it
        // would change the target.
        for bad in ["\"../evil\"", "\"9956167@evil.example\"", "\"\"", "null", "[]", "\"12 34\""] {
            let line = format!(r#"data: {{"title":"t","deezerId":{bad}}}"#);
            let m = parse_data_line(&line).unwrap();
            assert!(m.links.is_empty(), "wrongly accepted: {bad}");
        }
    }

    #[test]
    fn the_three_forms_of_a_numeric_identifier_are_settled_one_by_one() {
        // One test per form, because the three go through different paths in
        // `serde_json` and only one of them is measured on the real stream.
        let expected = vec![Link::Deezer { url: "https://www.deezer.com/track/9956167".into() }];
        // The measured form: a string of digits.
        let m = parse_data_line(r#"data: {"title":"t","deezerId":"9956167"}"#).unwrap();
        assert_eq!(m.links, expected, "string of digits");
        // A bare JSON integer: the stream can change its mind on the form,
        // as it did for `durationInSeconds`.
        let m = parse_data_line(r#"data: {"title":"t","deezerId":9956167}"#).unwrap();
        assert_eq!(m.links, expected, "JSON integer");
        // A JSON float, on the other hand, is refused: `9956167.0` is not an
        // identifier, it is a value an encoder dressed up, and having to
        // decide whether `.0` can be dropped would make us the author of an
        // identifier the third party did not write. A missing link is seen
        // and fixed; a link to the wrong track is not.
        let m = parse_data_line(r#"data: {"title":"t","deezerId":9956167.0}"#).unwrap();
        assert!(m.links.is_empty(), "JSON float");
    }

    #[test]
    fn a_frame_without_identifier_gives_no_link() {
        assert!(parse_data_line(r#"data: {"title":"t"}"#).unwrap().links.is_empty());
    }

    #[test]
    fn without_a_cover_the_frame_remains_usable() {
        assert_eq!(parse_data_line(r#"data: {"title":"t"}"#).unwrap().cover, None);
    }

    #[test]
    fn the_duration_is_read_as_string_as_well_as_number() {
        let as_string = parse_data_line(r#"data: {"title":"t","durationInSeconds":"216"}"#).unwrap();
        assert_eq!(as_string.duration_s, Some(216));
        let as_number = parse_data_line(r#"data: {"title":"t","durationInSeconds":216}"#).unwrap();
        assert_eq!(as_number.duration_s, Some(216));
    }

    #[test]
    fn an_absurd_duration_is_ignored_without_losing_the_title() {
        for raw in ["0", "-5", "abc", "999999999", "\"\"", "null", "[]"] {
            let line = format!(r#"data: {{"title":"t","durationInSeconds":{raw}}}"#);
            // Quotes already present for the textual cases.
            let line = if raw.starts_with('"') || raw == "null" || raw == "[]" {
                line
            } else {
                format!(r#"data: {{"title":"t","durationInSeconds":"{raw}"}}"#)
            };
            let m = parse_data_line(&line).unwrap_or_else(|| panic!("title expected for {raw}"));
            assert_eq!(m.duration_s, None, "raw={raw}");
            assert_eq!(m.title.as_deref(), Some("t"));
        }
    }

    #[test]
    fn ignores_what_is_not_a_usable_frame() {
        assert!(parse_data_line(":ping").is_none(), "keep-alive comment");
        assert!(parse_data_line("event: message").is_none());
        assert!(parse_data_line("").is_none());
        assert!(parse_data_line("data:").is_none());
        assert!(parse_data_line("data: not json").is_none());
        // Neither artist nor title: nothing to display, so not an answer.
        assert!(parse_data_line(r#"data: {"durationInSeconds":10}"#).is_none());
        assert!(parse_data_line(r#"data: {"artist":"","title":"  "}"#).is_none());
    }

    #[test]
    fn accepts_a_partial_frame() {
        // Owner's decision: any available information is displayed.
        let m = parse_data_line(r#"data: {"artist":"Téléphone"}"#).unwrap();
        assert_eq!(m.artist.as_deref(), Some("Téléphone"));
        assert_eq!(m.title, None);
    }

    #[test]
    fn splitting_yields_the_complete_lines_and_keeps_the_remainder() {
        let mut buffer = b"data: {\"a\":1}\ndata: {\"b\"".to_vec();
        let lines = split_lines(&mut buffer);
        assert_eq!(lines, vec!["data: {\"a\":1}".to_string()]);
        assert_eq!(buffer, b"data: {\"b\"".to_vec(), "the remainder waits for what follows");
    }

    #[test]
    fn an_accented_character_cut_between_two_chunks_stays_intact() {
        // « é » is two bytes in UTF-8. Decoding each chunk separately would
        // give « T?l?phone »; so only a complete line is decoded.
        let text = "data: {\"artist\":\"Téléphone\"}\n";
        let bytes = text.as_bytes();
        let cut = text.find('é').unwrap() + 1; // in the middle of the « é »
        let mut buffer = bytes[..cut].to_vec();
        assert!(split_lines(&mut buffer).is_empty(), "no complete line yet");
        buffer.extend_from_slice(&bytes[cut..]);
        let lines = split_lines(&mut buffer);
        let m = parse_data_line(&lines[0]).unwrap();
        assert_eq!(m.artist.as_deref(), Some("Téléphone"));
    }

    #[test]
    fn several_lines_from_a_single_chunk_are_all_yielded() {
        let mut buffer = b"data: {\"title\":\"un\"}\n\ndata: {\"title\":\"deux\"}\n".to_vec();
        let lines = split_lines(&mut buffer);
        assert_eq!(lines.len(), 3, "two frames and the empty separator line");
        let titles: Vec<String> =
            lines.iter().filter_map(|l| parse_data_line(l)).filter_map(|m| m.title).collect();
        assert_eq!(titles, vec!["un".to_string(), "deux".to_string()]);
    }

    #[test]
    fn the_metas_url_carries_the_identifier() {
        assert_eq!(metas_url("42"), "https://www.ouifm.fr/ws/metas?id=42");
    }

    /// Typical duration of a connection that breaks right away.
    const IMMEDIATE: Duration = Duration::from_millis(80);

    #[test]
    fn a_connection_that_breaks_right_away_grows_the_backoff_up_to_the_cap() {
        let mut backoff = BACKOFF_BASE;
        let mut seen = vec![backoff];
        for _ in 0..10 {
            backoff = next_backoff(backoff, IMMEDIATE);
            seen.push(backoff);
        }
        assert_eq!(seen[0], Duration::from_secs(2));
        assert_eq!(seen[1], Duration::from_secs(4));
        assert_eq!(seen[2], Duration::from_secs(8));
        assert_eq!(*seen.last().unwrap(), BACKOFF_MAX, "the cap must be reached");
        assert!(seen.windows(2).all(|p| p[1] >= p[0]), "never decreasing");
    }

    #[test]
    fn a_connection_that_held_resets_the_backoff() {
        assert_eq!(next_backoff(BACKOFF_MAX, HEALTHY_DURATION), BACKOFF_BASE);
        assert_eq!(next_backoff(BACKOFF_MAX, Duration::from_secs(4 * 3600)), BACKOFF_BASE);
    }

    #[test]
    fn one_received_frame_is_not_enough_to_reset_the_backoff() {
        // This is the defect this split fixes: the server pushes a frame
        // **as soon as the connection opens**, so "at least one frame
        // received" is always true. Relying on it, the backoff restarted from
        // 2 s on every round, the cap was unreachable, and a server that
        // pushes then closes right away made us open a request every 2 s
        // indefinitely at a third party. Here, a half-second connection — the
        // time of one frame — lets the backoff grow.
        let after_one_frame = next_backoff(Duration::from_secs(8), Duration::from_millis(500));
        assert_eq!(after_one_frame, Duration::from_secs(16));
    }

    #[test]
    fn the_health_threshold_is_sharp() {
        // Just below the threshold: the backoff still grows.
        assert_eq!(
            next_backoff(Duration::from_secs(2), HEALTHY_DURATION - Duration::from_millis(1)),
            Duration::from_secs(4)
        );
    }
}
