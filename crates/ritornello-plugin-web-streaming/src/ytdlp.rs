//! `yt-dlp`, run under a deadline, and its `-J --flat-playlist` answer read.
//!
//! The command is always `<bin> -J --flat-playlist -- <url>`: `bin` is the
//! wrapper script `<data_dir>/bin/yt-dlp` that already carries the
//! configuration flags, so nothing here passes any. The `--` keeps a link that
//! begins with a dash from being read as an option.
//!
//! The deadline is held here, by `tokio`, and not by yt-dlp: killing the
//! process holds for every version, and a stuck extractor would otherwise
//! hold the caller for as long as it likes. `kill_on_drop` kills the child
//! when the deadline elapses (the future is dropped).
//!
//! What yt-dlp says is read from captures of its real answers
//! (`tests/fixtures/`, see its README). Measured on them: a playlist entry
//! that is gone has the title `[Private video]` or `[Deleted video]` and an
//! `availability` of `null`, exactly like a perfectly available entry of a
//! flat playlist, so `null` never means "unavailable" and the titles are
//! matched whole (a real title may begin with `[`). That rule is YouTube's:
//! another site's entry is available unless it declares otherwise.
//!
//! **Which rules an entry follows** is said by its extractor
//! (`extractor_key`, `ie_key` on a flat entry): a YouTube entry keeps its
//! 11-character id and has its address rebuilt from it; any other entry is
//! known by `extractor:id` and plays the address yt-dlp gave, judged by the
//! very function that judges an operator's link ([`Video::play_url`]). Both
//! ways are safe whatever a hostile page claims: a false "Youtube" still
//! needs a well-formed id, a false other site still needs a valid address.
//!
//! **A live stream is refused** (`is_live`, or a `live_status` of `is_live`
//! or `is_upcoming`): the probe of one fails with [`ToolError::LiveStream`],
//! and a live entry of a list is unavailable. It never ends.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::link::{parse_link, watch_url_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The wrapper is not there (the tool was never installed).
    Missing,
    /// The deadline elapsed; the child was killed.
    Timeout,
    /// yt-dlp refused (stderr's last non-empty line), or said something that
    /// is not the JSON we asked for.
    Failed(String),
    /// The link names a live stream.
    LiveStream,
}

/// yt-dlp's name of the extractor of a YouTube video.
pub const YOUTUBE: &str = "Youtube";

/// The last line the wrapper writes when it refuses a `-J` answer that names
/// a live stream (`tool/wrapper.rs`, `guard_answer`): a probe ending on it is
/// a [`ToolError::LiveStream`], as if the answer had been read here.
pub const LIVE_REFUSAL: &str = "ERROR: a live stream is not played";

/// The longest id an entry may have: an id is only ever written into an
/// identity string, and one longer than this is not an id but garbage.
pub const ID_MAX_BYTES: usize = 256;

/// What an entry is known by when yt-dlp named no extractor.
const NO_EXTRACTOR: &str = "web";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Playlist,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Video {
    /// `extractor_key`, or `ie_key` on a flat entry: [`YOUTUBE`] for a
    /// YouTube video. Empty when yt-dlp named none.
    pub extractor: String,
    /// YouTube's 11-character id, or another site's id: free text, only ever
    /// written into an identity string, never into a path or an address.
    pub id: String,
    pub title: String,
    pub duration: Option<u32>,
    /// False for a live stream, for a declared availability other than
    /// public or unlisted, and on YouTube for `[Private video]` and
    /// `[Deleted video]`.
    pub available: bool,
    /// The largest `.jpg` thumbnail, when the entry carries any.
    pub thumbnail_jpg: Option<String>,
    /// The address yt-dlp gave: `webpage_url`, or a flat entry's `url`.
    /// Remote data: read only through [`Video::play_url`].
    pub url: Option<String>,
}

impl Video {
    pub fn is_youtube(&self) -> bool {
        self.extractor == YOUTUBE
    }

    /// The address that plays this entry, fit to be a line of an m3u: a
    /// YouTube entry's rebuilt `watch` URL (its id well-formed), any other's
    /// own address when it is a link an operator could have pasted
    /// (`link::parse_link`: http(s), no control character, a host, not a
    /// local IP literal; a YouTube address follows YouTube's rules there).
    pub fn play_url(&self) -> Option<String> {
        if self.is_youtube() {
            return watch_url_of(&self.id);
        }
        parse_link(self.url.as_deref()?).ok()?.watch_url()
    }

    /// What a `metadata` plugin knows this entry by: YouTube's id as it
    /// always was, `extractor:id` for any other site (`web:id` when yt-dlp
    /// named no extractor).
    pub fn identity(&self) -> String {
        if self.is_youtube() {
            return self.id.clone();
        }
        let extractor = if self.extractor.is_empty() { NO_EXTRACTOR } else { &self.extractor };
        format!("{extractor}:{}", self.id)
    }

    /// A name to show and to file the entry under: its title, or, when it
    /// has none (measured: a flat SoundCloud set's entries carry no title),
    /// the last non-empty path segment of its address, percent-decoded
    /// (`world-on-fire-1`). `None` when that is missing or only digits
    /// (`/tracks/47127631`), and on YouTube, whose address names nothing
    /// (its last segment is `watch`): the caller says "track n" instead.
    pub fn display_title(&self) -> Option<String> {
        let title = self.title.trim();
        if !title.is_empty() {
            return Some(title.to_string());
        }
        if self.is_youtube() {
            return None;
        }
        let url = url::Url::parse(&self.play_url()?).ok()?;
        let segment = url.path_segments()?.rev().find(|s| !s.is_empty())?;
        let decoded: String =
            percent_decode(segment).chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let decoded = decoded.trim();
        (!decoded.is_empty() && !decoded.chars().all(|c| c.is_ascii_digit())).then(|| decoded.to_string())
    }
}

/// `%XX` decoded, the rest kept; invalid UTF-8 replaced.
fn percent_decode(s: &str) -> String {
    let (bytes, mut out, mut i) = (s.as_bytes(), Vec::with_capacity(s.len()), 0);
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Probe {
    pub kind: Kind,
    /// The answer's `extractor_key` (a list's: `YoutubeTab`, `SoundcloudSet`,
    /// `BandcampAlbum`...). Empty when yt-dlp named none.
    pub extractor: String,
    pub id: String,
    pub title: String,
    pub channel: Option<String>,
    /// `webpage_url`.
    pub url: Option<String>,
    /// A video probe has exactly one entry: itself.
    pub entries: Vec<Video>,
    /// The thumbnail of the video, or the cover of the playlist.
    pub thumbnail_jpg: Option<String>,
}

pub(crate) fn kill_group(pgid: Option<u32>) {
    if let Some(pgid) = pgid {
        // SAFETY: `killpg` is a plain syscall taking a number; nothing here
        // checks that it still names our group. What the callers pass is the
        // id of a child they spawned as a group leader, shortly before. Linux
        // keeps a group id allocated while any member lives, so even when the
        // leader has been reaped (the probe's `wait()` resolves before its
        // pipes close) the id still names our group as long as a descendant
        // does; once none is left `killpg` answers ESRCH, and the id could be
        // reused only by a process that appeared in that gap. The run path
        // kills before it reaps, and does not rely on this.
        unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
    }
}

/// Set on every run of the wrapper the plugin starts itself: the wrapper
/// records a failure only for the runs it serves for mpv (`crate::playfail`).
pub const OWN_RUN_ENV: &str = "RITORNELLO_WEB_STREAMING_OWN_RUN";

/// Spawns `bin args` as the leader of a process group of its own, marked as
/// the plugin's own run ([`OWN_RUN_ENV`]).
///
/// `ETXTBSY` ("text file busy") is retried a few times: a script that was
/// written an instant ago can still be open for writing in a process forked
/// by another thread, between its fork and its exec, and the kernel refuses
/// to execute it until that child is gone.
pub(crate) async fn spawn_in_own_group(bin: &Path, args: &[String]) -> std::io::Result<tokio::process::Child> {
    let mut attempt = 0;
    loop {
        let spawned = tokio::process::Command::new(bin)
            .args(args)
            .env(OWN_RUN_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn();
        match spawned {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && attempt < 20 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            other => return other,
        }
    }
}

pub struct Ytdlp {
    bin: PathBuf,
}

/// The largest `-J` answer a probe reads: a flat playlist of 182 videos is a
/// few hundred kilobytes, and an answer of hundreds of megabytes (held twice,
/// as bytes then parsed, on a 1 GB device) is refused as unreadable rather
/// than read. The deadline bounds time, this bounds memory.
pub const PROBE_ANSWER_MAX: usize = 32 * 1024 * 1024;

/// The longest one `-J` probe may take — adding a link, refreshing a
/// playlist, a download's own probe — before yt-dlp's group is killed.
///
/// Sized for the wrapper's two attempts, not for one. Measured on the
/// reference device (armv7l Raspberry Pi 2, 2026-11-04): a YouTube video
/// resolves in 6.8 s without a JavaScript runtime, but some videos answer
/// "This video is not available" until a JS challenge is solved, and the
/// second attempt then took 2 min 10 s with QuickJS (28.8 s with node 22).
/// The earlier 120 s killed that QuickJS attempt just before it answered,
/// and the page said the site had not answered in time.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(300);

impl Ytdlp {
    pub fn new(data_dir: &Path) -> Self {
        Self { bin: data_dir.join("bin").join("yt-dlp") }
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    pub fn installed(&self) -> bool {
        self.bin.is_file()
    }

    pub async fn probe(&self, url: &str, deadline: Duration) -> Result<Probe, ToolError> {
        self.probe_capped(url, deadline, PROBE_ANSWER_MAX).await
    }

    /// [`probe`](Self::probe), the answer read up to `cap` bytes: past it,
    /// the run's group is killed (its pipe would otherwise fill and hold it
    /// until the deadline) and the answer refused.
    async fn probe_capped(&self, url: &str, deadline: Duration, cap: usize) -> Result<Probe, ToolError> {
        if !self.installed() {
            return Err(ToolError::Missing);
        }
        let mut child = spawn_in_own_group(&self.bin, &["-J".into(), "--flat-playlist".into(), "--".into(), url.into()])
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ToolError::Missing
                } else {
                    ToolError::Failed(e.to_string())
                }
            })?;
        let pgid = child.id();
        use tokio::io::AsyncReadExt;
        let (mut stdout, mut stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        // The child stays outside the timed future, so that on the deadline it
        // is still ours (not yet reaped) when its group is killed.
        let ran = tokio::time::timeout(deadline, async {
            let capped = async {
                (&mut stdout).take(cap as u64 + 1).read_to_end(&mut out).await?;
                if out.len() > cap {
                    kill_group(pgid);
                }
                Ok::<(), std::io::Error>(())
            };
            let (a, b, status) = tokio::join!(capped, stderr.read_to_end(&mut err), child.wait());
            a.and(b).and(status)
        })
        .await;
        let status = match ran {
            Ok(r) => r.map_err(|e| ToolError::Failed(e.to_string()))?,
            Err(_) => {
                kill_group(pgid);
                let _ = child.wait().await;
                return Err(ToolError::Timeout);
            }
        };
        if out.len() > cap {
            return Err(ToolError::Failed(format!(
                "{}larger than {} MiB",
                crate::reason::UNREADABLE_PREFIX,
                cap / (1024 * 1024)
            )));
        }
        if !status.success() {
            let stderr = String::from_utf8_lossy(&err);
            let last = stderr.lines().map(str::trim).rfind(|l| !l.is_empty());
            if last == Some(LIVE_REFUSAL) {
                return Err(ToolError::LiveStream);
            }
            return Err(ToolError::Failed(
                last.map_or_else(|| format!("{}{status}", crate::reason::EXIT_PREFIX), str::to_string),
            ));
        }
        parse_probe(&String::from_utf8_lossy(&out))
    }
}

/// Titles yt-dlp gives an entry that is gone, matched whole: a real title may
/// begin with a bracket.
const GONE_TITLES: [&str; 2] = ["[Private video]", "[Deleted video]"];

pub fn parse_probe(json: &str) -> Result<Probe, ToolError> {
    let unreadable = |what: &str| ToolError::Failed(format!("{}{what}", crate::reason::UNREADABLE_PREFIX));
    let root: Value = serde_json::from_str(json).map_err(|e| unreadable(&e.to_string()))?;
    let failed = unreadable;
    let id = root["id"].as_str().ok_or_else(|| failed("no id"))?.to_string();
    let title = root["title"].as_str().ok_or_else(|| failed("no title"))?.to_string();
    let channel = root["channel"].as_str().or_else(|| root["uploader"].as_str()).map(str::to_string);
    let thumbnail_jpg = largest_jpg(&root["thumbnails"]);
    if is_live(&root) {
        return Err(ToolError::LiveStream);
    }
    let extractor = extractor_of(&root);
    let url = root["webpage_url"].as_str().map(str::to_string);
    let (kind, entries) = match root["_type"].as_str() {
        Some("video") => (Kind::Video, vec![read_video(&root).ok_or_else(|| failed("video without id"))?]),
        Some("playlist") => {
            let entries =
                root["entries"].as_array().ok_or_else(|| failed("playlist without entries"))?.iter().filter_map(read_video).collect();
            (Kind::Playlist, entries)
        }
        other => return Err(failed(&format!("type {other:?}"))),
    };
    Ok(Probe { kind, extractor, id, title, channel, url, entries, thumbnail_jpg })
}

/// A live stream, or one announced and not started yet.
fn is_live(v: &Value) -> bool {
    v["is_live"].as_bool() == Some(true) || matches!(v["live_status"].as_str(), Some("is_live" | "is_upcoming"))
}

fn extractor_of(v: &Value) -> String {
    v["extractor_key"].as_str().or_else(|| v["ie_key"].as_str()).unwrap_or_default().to_string()
}

fn read_video(v: &Value) -> Option<Video> {
    let id = v["id"].as_str()?.to_string();
    let extractor = extractor_of(v);
    let title = v["title"].as_str().unwrap_or_default().to_string();
    // `duration` is an integer in a playlist entry and may be a float elsewhere.
    let duration =
        v["duration"].as_f64().filter(|d| *d >= 0.0 && *d <= f64::from(u32::MAX)).map(|d| d.round() as u32);
    // `availability: null` is also what an available entry of a flat playlist
    // carries, so only a declared value other than public/unlisted refuses.
    let declared_ok = matches!(v["availability"].as_str(), None | Some("public") | Some("unlisted"));
    let gone = extractor == YOUTUBE && GONE_TITLES.contains(&title.as_str());
    let available = declared_ok && !gone && !is_live(v) && id.len() <= ID_MAX_BYTES;
    // The page's address before a flat entry's `url`: on a full answer `url`
    // is the chosen format's media address.
    let url = v["webpage_url"].as_str().or_else(|| v["url"].as_str()).map(str::to_string);
    Some(Video { extractor, id, title, duration, available, thumbnail_jpg: largest_jpg(&v["thumbnails"]), url })
}

/// The URL of the largest `.jpg` of a `thumbnails` array (the extension is
/// judged on the URL's path, never its query string).
///
/// "Largest" is the greatest `width * height` among the entries that declare
/// both: those are the ones the source really served. Failing any, the best
/// `preference` (yt-dlp lists the worst first) among the unsized ones, whose
/// existence is a guess. A webp or png is never taken: the display side
/// decodes jpg only.
fn largest_jpg(thumbnails: &Value) -> Option<String> {
    let jpgs: Vec<(&str, Option<u64>, i64)> = thumbnails
        .as_array()?
        .iter()
        .filter_map(|t| {
            let url = t["url"].as_str()?;
            let parsed = url::Url::parse(url).ok()?;
            if !parsed.path().to_ascii_lowercase().ends_with(".jpg") {
                return None;
            }
            let area = match (t["width"].as_u64(), t["height"].as_u64()) {
                (Some(w), Some(h)) => Some(w * h),
                _ => None,
            };
            Some((url, area, t["preference"].as_i64().unwrap_or(0)))
        })
        .collect();
    let sized = jpgs.iter().filter(|(_, area, _)| area.is_some()).max_by_key(|(_, area, _)| *area);
    let best = sized.or_else(|| jpgs.iter().max_by_key(|(_, _, preference)| *preference));
    best.map(|(url, _, _)| (*url).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn a_video_probe_has_one_entry_and_its_title() {
        let p = parse_probe(&fixture("video.json")).unwrap();
        assert_eq!(p.kind, Kind::Video);
        assert_eq!(p.id, "jNQXAC9IVRw");
        assert_eq!(p.title, "Me at the zoo");
        assert_eq!(p.channel.as_deref(), Some("jawed"));
        assert_eq!(p.entries.len(), 1);
        let v = &p.entries[0];
        assert_eq!(v.id, "jNQXAC9IVRw");
        assert_eq!(v.title, "Me at the zoo");
        assert_eq!(v.duration, Some(19));
        assert!(v.available);
    }

    #[test]
    fn a_playlist_probe_lists_its_entries_in_order() {
        let p = parse_probe(&fixture("playlist.json")).unwrap();
        assert_eq!(p.kind, Kind::Playlist);
        assert_eq!(p.id, "PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI");
        assert_eq!(p.title, "Popular Music Videos");
        assert_eq!(p.channel.as_deref(), Some("Music"));
        assert_eq!(p.entries.len(), 12);
        assert_eq!(p.entries[0].id, "fOT0BUpITw8");
        assert_eq!(p.entries[0].title, "BELLAKEO (Video Oficial) - Peso Pluma, Anitta");
        assert_eq!(p.entries[0].duration, Some(235));
        assert!(p.entries.iter().all(|e| e.available));
        // Order is the file's, not sorted by anything.
        let raw: Value = serde_json::from_str(&fixture("playlist.json")).unwrap();
        let ids: Vec<&str> = raw["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        let got: Vec<&str> = p.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(got, ids);
    }

    #[test]
    fn unavailable_entries_are_kept_but_marked() {
        let p = parse_probe(&fixture("playlist-with-unavailable.json")).unwrap();
        assert_eq!(p.entries.len(), 7, "an unavailable entry keeps its place");
        let marks: Vec<(&str, bool)> = p.entries.iter().map(|e| (e.title.as_str(), e.available)).collect();
        assert_eq!(marks[1], ("[Private video]", false));
        assert_eq!(marks[5], ("[Private video]", false));
        assert_eq!(marks[6], ("[Deleted video]", false));
        // A real title that merely begins with a bracket is a playable video.
        assert_eq!(marks[3], ("[ORIGINAL] Kid cussing and punching! \"Hello Motherf*cker\"", true));
        // And `availability: null`, which yt-dlp also puts on available
        // entries of a flat playlist, is not a refusal.
        assert!(p.entries[0].available && p.entries[2].available && p.entries[4].available);
        assert_eq!(p.entries[1].duration, None);
    }

    #[test]
    fn a_declared_availability_other_than_public_or_unlisted_is_unavailable() {
        let entry = |availability: &str| {
            format!(
                r#"{{"_type":"playlist","id":"PL1","title":"t","entries":[{{"id":"aaaaaaaaaaa","title":"x","duration":1,"availability":{availability}}}]}}"#
            )
        };
        for (value, available) in [
            (r#""public""#, true),
            (r#""unlisted""#, true),
            ("null", true),
            (r#""private""#, false),
            (r#""needs_auth""#, false),
            (r#""subscriber_only""#, false),
            (r#""premium_only""#, false),
        ] {
            let p = parse_probe(&entry(value)).unwrap();
            assert_eq!(p.entries[0].available, available, "availability {value}");
        }
    }

    #[test]
    fn the_thumbnail_is_the_largest_jpg_and_never_a_webp() {
        // Sized thumbnails: the biggest area wins, the query string is kept
        // in the answer but ignored to judge the extension.
        let p = parse_probe(&fixture("video.json")).unwrap();
        let url = p.thumbnail_jpg.as_deref().unwrap();
        assert!(url.starts_with("https://i.ytimg.com/vi/jNQXAC9IVRw/hqdefault.jpg?sqp="), "{url}");
        assert_eq!(p.entries[0].thumbnail_jpg.as_deref(), Some(url));

        let p = parse_probe(&fixture("playlist.json")).unwrap();
        let first = p.entries[0].thumbnail_jpg.as_deref().unwrap();
        let raw: Value = serde_json::from_str(&fixture("playlist.json")).unwrap();
        let sized = raw["entries"][0]["thumbnails"].as_array().unwrap();
        let widest = sized.iter().max_by_key(|t| t["width"].as_u64().unwrap()).unwrap();
        assert_eq!(first, widest["url"].as_str().unwrap());
    }

    #[test]
    fn thumbnail_choice_edge_cases() {
        let pick = |thumbs: &str| {
            let json = format!(r#"{{"_type":"video","id":"aaaaaaaaaaa","title":"t","thumbnails":{thumbs}}}"#);
            parse_probe(&json).unwrap().thumbnail_jpg
        };
        // Only webp and png: nothing.
        assert_eq!(pick(r#"[{"url":"https://x/a.webp","width":9,"height":9},{"url":"https://x/b.png"}]"#), None);
        // No thumbnails at all.
        assert_eq!(pick("[]"), None);
        // A `.jpg` that is only in the query string is not a jpg.
        assert_eq!(pick(r#"[{"url":"https://x/a.webp?f=b.jpg","width":9,"height":9}]"#), None);
        // Extension judged on the path, case-insensitively, query ignored.
        assert_eq!(
            pick(r#"[{"url":"https://x/a.JPG?sqp=1","width":10,"height":10},{"url":"https://x/b.jpg","width":5,"height":5}]"#)
                .as_deref(),
            Some("https://x/a.JPG?sqp=1")
        );
        // A bigger webp does not beat a smaller jpg.
        assert_eq!(
            pick(r#"[{"url":"https://x/a.webp","width":100,"height":100},{"url":"https://x/b.jpg","width":5,"height":5}]"#)
                .as_deref(),
            Some("https://x/b.jpg")
        );
        // Unsized jpgs: the best `preference` (yt-dlp lists worst first) wins.
        assert_eq!(
            pick(r#"[{"url":"https://x/a.jpg","preference":-5},{"url":"https://x/b.jpg","preference":-1},{"url":"https://x/c.jpg","preference":-9}]"#)
                .as_deref(),
            Some("https://x/b.jpg")
        );
        // A sized jpg beats an unsized one: only the former is known to exist.
        assert_eq!(
            pick(r#"[{"url":"https://x/a.jpg","preference":0},{"url":"https://x/b.jpg","width":2,"height":2,"preference":-7}]"#)
                .as_deref(),
            Some("https://x/b.jpg")
        );
    }

    #[test]
    fn a_youtube_entry_keeps_its_id_as_identity_and_its_rebuilt_address() {
        let p = parse_probe(&fixture("video.json")).unwrap();
        assert_eq!(p.extractor, "Youtube");
        assert_eq!(p.url.as_deref(), Some("https://www.youtube.com/watch?v=jNQXAC9IVRw"));
        let v = &p.entries[0];
        assert!(v.is_youtube());
        assert_eq!(v.identity(), "jNQXAC9IVRw");
        assert_eq!(v.play_url().as_deref(), Some("https://www.youtube.com/watch?v=jNQXAC9IVRw"));
        let p = parse_probe(&fixture("playlist.json")).unwrap();
        assert_eq!(p.extractor, "YoutubeTab");
        assert!(p.entries.iter().all(|e| e.is_youtube() && e.identity() == e.id));
        assert_eq!(p.entries[0].play_url().as_deref(), Some("https://www.youtube.com/watch?v=fOT0BUpITw8"));
    }

    #[test]
    fn a_soundcloud_track_is_known_by_extractor_and_id_and_plays_its_page() {
        let p = parse_probe(&fixture("soundcloud-track.json")).unwrap();
        assert_eq!((p.kind, p.extractor.as_str(), p.id.as_str()), (Kind::Video, "Soundcloud", "293"));
        assert_eq!((p.title.as_str(), p.channel.as_deref()), ("Flickermood", Some("Forss")));
        let v = &p.entries[0];
        assert_eq!(v.identity(), "Soundcloud:293");
        assert_eq!(v.duration, Some(214));
        assert!(v.available);
        assert_eq!(v.play_url().as_deref(), Some("https://soundcloud.com/forss/flickermood"));
        // On a full answer `url` is the chosen format's media address (removed
        // from the capture, see the fixtures' README): the page is what plays.
        let mut raw: Value = serde_json::from_str(&fixture("soundcloud-track.json")).unwrap();
        raw["url"] = "https://playback.media-streaming.soundcloud.cloud/x/playlist.m3u8".into();
        let full = parse_probe(&raw.to_string()).unwrap();
        assert_eq!(full.entries[0].play_url().as_deref(), Some("https://soundcloud.com/forss/flickermood"));
        // The sized 500x500 beats the unsized "original".
        assert_eq!(
            p.thumbnail_jpg.as_deref(),
            Some("https://i1.sndcdn.com/artworks-000067273316-smsiqx-t500x500.jpg")
        );
    }

    #[test]
    fn a_soundcloud_set_lists_its_tracks_by_their_own_addresses() {
        let p = parse_probe(&fixture("soundcloud-set.json")).unwrap();
        assert_eq!((p.kind, p.extractor.as_str(), p.id.as_str()), (Kind::Playlist, "SoundcloudSet", "2284613"));
        assert_eq!(p.title, "The Royal Concept EP");
        assert_eq!(p.entries.len(), 6);
        assert!(p.entries.iter().all(|e| e.extractor == "Soundcloud" && e.available));
        assert_eq!(p.entries[0].identity(), "Soundcloud:75206121");
        assert_eq!(p.entries[0].play_url().as_deref(), Some("https://soundcloud.com/the-concept-band/world-on-fire-1"));
        // Measured: a flat SoundCloud entry has no title, and the last one of
        // this set is an API address. Both are what yt-dlp said.
        assert_eq!(p.entries[0].title, "");
        assert_eq!(p.entries[5].play_url().as_deref(), Some("https://api-v2.soundcloud.com/tracks/47127631"));
    }

    #[test]
    fn a_bandcamp_track_and_album_are_read() {
        let p = parse_probe(&fixture("bandcamp-track.json")).unwrap();
        assert_eq!((p.kind, p.extractor.as_str()), (Kind::Video, "Bandcamp"));
        let v = &p.entries[0];
        assert_eq!(v.identity(), "Bandcamp:1812978515");
        assert_eq!(v.title, "youtube-dl \"'/\\ä↭ - youtube-dl \"'/\\ä↭ - youtube-dl test song \"'/\\ä↭");
        assert_eq!(v.play_url().as_deref(), Some("https://youtube-dl.bandcamp.com/track/youtube-dl-test-song"));
        assert_eq!(p.thumbnail_jpg.as_deref(), Some("https://f4.bcbits.com/img/a3216802731_5.jpg"));

        let p = parse_probe(&fixture("bandcamp-album.json")).unwrap();
        assert_eq!((p.kind, p.extractor.as_str(), p.id.as_str()), (Kind::Playlist, "BandcampAlbum", "jazz-format-mixtape-vol-1"));
        assert_eq!(p.entries.len(), 22);
        assert_eq!(p.entries[1].title, "Kero One - Keep It Alive (Blazo remix)");
        assert_eq!(p.entries[1].identity(), "Bandcamp:38097443");
        assert!(p.entries.iter().all(|e| e.available && e.play_url().is_some()));
        assert_eq!(
            p.entries[21].play_url().as_deref(),
            Some("https://blazo.bandcamp.com/track/nujabes-the-final-view")
        );
    }

    /// The album capture, with entry `i`'s `url` replaced (or removed).
    fn album_with_url(i: usize, url: Option<&str>) -> Probe {
        let mut raw: Value = serde_json::from_str(&fixture("bandcamp-album.json")).unwrap();
        match url {
            Some(u) => raw["entries"][i]["url"] = u.into(),
            None => {
                raw["entries"][i].as_object_mut().unwrap().remove("url");
            }
        }
        parse_probe(&raw.to_string()).unwrap()
    }

    #[test]
    fn a_trapped_entry_address_has_nothing_to_play() {
        for bad in [
            "https://blazo.bandcamp.com/track/intro\n#EXTINF:1,x\nfile:///etc/passwd",
            "https://blazo.bandcamp.com/track/intro\r\nfile:///etc/passwd",
            "file:///etc/passwd",
            "http://192.168.1.1/admin",
            "https://user:pw@blazo.bandcamp.com/track/intro",
            "intro",
            // A YouTube address under another extractor follows YouTube's rules.
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "https://studio.youtube.com/watch?v=dQw4w9WgXcQ",
        ] {
            let p = album_with_url(0, Some(bad));
            assert_eq!(p.entries[0].play_url(), None, "{bad:?}");
            assert!(p.entries[1].play_url().is_some(), "only the trapped one: {bad:?}");
        }
        assert_eq!(album_with_url(0, None).entries[0].play_url(), None, "no address at all");
        // And a YouTube address under another extractor is rebuilt.
        let p = album_with_url(0, Some("https://youtu.be/dQw4w9WgXcQ?si=x"));
        assert_eq!(p.entries[0].play_url().as_deref(), Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
    }

    #[test]
    fn a_youtube_entry_never_plays_an_address_it_did_not_rebuild() {
        let mut raw: Value = serde_json::from_str(&fixture("playlist.json")).unwrap();
        raw["entries"][0]["id"] = "fOT0BUpITw\n".into();
        raw["entries"][1]["url"] = "https://soundcloud.com/x".into();
        let p = parse_probe(&raw.to_string()).unwrap();
        assert_eq!(p.entries[0].play_url(), None, "its own valid url does not save a bad id");
        assert_eq!(p.entries[1].play_url().as_deref(), Some("https://www.youtube.com/watch?v=NFvDHYMzj9U"));
    }

    #[test]
    fn the_gone_titles_are_youtube_s_rule_only() {
        let mut raw: Value = serde_json::from_str(&fixture("bandcamp-album.json")).unwrap();
        raw["entries"][0]["title"] = "[Private video]".into();
        raw["entries"][1]["availability"] = "needs_auth".into();
        let p = parse_probe(&raw.to_string()).unwrap();
        assert!(p.entries[0].available, "a Bandcamp track may be called that");
        assert!(!p.entries[1].available, "a declared availability holds for every site");
    }

    #[test]
    fn a_live_stream_is_refused_and_a_live_entry_is_unavailable() {
        let with = |field: &str, value: Value| {
            let mut raw: Value = serde_json::from_str(&fixture("soundcloud-track.json")).unwrap();
            raw[field] = value;
            parse_probe(&raw.to_string())
        };
        assert_eq!(with("is_live", true.into()), Err(ToolError::LiveStream));
        assert_eq!(with("live_status", "is_live".into()), Err(ToolError::LiveStream));
        assert_eq!(with("live_status", "is_upcoming".into()), Err(ToolError::LiveStream));
        for (field, value) in [("is_live", Value::from(false)), ("live_status", "was_live".into()), ("live_status", "not_live".into()), ("is_live", Value::Null)] {
            assert!(with(field, value.clone()).is_ok(), "{field} {value}");
        }
        // The real YouTube capture says `is_live: false, live_status: not_live`.
        assert!(parse_probe(&fixture("video.json")).is_ok());

        for (field, value) in [("is_live", Value::from(true)), ("live_status", "is_live".into()), ("live_status", "is_upcoming".into())] {
            let mut raw: Value = serde_json::from_str(&fixture("soundcloud-set.json")).unwrap();
            raw["entries"][2][field] = value.clone();
            let p = parse_probe(&raw.to_string()).unwrap();
            assert!(!p.entries[2].available, "{field} {value}");
            assert!(p.entries[1].available && p.entries[3].available);
        }
    }

    #[test]
    fn a_set_s_untitled_tracks_are_named_after_their_address() {
        let p = parse_probe(&fixture("soundcloud-set.json")).unwrap();
        let names: Vec<Option<String>> = p.entries.iter().map(Video::display_title).collect();
        assert_eq!(
            names,
            [
                Some("world-on-fire-1".to_string()),
                Some("gimme-twice-mastered".into()),
                Some("goldrushed-mastered".into()),
                Some("the-concept-d-d-dance".into()),
                Some("in-the-end-mastered".into()),
                // `/tracks/47127631`: digits only, nothing to show.
                None,
            ]
        );
        // A real title wins, trimmed.
        let p = parse_probe(&fixture("bandcamp-album.json")).unwrap();
        assert_eq!(p.entries[0].display_title().as_deref(), Some("Intro"));
        // Percent-decoded, a control character never shown, `_` kept; no
        // address at all: nothing.
        let mut v = p.entries[0].clone();
        v.title = "  ".into();
        v.url = Some("https://example.org/a/caf%C3%A9_au%0Alait/".into());
        assert_eq!(v.display_title().as_deref(), Some("café_au lait"));
        v.url = None;
        assert_eq!(v.display_title(), None);
        v.url = Some("https://example.org/".into());
        assert_eq!(v.display_title(), None);
        // A YouTube entry with no title: its rebuilt address ends in
        // `watch`, which names nothing ("track n" instead).
        let yt = Video {
            extractor: YOUTUBE.into(),
            id: "dQw4w9WgXcQ".into(),
            title: String::new(),
            duration: None,
            available: true,
            thumbnail_jpg: None,
            url: Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".into()),
        };
        assert_eq!(yt.play_url().as_deref(), Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert_eq!(yt.display_title(), None);
    }

    #[test]
    fn an_entry_without_an_extractor_is_known_as_web() {
        let mut raw: Value = serde_json::from_str(&fixture("bandcamp-album.json")).unwrap();
        raw["entries"][0].as_object_mut().unwrap().remove("ie_key");
        let p = parse_probe(&raw.to_string()).unwrap();
        assert_eq!(p.entries[0].extractor, "");
        assert_eq!(p.entries[0].identity(), "web:1353101989");
    }

    #[test]
    fn an_id_longer_than_256_bytes_is_unavailable() {
        let mut raw: Value = serde_json::from_str(&fixture("bandcamp-album.json")).unwrap();
        raw["entries"][0]["id"] = "9".repeat(ID_MAX_BYTES + 1).into();
        raw["entries"][1]["id"] = "9".repeat(ID_MAX_BYTES).into();
        let p = parse_probe(&raw.to_string()).unwrap();
        assert!(!p.entries[0].available);
        assert!(p.entries[1].available, "256 is still an id");
    }

    #[test]
    fn unreadable_answers_are_failures_not_panics() {
        for bad in ["", "not json", "[]", r#"{"_type":"video"}"#, r#"{"_type":"url","id":"x","title":"t"}"#] {
            assert!(matches!(parse_probe(bad), Err(ToolError::Failed(_))), "{bad:?}");
        }
    }

    #[cfg(unix)]
    mod run {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// A tool directory whose `bin/yt-dlp` is the given shell script.
        fn tool(script: &str) -> (tempfile::TempDir, Ytdlp) {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("bin")).unwrap();
            let y = Ytdlp::new(dir.path());
            std::fs::write(y.bin(), format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(y.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();
            (dir, y)
        }

        #[tokio::test]
        async fn probe_times_out_and_kills_the_child() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("bin")).unwrap();
            let y = Ytdlp::new(dir.path());
            let pidfile = dir.path().join("pid");
            std::fs::write(y.bin(), format!("#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n", pidfile.display())).unwrap();
            std::fs::set_permissions(y.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();

            let started = std::time::Instant::now();
            let r = y.probe("https://www.youtube.com/watch?v=jNQXAC9IVRw", Duration::from_millis(200)).await;
            assert_eq!(r.unwrap_err(), ToolError::Timeout);
            assert!(started.elapsed() < Duration::from_secs(10), "the deadline was not held");

            let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
            let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
            for _ in 0..50 {
                if !proc.exists() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("the child {pid} outlived its deadline");
        }

        #[tokio::test]
        async fn a_probe_deadline_kills_the_wrappers_children_too() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("bin")).unwrap();
            let y = Ytdlp::new(dir.path());
            let pidfile = dir.path().join("grandchild");
            std::fs::write(y.bin(), format!("#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n", pidfile.display())).unwrap();
            std::fs::set_permissions(y.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();
            let r = y.probe("https://www.youtube.com/watch?v=jNQXAC9IVRw", Duration::from_millis(500)).await;
            assert_eq!(r.unwrap_err(), ToolError::Timeout);
            let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
            for _ in 0..50 {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                if stat.map_or(true, |s| s.rsplit_once(')').is_some_and(|(_, r)| r.trim_start().starts_with('Z'))) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("the wrapper's child {pid} outlived the deadline");
        }

        #[tokio::test]
        async fn a_missing_binary_is_reported_as_missing() {
            let dir = tempfile::tempdir().unwrap();
            let y = Ytdlp::new(dir.path());
            assert!(!y.installed());
            let r = y.probe("https://www.youtube.com/watch?v=jNQXAC9IVRw", Duration::from_secs(5)).await;
            assert_eq!(r.unwrap_err(), ToolError::Missing);
        }

        #[tokio::test]
        async fn installed_means_a_file_not_a_directory() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("bin/yt-dlp")).unwrap();
            assert!(!Ytdlp::new(dir.path()).installed());
        }

        #[tokio::test]
        async fn the_command_line_is_exactly_the_documented_one_and_its_answer_is_read() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("bin")).unwrap();
            let y = Ytdlp::new(dir.path());
            let args = dir.path().join("args");
            let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video.json");
            std::fs::write(
                y.bin(),
                format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat '{}'\n", args.display(), fixture_path.display()),
            )
            .unwrap();
            std::fs::set_permissions(y.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(y.installed());

            let p = y.probe("-oops", Duration::from_secs(10)).await.unwrap();
            assert_eq!(p.title, "Me at the zoo");
            assert_eq!(std::fs::read_to_string(&args).unwrap(), "-J\n--flat-playlist\n--\n-oops\n");
        }

        #[tokio::test]
        async fn a_refusal_reports_the_last_non_empty_line_of_stderr() {
            let (_dir, y) = tool("echo 'WARNING: something' >&2\necho 'ERROR: Video unavailable' >&2\necho >&2\nexit 1");
            let r = y.probe("https://www.youtube.com/watch?v=aaaaaaaaaaa", Duration::from_secs(10)).await;
            assert_eq!(r.unwrap_err(), ToolError::Failed("ERROR: Video unavailable".into()));
        }

        #[tokio::test]
        async fn the_wrapper_s_live_refusal_is_a_live_stream() {
            let (_dir, y) = tool(&format!("echo '{LIVE_REFUSAL}' >&2\nexit 1"));
            let r = y.probe("https://soundcloud.com/x", Duration::from_secs(10)).await;
            assert_eq!(r.unwrap_err(), ToolError::LiveStream);
            // Only as the last line: elsewhere it is somebody else's words.
            let (_dir, y) = tool(&format!("echo '{LIVE_REFUSAL}' >&2\necho 'ERROR: other' >&2\nexit 1"));
            let r = y.probe("https://soundcloud.com/x", Duration::from_secs(10)).await;
            assert_eq!(r.unwrap_err(), ToolError::Failed("ERROR: other".into()));
        }

        /// An answer past the cap is refused, at once: the run is killed
        /// rather than left to fill its pipe until the deadline.
        #[tokio::test]
        async fn an_answer_larger_than_the_cap_is_refused_and_its_run_killed() {
            let (_dir, y) = tool("head -c 5000 /dev/zero | tr '\\0' 'x'\nexec sleep 30");
            let started = std::time::Instant::now();
            let r = y.probe_capped("https://a.example/", Duration::from_secs(20), 1000).await;
            let Err(ToolError::Failed(m)) = r else { panic!("{r:?}") };
            assert!(m.starts_with(crate::reason::UNREADABLE_PREFIX), "{m}");
            assert!(started.elapsed() < Duration::from_secs(10), "waited for the deadline");
            // At the cap exactly, the answer is read (and judged as JSON).
            let (_dir, y) = tool(&format!("cat '{}'", Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video.json").display()));
            let size = std::fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video.json")).unwrap().len() as usize;
            assert_eq!(y.probe_capped("https://a.example/", Duration::from_secs(20), size).await.unwrap().title, "Me at the zoo");
            let r = y.probe_capped("https://a.example/", Duration::from_secs(20), size - 1).await;
            assert!(matches!(r, Err(ToolError::Failed(_))), "{r:?}");
        }

        /// A failure that said nothing says how yt-dlp ended, under the
        /// prefix the page codes; a `-J` answer that cannot be read says so
        /// under its own.
        #[tokio::test]
        async fn the_plugins_own_failure_texts_carry_their_codes() {
            let (_dir, y) = tool("exit 3");
            let Err(ToolError::Failed(m)) = y.probe("https://a.example/", Duration::from_secs(10)).await else {
                panic!()
            };
            assert_eq!(crate::reason::coded(&m).code, "exited", "{m}");
            let (_dir, y) = tool("echo '{\"title\": \"x\"}'");
            let Err(ToolError::Failed(m)) = y.probe("https://a.example/", Duration::from_secs(10)).await else {
                panic!()
            };
            assert_eq!(crate::reason::coded(&m).code, "unreadable_output", "{m}");
            let (_dir, y) = tool("echo 'hello'");
            let Err(ToolError::Failed(m)) = y.probe("https://a.example/", Duration::from_secs(10)).await else {
                panic!()
            };
            assert_eq!(crate::reason::coded(&m).code, "unreadable_output", "{m}");
        }

        /// The plugin's own runs say so to the wrapper, which records a
        /// failure only for mpv's (`crate::playfail`).
        #[tokio::test]
        async fn the_plugins_own_runs_are_marked_as_such() {
            let dir = tempfile::tempdir().unwrap();
            let seen = dir.path().join("seen");
            let (_tool, y) = tool(&format!("echo \"${OWN_RUN_ENV}\" > '{}'\nexit 1", seen.display()));
            let _ = y.probe("https://www.youtube.com/watch?v=aaaaaaaaaaa", Duration::from_secs(10)).await;
            assert_eq!(std::fs::read_to_string(&seen).unwrap(), "1\n");
        }

        #[tokio::test]
        async fn output_that_is_not_json_is_a_failure() {
            let (_dir, y) = tool("echo 'hello'\necho 'boom' >&2");
            let r = y.probe("https://www.youtube.com/watch?v=aaaaaaaaaaa", Duration::from_secs(10)).await;
            assert!(matches!(r, Err(ToolError::Failed(_))), "{r:?}");
        }
    }
}
