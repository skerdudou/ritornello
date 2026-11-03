//! `metadata` plugin: completes the lyrics of the playing track from LRCLIB
//! (lrclib.net), and keeps what it found in a bounded disk cache.
//!
//! It only **completes** (`fill_only`): lyrics a file carries beside itself,
//! or that another contributor brought, always win, and the plugin does not
//! even ask when the core says lyrics are already held (`Known::lyrics`).
//!
//! What sets a search off is a new song — the playing identity with its
//! artist and title — not a new `NowPlaying`: the core sends one whenever
//! `known` grows, and a measured duration or a cover arriving for the same
//! song is not a reason to ask LRCLIB again.

mod admin;
mod cache;
mod client;
mod matching;
// Only compiled under `cargo test`: `ui_placeholder_js` is used at run time
// nowhere in this crate, only by `build.rs` (separate compilation, via
// `include!`) and by its own tests. Compiled permanently, it would be a
// `dead_code` that `-D warnings` refuses.
#[cfg(test)]
mod placeholder;

use anyhow::Result;
use cache::{Cache, Cached, Key};
use client::{Answer, Client, Query};
use ritornello_plugin_sdk::MetadataPlugin;
use ritornello_proto::{normalize_for_match, Enrichment, Lyrics, LyricsSubject, NowPlaying};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Embedded i18n catalog of the admin page (`admin.rs`): the layer the
/// core's `Runtime::texts` announces, and the fallback when no external pack
/// is present.
pub(crate) const LRCLIB_EN: &str = include_str!("locales/en.toml");

const LRCLIB_BASE: &str = "https://lrclib.net";

/// How long a file waits for the duration mpv measures before it is
/// searched without one.
///
/// The core resets the measured duration with every new track and fills it
/// again on its next tick, within a second. Searching before it arrives would
/// let LRCLIB pick any version of the song — measured, a search for "Get
/// Lucky" holds twenty, from 193 s to 370 s — and cache that guess. Three
/// seconds is three ticks: past that, the file has no duration to give.
const DURATION_WAIT: Duration = Duration::from_secs(3);

/// How long the same song is not asked about again after LRCLIB did not
/// answer.
///
/// Measured on 2026-11-02: LRCLIB answers 503 `ServerOverloaded`, every
/// time, for a song it does not have. That outage cannot be told from an
/// absence, so it is never cached as one — and without this spacing, a radio
/// coming back to that song, or a stop and play, would ask again each time.
/// Ten minutes outlasts a song, so one listening asks once.
const OVERLOADED_SPACING: Duration = Duration::from_secs(10 * 60);

/// How long every request waits after one went unanswered, when the server
/// gave no `Retry-After` of its own. LRCLIB's documentation asks clients to
/// back off, and threatens a ban otherwise.
const DEFAULT_PAUSE: Duration = Duration::from_secs(60);

/// Ceiling on a server's `Retry-After`. The header is read as any number of
/// seconds, and `Instant + Duration` panics past the clock's range; a server
/// asking for a year would also silence lyrics until the plugin restarts.
const MAX_SERVER_WAIT: Duration = Duration::from_secs(3600);

/// Sources whose identity is a track with a length mpv measures, so worth
/// [`DURATION_WAIT`]. A radio stream has no length to wait for.
const MEASURED_SOURCES: &[&str] = &["files", "cd"];

/// The cache's own directory inside the plugin's data directory.
///
/// A subdirectory, never the data directory itself: `Cache::open` and
/// `Cache::clear` delete every file they do not recognize as an entry.
fn cache_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("cache")
}

/// The cache lock, recovered from poisoning: a panic elsewhere while holding
/// it leaves an index that is at worst stale, and a stale cache costs one
/// request, where a poisoned one would end every lookup and the page.
pub(crate) fn lock(cache: &Mutex<Cache>) -> MutexGuard<'_, Cache> {
    cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The question asked of LRCLIB. A trait so the tests drive the timing (the
/// ten-minute spacing, the three-second wait) on a paused clock, which a real
/// socket does not allow: a paused runtime advances to its next timer while
/// a socket is pending, and fires the client's own timeout instead.
#[async_trait::async_trait]
trait Lookup: Send + Sync + 'static {
    async fn lookup(&self, q: &Query) -> Answer;
}

#[async_trait::async_trait]
impl Lookup for Client {
    async fn lookup(&self, q: &Query) -> Answer {
        Client::lookup(self, q).await
    }
}

/// One song, as far as starting a search goes: what plays, and its names
/// folded the way matching folds them.
///
/// The identity is part of it so that two files of the same song (an album
/// cut and a live take) are two songs; the names are part of it because a
/// radio's identity is its stream, the same for every song it plays.
#[derive(Debug, Clone, PartialEq)]
struct Song {
    identity: Value,
    artist: String,
    title: String,
}

/// A search decided on and not yet started.
#[derive(Debug, Clone)]
struct Wanted {
    song: Song,
    subject: LyricsSubject,
    query: Query,
    /// Not before this instant: the end of the wait for a duration.
    not_before: Instant,
}

/// What a lookup task sends back, in one message, with what it was for.
#[derive(Debug)]
struct Outcome {
    song: Song,
    subject: LyricsSubject,
    key: Key,
    answer: Answer,
}

struct LrclibPlugin {
    lookup: Arc<dyn Lookup>,
    /// Shared with the admin page, which reads its counts and clears it.
    cache: Arc<Mutex<Cache>>,
    /// The song now playing that this plugin handles, whatever the stage of
    /// its search. `None`: nothing to complete.
    current: Option<Song>,
    wanted: Option<Wanted>,
    /// One request at a time: LRCLIB asks for sequential requests, and a song
    /// change during a flight only queues the next one.
    in_flight: bool,
    /// An enrichment ready to go, with the song it is about: dropped when
    /// that song stops being the current one.
    ready: Option<(Song, Enrichment)>,
    /// Per song, the instant before which it is not asked again after an
    /// unanswered request (see [`OVERLOADED_SPACING`]). In memory only: a
    /// restart is a legitimate reason to ask again.
    retry_not_before: HashMap<Key, Instant>,
    /// No request at all before this instant (see [`DEFAULT_PAUSE`]).
    paused_until: Option<Instant>,
    outcome_tx: mpsc::Sender<Outcome>,
    outcome_rx: mpsc::Receiver<Outcome>,
}

impl LrclibPlugin {
    fn new(lookup: Arc<dyn Lookup>, cache: Arc<Mutex<Cache>>) -> Self {
        let (outcome_tx, outcome_rx) = mpsc::channel(4);
        Self {
            lookup,
            cache,
            current: None,
            wanted: None,
            in_flight: false,
            ready: None,
            retry_not_before: HashMap::new(),
            paused_until: None,
            outcome_tx,
            outcome_rx,
        }
    }

    /// The song `np` describes and the question to ask about it, or `None`
    /// when there is nothing to search: nothing plays, lyrics are already
    /// held, or the artist or the title is missing (a lone title matches
    /// anything, and LRCLIB's search requires both).
    fn target(np: &NowPlaying) -> Option<(Song, LyricsSubject, Query)> {
        let identity = np.identity.clone()?;
        if np.known.lyrics {
            return None;
        }
        let artist = np.known.artist.clone()?;
        let title = np.known.title.clone()?;
        let song = Song { identity, artist: normalize_for_match(&artist), title: normalize_for_match(&title) };
        // Names with no letter left ("!!!") match nothing, on LRCLIB's side
        // as on the core's: asking would only cache an absence.
        if song.artist.is_empty() || song.title.is_empty() {
            return None;
        }
        let query = Query {
            artist: artist.clone(),
            title: title.clone(),
            album: np.known.album.clone(),
            // A declared duration first, the measured one otherwise: a file's
            // `duration_s` is `None`, so this is the measured one for files.
            duration_s: np.known.duration_s.or(np.known.measured_duration_s),
        };
        Some((song, LyricsSubject { artist, title }, query))
    }

    /// Starts the wanted search if it is due: from the cache, or from the
    /// network when no pause or spacing holds it back.
    fn start_due(&mut self) {
        if self.in_flight {
            return;
        }
        let Some(wanted) = &self.wanted else { return };
        let now = Instant::now();
        if now < wanted.not_before {
            return;
        }
        let key = Key::of(&wanted.query);
        // A cached answer needs no network, so neither a pause nor a spacing
        // holds it back.
        let cached = lock(&self.cache).get(&key, SystemTime::now());
        if let Some(cached) = cached {
            let wanted = self.wanted.take().expect("checked above");
            let enrichment = match cached {
                Cached::Found(lyrics) => found(&wanted.song, wanted.subject, lyrics),
                Cached::Absent { .. } => searched_only(&wanted.song),
            };
            self.ready = Some((wanted.song, enrichment));
            return;
        }
        if self.retry_not_before.get(&key).is_some_and(|until| now < *until) {
            // Unanswered a moment ago: nothing is emitted, as for any outage.
            // The next play of the song after the spacing asks again.
            tracing::debug!("LRCLIB: not asking again yet about {} - {}", wanted.query.artist, wanted.query.title);
            self.wanted = None;
            return;
        }
        if self.paused_until.is_some_and(|until| now < until) {
            // Kept: `next_enrichment` sleeps until the pause ends.
            return;
        }
        let wanted = self.wanted.take().expect("checked above");
        self.in_flight = true;
        let lookup = self.lookup.clone();
        let tx = self.outcome_tx.clone();
        // A task, not the `next_enrichment` future: that future is dropped
        // at every `NowPlaying`, and a request dropped with it would be lost
        // for a song that has not changed.
        tokio::spawn(async move {
            let answer = lookup.lookup(&wanted.query).await;
            let outcome = Outcome { song: wanted.song, subject: wanted.subject, key, answer };
            let _ = tx.send(outcome).await;
        });
    }

    /// When `next_enrichment` must look again without an outcome arriving:
    /// the end of a wait for a duration, or of a pause.
    fn next_deadline(&self) -> Option<Instant> {
        if self.in_flight {
            return None;
        }
        let wanted = self.wanted.as_ref()?;
        Some(match self.paused_until {
            Some(until) if until > wanted.not_before => until,
            _ => wanted.not_before,
        })
    }

    /// Takes in what a lookup task reports.
    fn absorb(&mut self, outcome: Outcome) {
        self.in_flight = false;
        let Outcome { song, subject, key, answer } = outcome;
        let enrichment = match answer {
            // An answer is a fact about the song, whichever song plays now:
            // cached even when it arrives too late to be shown.
            Answer::Found { lyrics, .. } => {
                lock(&self.cache).put(&key, Cached::Found(lyrics.clone()), SystemTime::now());
                found(&song, subject, lyrics)
            }
            Answer::Instrumental | Answer::Absent => {
                let now = SystemTime::now();
                lock(&self.cache).put(&key, Cached::Absent { since: now }, now);
                searched_only(&song)
            }
            Answer::Unavailable { retry_after } => {
                // Nothing cached, nothing emitted: an outage is not an
                // answer, and the core shows nothing rather than a miss.
                let now = Instant::now();
                let server_wait = retry_after.map(|w| w.min(MAX_SERVER_WAIT));
                self.retry_not_before.retain(|_, until| *until > now);
                self.retry_not_before.insert(key, now + server_wait.unwrap_or_default().max(OVERLOADED_SPACING));
                self.paused_until = Some(now + server_wait.unwrap_or(DEFAULT_PAUSE));
                return;
            }
        };
        if self.current.as_ref() == Some(&song) {
            self.ready = Some((song, enrichment));
        }
    }
}

/// Lyrics found for `song`, named after the song they were asked for: the
/// core keeps them only while that song plays (a radio's identity does not
/// change between songs, so the echo alone would not tell).
fn found(song: &Song, subject: LyricsSubject, lyrics: Lyrics) -> Enrichment {
    Enrichment {
        identity: song.identity.clone(),
        lyrics: Some(lyrics),
        lyrics_subject: Some(subject),
        fill_only: true,
        searched: true,
        ..Default::default()
    }
}

/// "LRCLIB has nothing for this song": searched, and nothing else.
fn searched_only(song: &Song) -> Enrichment {
    Enrichment { identity: song.identity.clone(), fill_only: true, searched: true, ..Default::default() }
}

#[async_trait::async_trait]
impl MetadataPlugin for LrclibPlugin {
    async fn now_playing(&mut self, np: NowPlaying) {
        let Some((song, subject, query)) = Self::target(&np) else {
            self.current = None;
            self.wanted = None;
            self.ready = None;
            return;
        };
        if self.current.as_ref() != Some(&song) {
            let wait = query.duration_s.is_none() && MEASURED_SOURCES.contains(&np.source.as_str());
            let not_before = Instant::now() + if wait { DURATION_WAIT } else { Duration::ZERO };
            self.current = Some(song.clone());
            self.ready = None;
            self.wanted = Some(Wanted { song, subject, query, not_before });
        } else if let Some(wanted) = &mut self.wanted
            && wanted.query.duration_s.is_none()
            && query.duration_s.is_some()
        {
            // The duration the wait was for: search now, with it.
            wanted.query = query;
            wanted.not_before = Instant::now();
        }
        // Otherwise the same song, already searched or under way: a measured
        // duration or a cover arriving for it changes nothing.
    }

    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            if let Some((_, enrichment)) = self.ready.take() {
                return enrichment;
            }
            self.start_due();
            if self.ready.is_some() {
                continue;
            }
            let deadline = self.next_deadline();
            tokio::select! {
                Some(outcome) = self.outcome_rx.recv() => self.absorb(outcome),
                _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {}
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    // Everything this plugin writes lives in its own data directory — see
    // `ritornello_plugin_sdk::DATA_DIR_ENV` for why there is exactly one.
    let cache = Arc::new(Mutex::new(Cache::open(&cache_dir_in(&ritornello_plugin_sdk::data_dir()))));
    let lookup: Arc<dyn Lookup> = Arc::new(Client::new(LRCLIB_BASE)?);

    ritornello_plugin_sdk::declare_runtime!()?
        .texts([("en", LRCLIB_EN)])?
        .metadata(LrclibPlugin::new(lookup, cache.clone()))?
        .admin(admin::LrclibAdmin::new(cache))?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::Known;
    use serde_json::json;
    use std::collections::VecDeque;

    const GET_SYNCED: &str = include_str!("../tests/fixtures/get-synced.json");
    const SEARCH: &str = include_str!("../tests/fixtures/search.json");

    /// A lookup that answers from a script, after an optional delay, and
    /// records every question.
    #[derive(Default)]
    struct Scripted {
        answers: Mutex<VecDeque<(Duration, Answer)>>,
        asked: Mutex<Vec<Query>>,
    }

    impl Scripted {
        fn answering(answers: Vec<Answer>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into_iter().map(|a| (Duration::ZERO, a)).collect()),
                ..Default::default()
            })
        }

        fn asked(&self) -> Vec<Query> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Lookup for Scripted {
        async fn lookup(&self, q: &Query) -> Answer {
            self.asked.lock().unwrap().push(q.clone());
            let next = self.answers.lock().unwrap().pop_front();
            let (delay, answer) = next.unwrap_or_else(|| panic!("an unscripted question: {q:?}"));
            tokio::time::sleep(delay).await;
            answer
        }
    }

    fn real_lyrics() -> Lyrics {
        client::parse_record(GET_SYNCED).unwrap().lyrics().unwrap()
    }

    fn known(artist: Option<&str>, title: Option<&str>) -> Known {
        Known { artist: artist.map(Into::into), title: title.map(Into::into), ..Default::default() }
    }

    const STREAM: &str = "http://icecast.radiofrance.fr/fip-hifi.aac";

    fn radio(known: Known) -> NowPlaying {
        NowPlaying {
            source: "radio".into(),
            identity: Some(json!({ "kind": "stream", "url": STREAM })),
            known,
            armed: false,
        }
    }

    fn file(path: &str, known: Known) -> NowPlaying {
        NowPlaying { source: "files".into(), identity: Some(json!({ "kind": "file", "path": path })), known, armed: false }
    }

    fn stopped() -> NowPlaying {
        NowPlaying { source: "radio".into(), identity: None, known: Known::default(), armed: false }
    }

    fn lucky() -> Known {
        known(Some("Daft Punk"), Some("Get Lucky"))
    }

    struct Fixture {
        plugin: LrclibPlugin,
        cache: Arc<Mutex<Cache>>,
        _dir: tempfile::TempDir,
    }

    fn fixture(lookup: Arc<dyn Lookup>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Mutex::new(Cache::open(&cache_dir_in(dir.path()))));
        Fixture { plugin: LrclibPlugin::new(lookup, cache.clone()), cache, _dir: dir }
    }

    /// The next enrichment within `within`, `None` if there was none.
    async fn next(plugin: &mut LrclibPlugin, within: Duration) -> Option<Enrichment> {
        tokio::time::timeout(within, plugin.next_enrichment()).await.ok()
    }

    #[test]
    fn embedded_lrclib_en_is_not_empty() {
        assert!(!ritornello_i18n::try_parse(LRCLIB_EN).unwrap().is_empty());
    }

    #[test]
    fn every_file_lives_in_the_plugin_s_own_directory() {
        let d = Path::new("/x/plugins/lrclib");
        let cache = cache_dir_in(d);
        assert!(cache.starts_with(d));
        // A subdirectory of its own, never the data directory itself: the
        // cache deletes every file it does not recognize.
        assert_ne!(cache, d);
        assert_eq!(cache.file_name().unwrap(), "cache");
    }

    // --- Against a real client and a local server -----------------------

    /// A local server answering one connection per canned `(status, body)`,
    /// in order, and counting the requests it got.
    async fn serve(answers: Vec<(&'static str, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for (status, body) in answers {
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
                log.lock().unwrap().push(String::from_utf8_lossy(&request).lines().next().unwrap_or("").to_string());
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                sock.shutdown().await.ok();
            }
        });
        (base, seen)
    }

    /// Real time on purpose: a real socket under a paused clock lets the
    /// runtime jump to the client's own timeout (see [`Lookup`]).
    #[tokio::test]
    async fn lyrics_are_searched_once_artist_and_title_are_known() {
        let (base, seen) = serve(vec![("200 OK", SEARCH)]).await;
        let mut f = fixture(Arc::new(Client::new(&base).unwrap()));

        // A title alone, as an ICY header gives it before it is split.
        f.plugin.now_playing(radio(known(None, Some("Get Lucky")))).await;
        assert_eq!(next(&mut f.plugin, Duration::from_millis(300)).await, None);
        assert!(seen.lock().unwrap().is_empty(), "nothing asked without an artist");

        f.plugin.now_playing(radio(lucky())).await;
        let e = next(&mut f.plugin, Duration::from_secs(5)).await.expect("lyrics for a song LRCLIB has");
        assert!(e.lyrics.as_ref().is_some_and(|l| l.is_synced()), "synced lyrics are preferred: {e:?}");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(seen.lock().unwrap()[0].starts_with("GET /api/search?"), "no duration on a stream: a search");

        // The same song again, now with a cover: the core sends a new frame
        // whenever `known` grows. Not a new question.
        let mut with_cover = lucky();
        with_cover.cover = true;
        f.plugin.now_playing(radio(with_cover)).await;
        assert_eq!(next(&mut f.plugin, Duration::from_millis(300)).await, None);
        assert_eq!(seen.lock().unwrap().len(), 1, "one request for one song");
        assert_eq!(lock(&f.cache).stats().found, 1, "and its answer is cached");
    }

    // --- Timing, on a paused clock ---------------------------------------

    #[tokio::test(start_paused = true)]
    async fn nothing_is_searched_without_an_artist() {
        let lookup = Scripted::answering(vec![]);
        let mut f = fixture(lookup.clone());
        f.plugin.now_playing(radio(known(None, Some("Get Lucky")))).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(30)).await, None);
        f.plugin.now_playing(radio(known(Some("Daft Punk"), None))).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(30)).await, None);
        // Nor when nothing plays, whatever `known` says.
        let mut np = radio(lucky());
        np.identity = None;
        f.plugin.now_playing(np).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(30)).await, None);
        assert!(lookup.asked().is_empty());
    }

    /// A name that folds to nothing matches nothing, here as in the core's
    /// `LyricsSubject::matches`: asking would only cache an absence.
    #[tokio::test(start_paused = true)]
    async fn names_with_no_letter_left_are_not_searched() {
        let lookup = Scripted::answering(vec![]);
        let mut f = fixture(lookup.clone());
        f.plugin.now_playing(radio(known(Some("!!!"), Some("Get Lucky")))).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(30)).await, None);
        f.plugin.now_playing(radio(known(Some("Daft Punk"), Some("???")))).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(30)).await, None);
        assert!(lookup.asked().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_searched_when_lyrics_are_already_held() {
        let lookup = Scripted::answering(vec![]);
        let mut f = fixture(lookup.clone());
        let mut held = lucky();
        held.lyrics = true;
        f.plugin.now_playing(radio(held)).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(30)).await, None);
        assert!(lookup.asked().is_empty(), "a file's own .lrc wins without a request");
    }

    #[tokio::test(start_paused = true)]
    async fn a_measured_duration_alone_does_not_trigger_a_second_search() {
        let lookup = Scripted::answering(vec![Answer::Found { lyrics: real_lyrics(), duration_s: Some(248) }]);
        let mut f = fixture(lookup.clone());
        f.plugin.now_playing(radio(lucky())).await;
        assert!(next(&mut f.plugin, Duration::from_secs(1)).await.is_some());

        let mut measured = lucky();
        measured.measured_duration_s = Some(248);
        f.plugin.now_playing(radio(measured)).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(30)).await, None);
        assert_eq!(lookup.asked().len(), 1, "the duration changed the cache key, not the song");
    }

    #[tokio::test(start_paused = true)]
    async fn a_file_waits_for_its_measured_duration_and_searches_with_it() {
        let lookup = Scripted::answering(vec![Answer::Found { lyrics: real_lyrics(), duration_s: Some(248) }]);
        let mut f = fixture(lookup.clone());
        // The core resets the measured duration with a new track: the first
        // frame of a file comes without one.
        f.plugin.now_playing(file("/music/lucky.flac", lucky())).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(1)).await, None);
        assert!(lookup.asked().is_empty(), "not searched before the duration had a chance to come");

        let mut measured = lucky();
        measured.measured_duration_s = Some(248);
        f.plugin.now_playing(file("/music/lucky.flac", measured)).await;
        assert!(next(&mut f.plugin, Duration::from_millis(100)).await.is_some(), "searched at once, not at 3 s");
        assert_eq!(lookup.asked()[0].duration_s, Some(248));
        assert_eq!(lookup.asked().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_file_without_a_duration_is_searched_after_three_seconds() {
        let lookup = Scripted::answering(vec![Answer::Absent]);
        let mut f = fixture(lookup.clone());
        let start = Instant::now();
        f.plugin.now_playing(file("/music/lucky.flac", lucky())).await;
        assert!(next(&mut f.plugin, Duration::from_secs(10)).await.is_some());
        assert_eq!(start.elapsed(), DURATION_WAIT);
        assert_eq!(lookup.asked()[0].duration_s, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_does_not_wait_for_a_duration() {
        let lookup = Scripted::answering(vec![Answer::Absent]);
        let mut f = fixture(lookup.clone());
        let start = Instant::now();
        f.plugin.now_playing(radio(lucky())).await;
        assert!(next(&mut f.plugin, Duration::from_secs(10)).await.is_some());
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    /// Review Focus 2. LRCLIB answers 503 for a song it does not have, every
    /// time: nothing may be cached (an outage is not an absence), and the
    /// song may not be asked about at every play either.
    #[tokio::test(start_paused = true)]
    async fn an_overloaded_answer_is_neither_cached_nor_retried_at_once() {
        let lookup = Scripted::answering(vec![
            Answer::Unavailable { retry_after: None },
            Answer::Found { lyrics: real_lyrics(), duration_s: Some(248) },
        ]);
        let mut f = fixture(lookup.clone());
        f.plugin.now_playing(radio(lucky())).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(5)).await, None, "an outage emits nothing");
        assert_eq!(lookup.asked().len(), 1);
        assert_eq!(lock(&f.cache).stats(), cache::Stats::default(), "and caches nothing");

        // The same song a minute later (a stop, then play again).
        f.plugin.now_playing(stopped()).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        f.plugin.now_playing(radio(lucky())).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(5)).await, None);
        assert_eq!(lookup.asked().len(), 1, "not asked again a minute later");

        // Eleven minutes after the first request: asked again.
        f.plugin.now_playing(stopped()).await;
        tokio::time::sleep(Duration::from_secs(10 * 60)).await;
        f.plugin.now_playing(radio(lucky())).await;
        assert!(next(&mut f.plugin, Duration::from_secs(5)).await.is_some_and(|e| e.lyrics.is_some()));
        assert_eq!(lookup.asked().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_overloaded_answer_holds_every_request_for_the_server_s_wait() {
        let lookup = Scripted::answering(vec![
            Answer::Unavailable { retry_after: Some(Duration::from_secs(30)) },
            Answer::Absent,
        ]);
        let mut f = fixture(lookup.clone());
        f.plugin.now_playing(radio(lucky())).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(1)).await, None);

        // Another song: kept waiting, not dropped.
        let start = Instant::now();
        f.plugin.now_playing(radio(known(Some("Daft Punk"), Some("Instant Crush")))).await;
        assert!(next(&mut f.plugin, Duration::from_secs(60)).await.is_some());
        assert_eq!(lookup.asked().len(), 2);
        assert_eq!(start.elapsed(), Duration::from_secs(29), "asked when the server's 30 s were over");
    }

    #[tokio::test(start_paused = true)]
    async fn a_huge_retry_after_neither_panics_nor_silences_the_plugin_for_good() {
        let lookup = Scripted::answering(vec![
            Answer::Unavailable { retry_after: Some(Duration::from_secs(u64::MAX)) },
            Answer::Absent,
        ]);
        let mut f = fixture(lookup.clone());
        f.plugin.now_playing(radio(lucky())).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(1)).await, None);
        f.plugin.now_playing(radio(known(Some("Daft Punk"), Some("Instant Crush")))).await;
        assert!(next(&mut f.plugin, MAX_SERVER_WAIT + Duration::from_secs(1)).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_cached_answer_needs_no_network() {
        let lookup = Scripted::answering(vec![]);
        let mut f = fixture(lookup.clone());
        let lucky_query = Query { artist: "Daft Punk".into(), title: "Get Lucky".into(), album: None, duration_s: None };
        let crush_query = Query { title: "Instant Crush".into(), ..lucky_query.clone() };
        lock(&f.cache).put(&Key::of(&lucky_query), Cached::Found(real_lyrics()), SystemTime::now());
        lock(&f.cache).put(&Key::of(&crush_query), Cached::Absent { since: SystemTime::now() }, SystemTime::now());

        f.plugin.now_playing(radio(lucky())).await;
        let e = next(&mut f.plugin, Duration::from_secs(1)).await.unwrap();
        assert_eq!(e.lyrics, Some(real_lyrics()));

        f.plugin.now_playing(radio(known(Some("Daft Punk"), Some("Instant Crush")))).await;
        let e = next(&mut f.plugin, Duration::from_secs(1)).await.unwrap();
        assert_eq!((e.lyrics, e.searched), (None, true), "a cached absence is still a search that found nothing");
        assert!(lookup.asked().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_enrichment_names_the_song_it_was_found_for() {
        let lookup = Scripted::answering(vec![
            Answer::Found { lyrics: real_lyrics(), duration_s: Some(248) },
            Answer::Instrumental,
        ]);
        let mut f = fixture(lookup.clone());
        let np = radio(lucky());
        let identity = np.identity.clone().unwrap();
        f.plugin.now_playing(np).await;
        let e = next(&mut f.plugin, Duration::from_secs(1)).await.unwrap();
        assert_eq!(
            e,
            Enrichment {
                identity: identity.clone(),
                lyrics: Some(real_lyrics()),
                lyrics_subject: Some(LyricsSubject { artist: "Daft Punk".into(), title: "Get Lucky".into() }),
                fill_only: true,
                searched: true,
                ..Default::default()
            }
        );

        // An instrumental: searched, nothing found, nothing else said.
        f.plugin.now_playing(radio(known(Some("Daft Punk"), Some("Contact")))).await;
        let e = next(&mut f.plugin, Duration::from_secs(1)).await.unwrap();
        assert_eq!(e, Enrichment { identity, fill_only: true, searched: true, ..Default::default() });
        assert_eq!(lock(&f.cache).stats().absent, 1, "an instrumental is cached as an absence");
    }

    /// The radio case: the answer about A arrives once B plays, under the
    /// same stream identity. It is cached, never emitted under B.
    #[tokio::test(start_paused = true)]
    async fn a_late_answer_for_the_previous_song_is_cached_not_emitted() {
        let lookup = Arc::new(Scripted {
            answers: Mutex::new(VecDeque::from([
                (Duration::from_secs(5), Answer::Found { lyrics: real_lyrics(), duration_s: Some(248) }),
                (Duration::ZERO, Answer::Absent),
            ])),
            ..Default::default()
        });
        let mut f = fixture(lookup.clone());
        f.plugin.now_playing(radio(lucky())).await;
        assert_eq!(next(&mut f.plugin, Duration::from_secs(1)).await, None);
        f.plugin.now_playing(radio(known(Some("Daft Punk"), Some("Instant Crush")))).await;
        let e = next(&mut f.plugin, Duration::from_secs(30)).await.unwrap();
        assert_eq!(e.lyrics, None, "B's own answer, not A's lyrics: {e:?}");
        assert_eq!(lookup.asked().len(), 2);
        assert_eq!(lock(&f.cache).stats().found, 1, "A's lyrics are kept for the next time A plays");
    }
}
