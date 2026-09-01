//! Querying the Radio Browser community online directory.
//!
//! Testable split, on the model of the cd plugin's `musicbrainz.rs`: the *pure*
//! part (building the request URL, parsing the response) is tested against a
//! real capture stored in `tests/fixtures/`, the network call is isolated
//! separately. No test touches the network: the API was seen down during
//! design, a network test would be unstable by construction.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Number of results requested from the directory (politeness: upper bound).
const LIMIT: u32 = 30;

/// Directory servers, tried **in this order** until the first one that
/// answers, and as long as budget remains (see `SEARCH_BUDGET`).
/// `all.api.radio-browser.info` is a rotating record (a different address at
/// every resolution): we target concrete servers.
///
/// Honesty about this list: Radio Browser's mirror fleet **moves over time**,
/// these five names are the ones known at the time of writing and nothing
/// guarantees they all still exist in two years. That is not serious: an
/// unknown host fails fast (DNS resolution or connection refused, well before
/// the cap of one attempt) and we move on to the next without having eaten
/// into the budget. It is the **slow** servers that cost, and it is precisely
/// them that the global budget bounds. During design, `de1` returned `503` and
/// `/json/servers` itself answered "no available server": hence this fallback,
/// rather than a dynamic discovery that would depend on the same downed
/// directory.
pub const DEFAULT_BASES: &[&str] = &[
    "https://de1.api.radio-browser.info",
    "https://de2.api.radio-browser.info",
    "https://at1.api.radio-browser.info",
    "https://nl1.api.radio-browser.info",
    "https://fi1.api.radio-browser.info",
];

/// Error detail when no server answered. Like `timeout` or `connect failed`,
/// this short text is injected into the **translated** `search_error` message
/// at the admin boundary: the sentence seen by the user stays in their
/// language, only the technical detail is in English (same convention as the
/// rest of the plugin).
const NO_SERVER: &str = "no directory server answered";

/// **Global** budget of the search operation, shared by *all* attempts — and
/// not a per-server delay applied as many times as there are servers.
///
/// The reason is external to this module, and it is hard: the core invokes
/// `set_data` through `AdminClient::request`
/// (`crates/ritornello-plugin-sdk/src/client.rs`), which wraps **every** admin
/// round trip in a `tokio::time::timeout(Duration::from_secs(5), …)`. Past
/// that delay, the core returns a timeout error to the browser and **drops**
/// our response, even if it eventually arrives. A search exceeding 5 s is
/// therefore never seen: it keeps working for nobody while the page already
/// shows an error.
///
/// Hence 4 s and no more: margin is needed under the cap for serialization and
/// the round trip on the admin socket. And hence, too, a deliberately
/// **short** server list: it is not walked "to the end whatever the cost", but
/// as long as budget remains — lengthening the list would buy nothing, only
/// the first servers would actually be tried when things are slow.
const SEARCH_BUDGET: Duration = Duration::from_secs(4);

/// Cap of an individual attempt. A server that has not answered in 2 s is
/// considered lost: the remaining budget is better spent on the next one.
const PER_SERVER: Duration = Duration::from_secs(2);

/// Remainder below which no further attempt is opened: establishing a TLS
/// connection only to abandon it at once serves nobody (and would pin on the
/// next server the odium of a near-zero `timeout`, logged as a failure on its
/// part).
const MIN_ATTEMPT: Duration = Duration::from_millis(300);

/// Header explicitly required by the Radio Browser API: an identifiable agent,
/// in the same format as the cd plugin (`musicbrainz.rs`), with the crate
/// version rather than a frozen number.
const USER_AGENT: &str = concat!(
    "ritornello/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/skerdudou/ritornello)"
);

/// A station as returned by the directory, reduced to the fields useful to the
/// UI. This is the shape exposed by `GetData` (field `search`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryStation {
    pub name: String,
    pub url: String,
    pub codec: String,
    pub bitrate: u32,
    pub country: String,
}

/// Raw shape of a `/json/stations/search` entry. The API returns some thirty
/// fields: all those not declared here are ignored by serde, which makes
/// parsing insensitive to the directory's evolutions. Each field is `Option` +
/// `#[serde(default)]`: an incomplete entry or an explicit `null` must not make
/// the whole response fail.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawStation {
    name: Option<String>,
    url: Option<String>,
    url_resolved: Option<String>,
    codec: Option<String>,
    bitrate: Option<u32>,
    countrycode: Option<String>,
}

/// Usable URL of an entry: `url_resolved` (already de-redirected by the
/// directory) first, `url` otherwise. `None` if neither is an http(s) — the
/// station is then ignored, rather than offered only to end up rejected by
/// `Stations::validate` at save time.
fn usable_url(raw: &RawStation) -> Option<String> {
    for candidate in [raw.url_resolved.as_deref(), raw.url.as_deref()] {
        let u = candidate.unwrap_or("").trim();
        if u.starts_with("http://") || u.starts_with("https://") {
            return Some(u.to_string());
        }
    }
    None
}

/// Parses a `/json/stations/search` response. *Pure* function: it is what the
/// tests test, never the network. Unusable entries are silently ignored rather
/// than making the whole response fail.
pub fn parse_search_results(json: &str) -> Result<Vec<DirectoryStation>, String> {
    let raws: Vec<RawStation> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(raws
        .iter()
        .filter_map(|r| {
            let url = usable_url(r)?;
            Some(DirectoryStation {
                name: r.name.clone().unwrap_or_default(),
                url,
                codec: r.codec.clone().unwrap_or_default(),
                bitrate: r.bitrate.unwrap_or(0),
                country: r.countrycode.clone().unwrap_or_default(),
            })
        })
        .collect())
}

/// A directory country, reduced to what the UI needs.
///
/// `code` is the ISO 3166-1 alpha-2 code, the very one `countrycode=` expects
/// at search. No country **name** is carried: the UI renders it with
/// `Intl.DisplayNames`, hence in the browser's language and with no table to
/// keep up to date on our side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryCountry {
    pub code: String,
    pub stations: u32,
}

/// Raw entry of `/json/countrycodes`. Its `name` field carries the **code**
/// (`"FR"`), not a country name — the API's naming, not ours.
#[derive(Debug, Deserialize)]
struct RawCountry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    stationcount: Option<u32>,
}

/// Parses a `/json/countrycodes` response. *Pure* function, tested on a real
/// capture.
///
/// Unusable entries are silently discarded, as for stations: a code that is
/// not two letters cannot serve `countrycode=`, and a country without stations
/// has nothing to offer. Observed on 2026-07-27: 241 entries, all two-letter
/// and all non-empty — these guards are therefore preventive, and that is
/// exactly what one wants from third-party data.
pub fn parse_countries(json: &str) -> Result<Vec<DirectoryCountry>, String> {
    let raws: Vec<RawCountry> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(raws
        .iter()
        .filter_map(|r| {
            let code = r.name.as_deref()?.trim().to_ascii_uppercase();
            if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
                return None;
            }
            let stations = r.stationcount.unwrap_or(0);
            (stations > 0).then_some(DirectoryCountry { code, stations })
        })
        .collect())
}

/// URL of the country list.
pub fn countries_url(base: &str) -> String {
    format!("{}/json/countrycodes", base.trim_end_matches('/'))
}

/// Percent-encoding of a query parameter (unreserved characters left as is).
/// Hand-written: the pure part of the module depends on no HTTP library, it
/// stays compilable and testable on its own.
fn encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Search URL: `countrycode` is omitted when no country is requested ("all
/// countries"). `hidebroken` lets the directory filter dead streams itself,
/// `order=clickcount` + `reverse=true` bring the most listened-to first.
pub fn search_url(base: &str, query: &str, country: Option<&str>) -> String {
    let mut url = format!(
        "{}/json/stations/search?name={}",
        base.trim_end_matches('/'),
        encode(query)
    );
    if let Some(c) = country {
        url.push_str("&countrycode=");
        url.push_str(&encode(&c.to_ascii_uppercase()));
    }
    url.push_str("&hidebroken=true&order=clickcount&reverse=true&limit=");
    url.push_str(&LIMIT.to_string());
    url
}

/// Bases to try, in order. `RITORNELLO_RADIO_DIRECTORY`, if set and non-empty,
/// **pins** a server: it becomes the only one tried (an operator who imposes
/// their mirror does not want to see us sneak off elsewhere). Otherwise, the
/// built-in list.
pub fn bases_from_env() -> Vec<String> {
    match std::env::var("RITORNELLO_RADIO_DIRECTORY") {
        Ok(v) if !v.trim().is_empty() => vec![v.trim().to_string()],
        _ => DEFAULT_BASES.iter().map(|b| b.to_string()).collect(),
    }
}

/// Short error message: the full display of a reqwest error embeds the URL and
/// the whole chain of causes, unreadable in the admin page's message area. The
/// text is then injected into a translated message (`search_error`) at the
/// admin boundary.
fn short_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "timeout".to_string()
    } else if e.is_connect() {
        "connect failed".to_string()
    } else {
        e.without_url().to_string()
    }
}

/// Delay to grant the next attempt, from the **remaining** budget. *Pure*
/// function: all the budget arithmetic lives here, which makes it testable
/// without network or clock (the only caller, for its part, reads an
/// `Instant`).
///
/// `None` means "budget exhausted": do not start one more attempt.
fn attempt_timeout(remaining: Duration) -> Option<Duration> {
    if remaining < MIN_ATTEMPT {
        None
    } else {
        Some(remaining.min(PER_SERVER))
    }
}

/// Queries **one** directory server, with the delay the caller grants it:
/// `search` does not decide its own time, `search_with_fallback` is what
/// apportions the budget. The only point of the plugin that touches the
/// network, and only from the `search` operation of the Admin half: the Source
/// half (audio playback) never depends on it. Parsing is delegated to
/// `parse_search_results`, which keeps all decoding testable offline.
pub async fn search(
    base: &str,
    query: &str,
    country: Option<&str>,
    timeout: Duration,
) -> Result<Vec<DirectoryStation>, String> {
    let url = search_url(base, query, country);
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(short_error)?;
    let resp = client.get(&url).send().await.map_err(short_error)?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(short_error)?;
    parse_search_results(&body)
}

/// Queries **one** server for the country list. Same shape as `search`: the
/// delay is imposed by the caller, parsing is delegated to a pure function.
pub async fn countries(base: &str, timeout: Duration) -> Result<Vec<DirectoryCountry>, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(short_error)?;
    let resp = client.get(countries_url(base)).send().await.map_err(short_error)?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(short_error)?;
    parse_countries(&body)
}

/// Tries the servers **in order** and returns the first usable response. This
/// is the minimal resilience expected from a community directory: during
/// design, the default server returned `503` and the official server list was
/// itself unavailable.
///
/// The fallback triggers on **any** error, including an unreadable response: a
/// mirror answering broken JSON is as useless as a silent mirror. Accepted
/// trade-off: a real parsing defect (which the tests of Task 1 guard against)
/// would show up here as "no server answered" — hence the detailed log below,
/// which gives the real reason server by server.
///
/// **Budget**: it is held here, and nowhere else. An `Instant` is taken on
/// entry; before each attempt, `attempt_timeout` says how much time to grant
/// it from what remains of `SEARCH_BUDGET`, or that no more should be opened.
/// We then stop immediately, without walking the rest of the list: the core
/// will no longer listen to us past 5 s (see `SEARCH_BUDGET`), continuing
/// would be working for nobody.
///
/// Logging: each failure at `warn` with the server concerned, success at
/// `debug`, and budget exhaustion explicitly distinguished from list
/// exhaustion. On a screenless Pi, this is the only way to tell "the whole
/// directory is down" from "that mirror is dead" or from "the link is so slow
/// that only two servers could be tried" — the page, for its part, only
/// receives a short message.
pub async fn search_with_fallback(
    bases: &[String],
    query: &str,
    country: Option<&str>,
) -> Result<Vec<DirectoryStation>, String> {
    with_fallback(bases, "search", |base, timeout| async move {
        search(&base, query, country, timeout).await
    })
    .await
}

/// Country list, same fallback mechanics and same budget as the search: the
/// request leaves on the same admin socket, with the same 5 s cap on the core
/// side.
pub async fn countries_with_fallback(bases: &[String]) -> Result<Vec<DirectoryCountry>, String> {
    with_fallback(bases, "countries", |base, timeout| async move { countries(&base, timeout).await }).await
}

/// Tries the servers **in order**, under budget, and returns the first usable
/// response. All the logic described on `search_with_fallback` lives here —
/// search and country list share it, rather than keeping two budget
/// arithmetics consistent.
/// The server is passed **owned** to `attempt` and not borrowed: a future
/// borrowing the base would have to hold for any lifetime, which an async
/// closure cannot express. One `String` per attempt, on five attempts at most,
/// is not measurable.
async fn with_fallback<T, F, Fut>(bases: &[String], what: &str, attempt: F) -> Result<T, String>
where
    F: Fn(String, Duration) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let start = Instant::now();
    let mut attempts = 0usize;
    for base in bases {
        let remaining = SEARCH_BUDGET.saturating_sub(start.elapsed());
        let Some(timeout) = attempt_timeout(remaining) else {
            tracing::warn!(
                "{what} budget exhausted after {attempts} attempt(s), \
                 {} server(s) not tried",
                bases.len() - attempts
            );
            break;
        };
        attempts += 1;
        match attempt(base.clone(), timeout).await {
            Ok(response) => {
                tracing::debug!("directory {base}: {what} succeeded");
                return Ok(response);
            }
            Err(e) => tracing::warn!("directory {base} failed ({what}): {e}"),
        }
    }
    // A single short message, never the concatenation of the errors: the
    // detail is in the log, the admin page has no room for five causes.
    tracing::warn!(
        "no directory server answered for {what} ({attempts} tried in {:?})",
        start.elapsed()
    );
    Err(format!("{NO_SERVER} ({attempts} tried)"))
}

/// Injection seam: the Admin half only knows this trait, which lets tests
/// provide results (or an error) without opening a single socket.
#[async_trait::async_trait]
pub trait Directory: Send + Sync {
    async fn search(
        &self,
        query: &str,
        country: Option<&str>,
    ) -> Result<Vec<DirectoryStation>, String>;

    /// List of countries having at least one station.
    async fn countries(&self) -> Result<Vec<DirectoryCountry>, String>;
}

/// Real implementation: an HTTP call on the configured bases, tried in order.
/// The list is frozen at construction (no re-reading of the environment at
/// every search): the behaviour of a live process does not change under the
/// user's feet.
pub struct HttpDirectory {
    pub bases: Vec<String>,
}

impl HttpDirectory {
    /// Usual construction: the built-in list, or the server pinned by
    /// `RITORNELLO_RADIO_DIRECTORY`.
    pub fn from_env() -> Self {
        HttpDirectory { bases: bases_from_env() }
    }
}

#[async_trait::async_trait]
impl Directory for HttpDirectory {
    async fn search(
        &self,
        query: &str,
        country: Option<&str>,
    ) -> Result<Vec<DirectoryStation>, String> {
        search_with_fallback(&self.bases, query, country).await
    }

    async fn countries(&self) -> Result<Vec<DirectoryCountry>, String> {
        countries_with_fallback(&self.bases).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/radio-browser-search.json");
    /// Real capture of `/json/countrycodes` (taken on 2026-07-27, 241 entries),
    /// **reduced** to fourteen to stay readable in review.
    const COUNTRIES: &str = include_str!("../tests/fixtures/radio-browser-countrycodes.json");

    #[test]
    fn parse_countries_reads_a_real_capture() {
        let countries = parse_countries(COUNTRIES).unwrap();
        assert_eq!(countries.len(), 14);
        let fr = countries.iter().find(|p| p.code == "FR").expect("FR present");
        assert!(fr.stations > 1000, "unexpected counter: {}", fr.stations);
        // The API's `name` field carries the **code**, not a country name: if
        // that confusion ever crept in, `countrycode=` would receive "France"
        // and the search would return nothing any more.
        assert!(countries.iter().all(|p| p.code.len() == 2), "codes ISO expected");
    }

    #[test]
    fn parse_countries_discards_what_cannot_be_used() {
        // A code that is not two letters cannot feed `countrycode=`, and a
        // country without stations has nothing to offer. Third-party data: the
        // guard is preventive.
        let json = r#"[
            {"name":"FR","stationcount":10},
            {"name":"","stationcount":5},
            {"name":"FRANCE","stationcount":5},
            {"name":"XX","stationcount":0},
            {"name":"be","stationcount":3},
            {"stationcount":7},
            {"name":"D1","stationcount":2}
        ]"#;
        let countries = parse_countries(json).unwrap();
        let codes: Vec<&str> = countries.iter().map(|p| p.code.as_str()).collect();
        assert_eq!(codes, vec!["FR", "BE"], "lowercase normalized, rest discarded");
    }

    #[test]
    fn parse_countries_rejects_invalid_json() {
        assert!(parse_countries("not json").is_err());
        assert!(parse_countries("{}").is_err());
        assert_eq!(parse_countries("[]").unwrap().len(), 0);
    }

    #[test]
    fn the_countries_url_is_well_formed() {
        assert_eq!(
            countries_url("https://de1.api.radio-browser.info/"),
            "https://de1.api.radio-browser.info/json/countrycodes"
        );
    }

    #[test]
    fn parse_extracts_the_stations_from_the_fixture() {
        let stations = parse_search_results(FIXTURE).unwrap();
        // 5 entries in the capture, the last one has no usable URL
        assert_eq!(stations.len(), 4);
        assert_eq!(
            stations[0],
            DirectoryStation {
                name: "France Info".into(),
                url: "http://direct.franceinfo.fr/live/franceinfo-midfi.mp3".into(),
                codec: "MP3".into(),
                bitrate: 128,
                country: "FR".into(),
            }
        );
        assert_eq!(stations[1].name, "RTL");
        assert_eq!(stations[1].bitrate, 64);
        assert_eq!(stations[3].name, "RMC FR");
    }

    #[test]
    fn parse_ignores_an_entry_without_usable_url() {
        let stations = parse_search_results(FIXTURE).unwrap();
        assert!(
            !stations.iter().any(|s| s.name == "Station sans stream"),
            "an entry without URL must not be offered"
        );
        // a non-http(s) URL is treated as absent
        let json = r#"[{"name":"X","url":"ftp://nope","url_resolved":"","codec":"MP3","bitrate":64,"countrycode":"FR"}]"#;
        assert!(parse_search_results(json).unwrap().is_empty());
    }

    #[test]
    fn parse_prefers_url_resolved_over_url() {
        let json = r#"[{"name":"X","url":"http://redirige","url_resolved":"http://final","codec":"MP3","bitrate":128,"countrycode":"FR"}]"#;
        assert_eq!(parse_search_results(json).unwrap()[0].url, "http://final");
        // fallback on `url` when `url_resolved` is empty or absent
        let json = r#"[{"name":"X","url":"http://direct","url_resolved":"","codec":"MP3","bitrate":128,"countrycode":"FR"}]"#;
        assert_eq!(parse_search_results(json).unwrap()[0].url, "http://direct");
        let json = r#"[{"name":"X","url":"http://direct"}]"#;
        assert_eq!(parse_search_results(json).unwrap()[0].url, "http://direct");
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        let json = r#"[{"stationuuid":"abc","votes":42,"lastcheckok":1,"name":"X",
            "url":"http://x","url_resolved":"http://x","codec":"MP3","bitrate":128,
            "countrycode":"FR","geo_lat":null}]"#;
        let stations = parse_search_results(json).unwrap();
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].name, "X");
    }

    #[test]
    fn parse_rejects_invalid_json() {
        assert!(parse_search_results("not json").is_err());
        assert!(parse_search_results("{}").is_err());
        // empty list = zero results, not an error
        assert_eq!(parse_search_results("[]").unwrap(), vec![]);
    }

    #[test]
    fn request_url_with_country() {
        assert_eq!(
            search_url("https://de1.api.radio-browser.info", "france info", Some("fr")),
            "https://de1.api.radio-browser.info/json/stations/search?name=france%20info\
             &countrycode=FR&hidebroken=true&order=clickcount&reverse=true&limit=30"
        );
    }

    #[test]
    fn request_url_without_country_omits_countrycode() {
        let url = search_url("https://de1.api.radio-browser.info", "jazz", None);
        assert_eq!(
            url,
            "https://de1.api.radio-browser.info/json/stations/search?name=jazz\
             &hidebroken=true&order=clickcount&reverse=true&limit=30"
        );
        assert!(!url.contains("countrycode"));
    }

    #[test]
    fn request_url_normalizes_the_base_and_encodes_the_query() {
        assert!(search_url("https://de1.api.radio-browser.info/", "x", None)
            .starts_with("https://de1.api.radio-browser.info/json/stations/search?"));
        assert!(search_url("https://x", "rock & roll", None).contains("name=rock%20%26%20roll"));
    }

    /// *Pure* test on the construction of the list: the order of the servers
    /// and pinning through the environment. The real fallback (a silent
    /// server, the next one answering) is **not** tested: it would require the
    /// network. The three cases are in a single test because they manipulate
    /// the same environment variable, global to the process: spreading them
    /// over several tests would make them dependent on cargo's scheduling.
    #[test]
    fn default_bases_in_order_and_pinning_through_the_environment() {
        std::env::remove_var("RITORNELLO_RADIO_DIRECTORY");
        let expected: Vec<String> = DEFAULT_BASES.iter().map(|b| b.to_string()).collect();
        assert_eq!(bases_from_env(), expected);
        // the order is that of the constant, not an arbitrary order
        assert_eq!(bases_from_env()[0], "https://de1.api.radio-browser.info");
        assert_eq!(bases_from_env()[1], "https://de2.api.radio-browser.info");
        assert_eq!(bases_from_env().len(), 5);

        // pinned: the variable becomes the only base tried
        std::env::set_var("RITORNELLO_RADIO_DIRECTORY", "https://fr1.api.radio-browser.info");
        assert_eq!(bases_from_env(), vec!["https://fr1.api.radio-browser.info".to_string()]);

        // empty or blank value = variable ignored (fallback on the list)
        std::env::set_var("RITORNELLO_RADIO_DIRECTORY", "   ");
        assert_eq!(bases_from_env(), expected);
        std::env::remove_var("RITORNELLO_RADIO_DIRECTORY");

        // `HttpDirectory::from_env()` delegates to `bases_from_env()`: same
        // assertion here, in the only test that owns the environment
        // variable, rather than in a separate test that would read it without
        // owning it (and could then observe the pinning above set by another
        // test thread).
        assert_eq!(HttpDirectory::from_env().bases.len(), DEFAULT_BASES.len());
    }

    /// Budget arithmetic, tested **without network or clock**: it is what
    /// guarantees that `search_with_fallback` yields before the 5 s cap
    /// imposed by `AdminClient::request` on the core side. *Pure* test: it
    /// calls a total function on given `Duration`s, nothing that could depend
    /// on the machine's load.
    #[test]
    fn the_budget_bounds_each_attempt_then_refuses_to_open_another() {
        // intact budget: the attempt is capped by PER_SERVER, not by the remainder
        assert_eq!(attempt_timeout(SEARCH_BUDGET), Some(PER_SERVER));
        assert_eq!(attempt_timeout(Duration::from_secs(60)), Some(PER_SERVER));
        // budget eaten into: the attempt only gets what remains, never more
        let remainder = Duration::from_millis(1_500);
        assert_eq!(attempt_timeout(remainder), Some(remainder));
        // remainder just barely usable
        assert_eq!(attempt_timeout(MIN_ATTEMPT), Some(MIN_ATTEMPT));
        // budget exhausted (or unusable residue): no further attempt
        assert_eq!(attempt_timeout(Duration::ZERO), None);
        assert_eq!(attempt_timeout(MIN_ATTEMPT - Duration::from_millis(1)), None);
        // the constants themselves fit under the core's cap: this is the
        // invariant that makes the operation visible to the browser.
        assert!(SEARCH_BUDGET < Duration::from_secs(5), "AdminClient cap exceeded");
        assert!(PER_SERVER <= SEARCH_BUDGET, "a single attempt must not exhaust the budget");
        assert!(MIN_ATTEMPT < PER_SERVER);
    }

    /// No test touches the network: a base that is not an absolute URL makes
    /// reqwest fail **before** any I/O, which checks the error path (short
    /// message, no panic) without depending on the directory — observed down
    /// during design. The delay passed is never reached: the failure is
    /// immediate, the test does not last 2 s.
    #[tokio::test]
    async fn search_on_an_invalid_base_returns_a_short_error() {
        let err = search("not-a-url", "fip", None, PER_SERVER).await.unwrap_err();
        assert!(!err.is_empty(), "an error message is expected");
        assert!(!err.contains('\n'), "single-line message expected: {err}");
    }

    /// List exhaustion: all bases are invalid, so all fail before any I/O
    /// (same mechanics as the previous test). What is checked here is the
    /// **loop**: a single short message out, not the concatenation of the five
    /// errors. The failures being immediate, the budget is not eaten into: the
    /// list is walked entirely and the test stays instantaneous (no real
    /// waiting, hence nothing dependent on the scheduler).
    #[tokio::test]
    async fn search_with_fallback_tries_all_bases_then_gives_up() {
        let bases = vec!["not-a-url".to_string(), "not-one-either".to_string()];
        let err = search_with_fallback(&bases, "fip", None).await.unwrap_err();
        assert!(err.contains("no directory server answered"), "unexpected message: {err}");
        assert!(!err.contains('\n'), "single-line message expected: {err}");
    }

    /// Does not touch `RITORNELLO_RADIO_DIRECTORY`: the base list is given
    /// explicitly, not read from the environment. The variable is global to
    /// the process and already manipulated by
    /// `default_bases_in_order_and_pinning_through_the_environment` (which
    /// also checks `HttpDirectory::from_env()`); reading it here too would
    /// expose this test to an interleaving between test threads.
    #[tokio::test]
    async fn http_directory_delegates_to_the_fallback_on_its_base_list() {
        let d = HttpDirectory { bases: vec!["not-a-url".into()] };
        assert!(Directory::search(&d, "fip", Some("FR")).await.is_err());
    }
}
