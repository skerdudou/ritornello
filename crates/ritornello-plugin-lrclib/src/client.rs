//! The LRCLIB lookup: one question ("lyrics for this song?"), four answers.
//!
//! The answers keep apart what the plugin must never confuse: LRCLIB *said*
//! there is nothing (`Absent`, cached for a while), the song has no words
//! (`Instrumental`), and LRCLIB *did not answer* (`Unavailable`, never
//! cached). Measured on 2026-11-02: an unknown track gets a 503
//! `ServerOverloaded`, every time, on `/api/get` and `/api/search` alike — so
//! a 503 can hide an absence, but it is still never recorded as one.

use std::time::Duration;

use ritornello_proto::Lyrics;
use serde::Deserialize;

use crate::matching;

/// What the plugin knows about the playing track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Validated lyrics, and the duration of the version they belong to.
    Found { lyrics: Lyrics, duration_s: Option<u32> },
    /// LRCLIB knows the song and says it has no words.
    Instrumental,
    /// LRCLIB answered, and nothing it has fits the song.
    Absent,
    /// No usable answer: overloaded, rate-limited, timed out, unreachable,
    /// or unreadable. `retry_after` is the server's own wait, when it gave
    /// one in seconds.
    Unavailable { retry_after: Option<Duration> },
}

/// One LRCLIB track record, reduced to what matching and display need.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration_s: Option<f64>,
    pub instrumental: bool,
    pub plain: Option<String>,
    pub synced: Option<String>,
}

impl Record {
    /// The record's lyrics through the shared gate, `None` when there are
    /// none worth keeping (empty, oversized).
    pub fn lyrics(&self) -> Option<Lyrics> {
        Lyrics { plain: self.plain.clone(), synced: self.synced.clone() }.validated()
    }
}

/// The wire shape of a record. `artistName` and `trackName` are required: a
/// record without them cannot be matched, so it is not a record.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    artist_name: String,
    track_name: String,
    album_name: Option<String>,
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

impl From<Wire> for Record {
    fn from(w: Wire) -> Self {
        Self {
            artist: w.artist_name,
            title: w.track_name,
            album: w.album_name,
            duration_s: w.duration,
            instrumental: w.instrumental,
            plain: w.plain_lyrics,
            synced: w.synced_lyrics,
        }
    }
}

/// One record, as `/api/get` answers it.
pub fn parse_record(body: &str) -> Option<Record> {
    serde_json::from_str::<Wire>(body).ok().map(Record::from)
}

/// The array `/api/search` answers. `None` only when the body is not an
/// array at all; an entry that is not a record is skipped, so one odd entry
/// among twenty does not lose the other nineteen.
pub fn parse_search(body: &str) -> Option<Vec<Record>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(body).ok()?;
    Some(
        values
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Wire>(v).ok())
            .map(Record::from)
            .collect(),
    )
}

/// What one request gave.
enum Reply {
    Body(String),
    /// A 404: LRCLIB's own "no such track".
    NotFound,
    /// Anything else that is not a body: see [`Answer::Unavailable`].
    Unavailable(Option<Duration>),
}

/// `Retry-After` in seconds. The HTTP-date form is not read: LRCLIB has not
/// been seen sending it, and a wrong guess would be worse than none (the
/// plugin spaces its retries on its own).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

pub struct Client {
    http: reqwest::Client,
    base: String,
}

impl Client {
    /// `base` is `https://lrclib.net` in production, a local server in tests.
    pub fn new(base: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "ritornello-plugin-lrclib/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/skerdudou/ritornello)"
            ))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { http, base: base.trim_end_matches('/').to_string() })
    }

    /// Asks LRCLIB about `q`.
    ///
    /// With a known duration, `/api/get` first: LRCLIB picks the version
    /// itself (within about 2 s), one small answer instead of twenty. Its 404
    /// falls back to `/api/search`, which also serves an unknown duration.
    /// Whatever comes back still goes through [`matching::answer_for`]: the
    /// server's match is looser than ours. When it rejects the one record
    /// `/api/get` gave, the search is asked too before concluding: an absence
    /// is cached for 30 days, and one fuzzy server match must not settle it.
    pub async fn lookup(&self, q: &Query) -> Answer {
        let names = [("artist_name", q.artist.clone()), ("track_name", q.title.clone())];
        if let Some(duration) = q.duration_s {
            let mut params = names.to_vec();
            if let Some(album) = &q.album {
                params.push(("album_name", album.clone()));
            }
            params.push(("duration", duration.to_string()));
            match self.fetch("/api/get", &params).await {
                Reply::Body(body) => match parse_record(&body) {
                    Some(record) => match matching::answer_for(q, std::slice::from_ref(&record)) {
                        Answer::Absent => {}
                        answer => return answer,
                    },
                    None => return unreadable("/api/get"),
                },
                Reply::NotFound => {}
                Reply::Unavailable(retry_after) => return Answer::Unavailable { retry_after },
            }
        }
        match self.fetch("/api/search", &names).await {
            Reply::Body(body) => match parse_search(&body) {
                Some(records) => matching::answer_for(q, &records),
                None => unreadable("/api/search"),
            },
            // A search answers `[]` when it finds nothing; a 404 says the
            // same thing in another way.
            Reply::NotFound => Answer::Absent,
            Reply::Unavailable(retry_after) => Answer::Unavailable { retry_after },
        }
    }

    async fn fetch(&self, path: &str, params: &[(&str, String)]) -> Reply {
        let url = format!("{}{path}", self.base);
        let resp = match self.http.get(&url).query(params).send().await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::info!("LRCLIB {path}: no answer: {e}");
                return Reply::Unavailable(None);
            }
        };
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Reply::NotFound;
        }
        if !status.is_success() {
            // Named in the log: a 503 is what LRCLIB answers for a song it
            // does not have, and the only trace of why nothing showed.
            let wait = retry_after(resp.headers());
            tracing::info!("LRCLIB {path}: HTTP {status}");
            return Reply::Unavailable(wait);
        }
        read_bounded(path, resp).await
    }
}

/// Ceiling on an answer's body. The largest real answer measured, a search
/// returning twenty versions of "Get Lucky" with their lyrics
/// (`tests/fixtures/search.json`), is 286 KB; 2 MiB leaves room for wordier
/// songs. Without it only the 10 s timeout bounds a body — about 125 MB at
/// 100 Mbit/s, on a Pi.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Reads the body, at most [`MAX_BODY_BYTES`] of it. A declared length past
/// the ceiling is refused before reading a byte; an undeclared one is read
/// chunk by chunk and dropped as soon as it passes the ceiling. Too big is
/// `Unavailable`, never an absence: the server did answer, but not with
/// anything we can trust to mean "no lyrics".
async fn read_bounded(path: &str, mut resp: reqwest::Response) -> Reply {
    if resp.content_length().is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        tracing::info!("LRCLIB {path}: answer too large ({:?} bytes)", resp.content_length());
        return Reply::Unavailable(None);
    }
    let mut body = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    tracing::info!("LRCLIB {path}: answer too large, dropped past {MAX_BODY_BYTES} bytes");
                    return Reply::Unavailable(None);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Reply::Body(String::from_utf8_lossy(&body).into_owned()),
            Err(e) => {
                tracing::info!("LRCLIB {path}: answer cut short: {e}");
                return Reply::Unavailable(None);
            }
        }
    }
}

/// A 200 whose body is not what the endpoint answers (a maintenance page, a
/// proxy's error): the server did not answer the question, so nothing may be
/// concluded — least of all an absence.
fn unreadable(path: &str) -> Answer {
    tracing::info!("LRCLIB {path}: unreadable answer");
    Answer::Unavailable { retry_after: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const GET_SYNCED: &str = include_str!("../tests/fixtures/get-synced.json");
    const SEARCH: &str = include_str!("../tests/fixtures/search.json");
    const SEARCH_INSTRUMENTAL: &str = include_str!("../tests/fixtures/search-instrumental.json");
    const OVERLOADED: &str = include_str!("../tests/fixtures/overloaded-503.json");

    #[test]
    fn a_real_get_answer_parses_into_a_record() {
        let r = parse_record(GET_SYNCED).expect("a real /api/get answer");
        assert_eq!(r.artist, "Daft Punk");
        assert_eq!(r.title, "Get Lucky");
        assert_eq!(r.album.as_deref(), Some("Random Access Memories"));
        assert_eq!(r.duration_s, Some(367.0));
        assert!(!r.instrumental);
        assert!(r.plain.as_deref().is_some_and(|p| p.starts_with("Like the legend of the phoenix")));
        let lyrics = r.lyrics().expect("real lyrics pass the gate");
        assert!(lyrics.is_synced(), "the captured LRC parses as synced lyrics");
    }

    #[test]
    fn a_real_search_answer_parses_twenty_records() {
        let records = parse_search(SEARCH).expect("a real /api/search answer");
        assert_eq!(records.len(), 20);
        assert!(records.iter().all(|r| r.artist == "Daft Punk" && r.title == "Get Lucky"));
        // The measured fact matching rests on: same names, durations from
        // 193 s to 370 s.
        let min = records.iter().filter_map(|r| r.duration_s).fold(f64::MAX, f64::min);
        let max = records.iter().filter_map(|r| r.duration_s).fold(f64::MIN, f64::max);
        assert_eq!((min, max), (193.0, 370.0));
    }

    #[test]
    fn an_instrumental_record_has_no_lyrics() {
        let records = parse_search(SEARCH_INSTRUMENTAL).expect("a real search answer");
        let first = &records[0];
        assert_eq!((first.artist.as_str(), first.title.as_str()), ("Aphex Twin", "Avril 14th"));
        assert!(first.instrumental);
        assert_eq!((first.plain.as_deref(), first.synced.as_deref()), (None, None));
        assert_eq!(first.lyrics(), None);
    }

    #[test]
    fn an_error_body_is_neither_a_record_nor_a_search() {
        // The 503 body is an object, but not a track: it must not slip
        // through as a record with empty names.
        assert_eq!(parse_record(OVERLOADED), None);
        assert_eq!(parse_search(OVERLOADED), None);
        assert_eq!(parse_search("not json"), None);
    }

    #[test]
    fn one_malformed_search_entry_does_not_lose_the_others() {
        let mut values: Vec<serde_json::Value> = serde_json::from_str(SEARCH).unwrap();
        values[0] = serde_json::json!({ "id": 1, "message": "no names here" });
        let body = serde_json::to_string(&values).unwrap();
        assert_eq!(parse_search(&body).map(|r| r.len()), Some(19));
    }

    /// One canned HTTP answer.
    struct Canned {
        status: &'static str,
        headers: &'static str,
        body: String,
        /// The declared `Content-Length`; `None` sends none (the body then
        /// ends when the connection closes).
        content_length: Option<usize>,
        /// Keeps the connection open after the body instead of closing it,
        /// so a client that waits for the end waits for its timeout.
        hold_open: bool,
    }

    fn canned(status: &'static str, body: &str) -> Canned {
        Canned {
            status,
            headers: "",
            body: body.to_string(),
            content_length: Some(body.len()),
            hold_open: false,
        }
    }

    /// A local server answering one connection per canned answer, in order,
    /// and recording each request line (`GET /path?query HTTP/1.1`).
    async fn serve(answers: Vec<Canned>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for answer in answers {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let text = String::from_utf8_lossy(&request);
                log.lock().unwrap().push(text.lines().next().unwrap_or("").to_string());
                let length = answer
                    .content_length
                    .map(|n| format!("Content-Length: {n}\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\n{}{length}Connection: close\r\n\r\n{}",
                    answer.status, answer.headers, answer.body
                );
                // A client that gives up mid-body closes its end: not an
                // error of the server's.
                if sock.write_all(response.as_bytes()).await.is_err() {
                    continue;
                }
                if answer.hold_open {
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        drop(sock);
                    });
                } else {
                    sock.shutdown().await.ok();
                }
            }
        });
        (base, seen)
    }

    fn lucky(duration_s: Option<u32>) -> Query {
        Query { artist: "Daft Punk".into(), title: "Get Lucky".into(), album: None, duration_s }
    }

    #[tokio::test]
    async fn a_503_is_unavailable_not_absent() {
        // What LRCLIB measurably answers for a song it does not have, on
        // both endpoints: an absence it never states as one.
        let (base, seen) = serve(vec![
            canned("503 Service Unavailable", OVERLOADED),
            canned("503 Service Unavailable", OVERLOADED),
        ])
        .await;
        let client = Client::new(&base).unwrap();
        assert_eq!(client.lookup(&lucky(Some(254))).await, Answer::Unavailable { retry_after: None });
        assert_eq!(client.lookup(&lucky(None)).await, Answer::Unavailable { retry_after: None });
        let seen = seen.lock().unwrap();
        assert!(seen[0].starts_with("GET /api/get?"), "{seen:?}");
        assert!(seen[1].starts_with("GET /api/search?"), "{seen:?}");
    }

    #[tokio::test]
    async fn a_404_on_get_falls_back_to_search() {
        let (base, seen) = serve(vec![
            canned("404 Not Found", r#"{"code":404,"name":"TrackNotFound","message":"Failed to find specified track"}"#),
            canned("200 OK", SEARCH),
        ])
        .await;
        let client = Client::new(&base).unwrap();
        let q = Query { album: Some("Random Access Memories".into()), ..lucky(Some(254)) };
        match client.lookup(&q).await {
            Answer::Found { duration_s, lyrics } => {
                assert_eq!(duration_s, Some(254));
                assert!(lyrics.is_synced());
            }
            other => panic!("expected the 254 s version, got {other:?}"),
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[0].starts_with("GET /api/get?"), "{seen:?}");
        for param in ["artist_name=Daft+Punk", "track_name=Get+Lucky", "album_name=Random+Access+Memories", "duration=254"] {
            assert!(seen[0].contains(param), "{param} missing from {}", seen[0]);
        }
        assert!(seen[1].starts_with("GET /api/search?"), "{seen:?}");
        assert!(seen[1].contains("artist_name=Daft+Punk") && seen[1].contains("track_name=Get+Lucky"));
    }

    #[tokio::test]
    async fn a_known_duration_answered_by_get_is_found_without_a_search() {
        let (base, seen) = serve(vec![canned("200 OK", GET_SYNCED)]).await;
        let client = Client::new(&base).unwrap();
        match client.lookup(&lucky(Some(369))).await {
            // The server's own ±2 s tolerance: 369 asked, 367 answered.
            Answer::Found { duration_s, .. } => assert_eq!(duration_s, Some(367)),
            other => panic!("expected the record /api/get gave, got {other:?}"),
        }
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn retry_after_is_read_when_present() {
        let (base, _) = serve(vec![Canned {
            status: "429 Too Many Requests",
            headers: "Retry-After: 120\r\n",
            ..canned("", "{}")
        }])
        .await;
        let client = Client::new(&base).unwrap();
        assert_eq!(
            client.lookup(&lucky(None)).await,
            Answer::Unavailable { retry_after: Some(Duration::from_secs(120)) }
        );
    }

    #[tokio::test]
    async fn an_unreadable_200_is_unavailable_not_absent() {
        let (base, _) = serve(vec![canned("200 OK", "<html>maintenance</html>")]).await;
        let client = Client::new(&base).unwrap();
        assert_eq!(client.lookup(&lucky(None)).await, Answer::Unavailable { retry_after: None });
    }

    #[tokio::test]
    async fn a_rejected_get_answer_falls_back_to_search() {
        // `/api/get` matches loosely; here it hands back another artist's
        // record. Concluding `Absent` from it would hide the song's lyrics
        // for 30 days although the search has them.
        let mut other: serde_json::Value = serde_json::from_str(GET_SYNCED).unwrap();
        other["artistName"] = "Daft Punk Tribute Band".into();
        other["duration"] = 254.0.into();
        let (base, seen) = serve(vec![
            canned("200 OK", &other.to_string()),
            canned("200 OK", SEARCH),
        ])
        .await;
        let client = Client::new(&base).unwrap();
        match client.lookup(&lucky(Some(254))).await {
            Answer::Found { duration_s, .. } => assert_eq!(duration_s, Some(254)),
            other => panic!("expected the search's 254 s version, got {other:?}"),
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[1].starts_with("GET /api/search?"), "{seen:?}");
    }

    #[tokio::test]
    async fn an_oversized_body_is_unavailable_and_read_no_further() {
        // Both answers end in silence rather than a close: a client that
        // kept reading would hang until its 10 s timeout, so finishing
        // quickly is what proves it stopped.
        //
        // Declared: a length past the ceiling, refused before any byte.
        let declared = Canned {
            content_length: Some(MAX_BODY_BYTES + 1),
            hold_open: true,
            ..canned("200 OK", "")
        };
        // Undeclared: more than the ceiling, dropped once past it.
        let undeclared = Canned {
            content_length: None,
            hold_open: true,
            ..canned("200 OK", &"x".repeat(MAX_BODY_BYTES + 64 * 1024))
        };
        let (base, _) = serve(vec![declared, undeclared]).await;
        let client = Client::new(&base).unwrap();
        for case in ["declared length", "no length"] {
            let start = std::time::Instant::now();
            assert_eq!(
                client.lookup(&lucky(None)).await,
                Answer::Unavailable { retry_after: None },
                "{case}"
            );
            assert!(start.elapsed() < Duration::from_secs(5), "{case}: took {:?}", start.elapsed());
        }
    }

    #[tokio::test]
    async fn an_unreachable_server_is_unavailable() {
        // Bind then drop: the port is closed by the time the client dials.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = Client::new(&base).unwrap();
        assert_eq!(client.lookup(&lucky(Some(254))).await, Answer::Unavailable { retry_after: None });
    }
}
