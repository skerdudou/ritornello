//! Downloading the audio of presets into a folder of a writable share.
//!
//! **One video at a time**, so that one failure never stops a playlist. The
//! command is `<bin> -f bestaudio --no-playlist --downloader native
//! --hls-prefer-native --ffmpeg-location /dev/null/ffmpeg --newline
//! --progress-template ... -o <template> -- <address>`: `bin` is the wrapper
//! that already carries the configuration flags. The address is the video's
//! [`Video::play_url`]: YouTube's rebuilt `watch` URL, another site's address
//! judged like an operator's link. One stream, no merge, **no ffmpeg
//! anywhere**: no thumbnail is asked of yt-dlp, no conversion, nothing
//! embedded.
//!
//! **Only yt-dlp's own downloader runs**, so that every byte goes through the
//! network guard of the wrapper (`tool/wrapper.rs`), which sees the sockets
//! of this one Python process and of nothing it would start. Measured in
//! yt-dlp 2026.10.26's source: `--downloader native` makes every protocol
//! use the native downloader (never aria2c, curl, wget), including HLS;
//! `--hls-prefer-native` is the older spelling of the same for HLS, still
//! accepted (hidden from `--help`); and the native HLS downloader still
//! hands a stream it cannot handle (AES-128 without pycryptodomex, which
//! the isolated interpreter never sees) over to ffmpeg **when ffmpeg is
//! found**: `--ffmpeg-location` pointing below `/dev/null`, a path that can
//! never exist, makes yt-dlp see no ffmpeg at all (measured: with a fake
//! ffmpeg on the PATH, `FFmpegFD.available()` turns false). Such a stream
//! then fails, rather than downloading outside the guard. A live stream
//! (always ffmpeg's) is refused before any of this.
//!
//! **The names are the plugin's.** yt-dlp is handed a finished path, not a
//! template to fill from remote data: [`sanitize`] turns a title into a name
//! that cannot leave its folder, and `%` is doubled so that a title cannot
//! write an output-template field of its own.
//!
//! **The cover is fetched here**, from the `jpg` the probe named, with
//! timeouts, through the network guard (`netguard.rs`: never the local
//! network, every redirect judged again), and written as `cover.jpg` (a playlist's folder; or, for a single
//! video, next to it and only if the folder has none yet). A missing or failed
//! cover is never a failure of the download.
//!
//! **A spawn is a process group**, and cancelling or running out of time kills
//! the whole group: the wrapper is a shell script and yt-dlp a child of it,
//! and killing the leader alone would leave the download running.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

use crate::entries::{Entry, ListCache, write_atomically};
use crate::reason;
use crate::link::parse_link;
use crate::netguard::{self, Guard};
use crate::targets::{Resolved, TargetError, blocking};
use crate::ytdlp::{Kind, Probe, ToolError, Video, Ytdlp, kill_group, spawn_in_own_group};

/// The longest a file name (without its number or extension) may be.
const NAME_MAX_BYTES: usize = 120;

/// A name cut at `max` bytes, on a character boundary.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn scrub(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| {
            // The separators and controls the brief names, plus what a
            // Windows share refuses in a name (a title often holds `:` or `?`).
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '$') { '_' } else { c }
        })
        .collect();
    // A leading dot hides the file. A trailing dot or space is refused by a
    // Windows share, and is trimmed after the cut, which may expose one.
    let trimmed = replaced.trim_start_matches(|c: char| c == '.' || c.is_whitespace());
    let cut = truncate_utf8(trimmed, NAME_MAX_BYTES);
    cut.trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_string()
}

/// A file or folder name made from remote text: never empty, never a path,
/// at most 120 bytes. `fallback` (a video id) serves when nothing is left.
pub fn sanitize(name: &str, fallback: &str) -> String {
    let clean = scrub(name);
    if !clean.is_empty() {
        return clean;
    }
    let clean = scrub(fallback);
    if clean.is_empty() { "untitled".to_string() } else { clean }
}

/// `%` is yt-dlp's template escape: doubled, it is a literal percent.
fn template_escape(s: &str) -> String {
    s.replace('%', "%%")
}

/// A percentage and the video it belongs to, from a line of
/// `--progress-template "download:%(progress._percent_str)s %(info.id)s"`.
///
/// yt-dlp strips the `download:` selector and pads the percentage
/// (`"  4.2% jNQXAC9IVRw"`); both spellings are read. Any other line (the
/// `[youtube]` chatter) is `None`.
pub fn parse_progress(line: &str) -> Option<(f32, String)> {
    let line = line.trim();
    let line = line.strip_prefix("download:").unwrap_or(line).trim_start();
    let mut words = line.split_whitespace();
    let percent: f32 = words.next()?.strip_suffix('%')?.parse().ok()?;
    let id = words.next()?;
    if words.next().is_some() || !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        return None;
    }
    Some((percent, id.to_string()))
}

// ---- cancelling ------------------------------------------------------------

/// A cancellation both a waiting task and a polling one can see.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    notify: Notify,
}

impl Cancel {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // A permit is kept if nobody waits yet: the wake-up is never lost.
        self.notify.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            self.notify.notified().await;
        }
    }
}

// ---- running the tool ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    Exited { success: bool, last_error: Option<String> },
    Cancelled,
    TimedOut,
    Spawn(String),
}

/// Runs `bin args` in its own process group, feeding each stdout line to
/// `on_line`; cancelling or the deadline kills the whole group.
pub async fn run_ytdlp(
    bin: &Path,
    args: &[String],
    cancel: &Cancel,
    deadline: Duration,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> RunEnd {
    let mut child = match spawn_in_own_group(bin, args).await {
        Ok(c) => c,
        Err(e) => return RunEnd::Spawn(e.to_string()),
    };
    let pgid = child.id();
    let mut out = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut err = BufReader::new(child.stderr.take().expect("piped")).lines();
    let mut last_error: Option<String> = None;
    let (mut out_open, mut err_open) = (true, true);
    let timer = tokio::time::sleep(deadline);
    tokio::pin!(timer);
    let mut keep_last = |line: String| {
        let line = line.trim();
        if !line.is_empty() {
            last_error = Some(line.to_string());
        }
    };
    let status = loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                kill_group(pgid);
                let _ = child.wait().await;
                return RunEnd::Cancelled;
            }
            () = &mut timer => {
                kill_group(pgid);
                let _ = child.wait().await;
                return RunEnd::TimedOut;
            }
            status = child.wait() => break status,
            line = out.next_line(), if out_open => match line {
                Ok(Some(l)) => on_line(&l),
                _ => out_open = false,
            },
            line = err.next_line(), if err_open => match line {
                Ok(Some(l)) => keep_last(l),
                _ => err_open = false,
            },
        }
    };
    // What was written just before the exit. A straggler that keeps the pipe
    // open must not hold us: each read is short.
    let short = Duration::from_millis(200);
    while err_open {
        match tokio::time::timeout(short, err.next_line()).await {
            Ok(Ok(Some(l))) => keep_last(l),
            _ => err_open = false,
        }
    }
    while out_open {
        match tokio::time::timeout(short, out.next_line()).await {
            Ok(Ok(Some(l))) => on_line(&l),
            _ => out_open = false,
        }
    }
    match status {
        Ok(s) if s.success() => RunEnd::Exited { success: true, last_error: None },
        Ok(s) => RunEnd::Exited {
            success: false,
            last_error: Some(last_error.unwrap_or_else(|| format!("{}{s}", reason::EXIT_PREFIX))),
        },
        Err(e) => RunEnd::Spawn(e.to_string()),
    }
}

/// A path that can never exist (`/dev/null` is not a directory): handed to
/// `--ffmpeg-location`, it makes yt-dlp find no ffmpeg (see the module's
/// comment).
pub const NO_FFMPEG: &str = "/dev/null/ffmpeg";

/// The arguments of one video's download.
pub fn download_args(dir: &Path, stem: &str, url: &str) -> Vec<String> {
    let template = format!("{}/{}.%(ext)s", template_escape(&dir.to_string_lossy()), template_escape(stem));
    [
        "-f",
        "bestaudio",
        "--no-playlist",
        "--downloader",
        "native",
        "--hls-prefer-native",
        "--ffmpeg-location",
        NO_FFMPEG,
        "--newline",
        "--progress-template",
        "download:%(progress._percent_str)s %(info.id)s",
        "-o",
        &template,
        "--",
        url,
    ]
    .map(str::to_string)
    .to_vec()
}

// ---- the plan --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Download { stem: String, url: String },
    Skip(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub id: String,
    pub title: String,
    pub action: Action,
}

/// What to do with each video, in the list's order. Only an available video
/// with an address ([`Video::play_url`]: an id or an address from yt-dlp is
/// remote data, the same rule as playback) is downloaded; the others are
/// skipped, with the reason. A playlist's files are numbered `NN - ` over the
/// *downloaded* ones, on three digits when there are more than 99 (so that
/// they still sort).
pub fn plan(videos: &[Video], numbered: bool) -> Vec<Planned> {
    let ok = |v: &Video| v.available && v.play_url().is_some();
    let count = videos.iter().filter(|v| ok(v)).count();
    let width = if count > 99 { 3 } else { 2 };
    let mut n = 0;
    videos
        .iter()
        .map(|v| {
            let action = if !v.available {
                Action::Skip(reason::UNAVAILABLE.into())
            } else if let Some(url) = v.play_url() {
                n += 1;
                // A set's untitled track is named after its address, else its id.
                let name = sanitize(&v.display_title().unwrap_or_default(), &v.id);
                let stem = if numbered { format!("{n:0width$} - {name}") } else { name };
                Action::Download { stem, url }
            } else {
                Action::Skip(reason::INVALID_ID.into())
            };
            Planned { id: v.id.clone(), title: v.display_title().unwrap_or_else(|| v.title.clone()), action }
        })
        .collect()
}

// ---- covers ----------------------------------------------------------------

pub type CoverFetch = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + Send>> + Send + Sync>;

const COVER_MAX_BYTES: usize = 5 * 1024 * 1024;

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// An address a cover may be fetched from: https, the default port, no
/// credentials, and not a local IP literal ([`netguard::url_ok`]). The
/// address comes from a probe, that is from remote data, of any site: the
/// plugin does not become a way to reach the operator's own network. A name
/// is judged when it resolves, and every redirect again.
pub fn cover_url_ok(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| netguard::url_ok(&u))
}

/// The picture at `url`, when the guard lets every address on the way
/// through, it answers in time, is small enough and really is a jpeg. Every
/// failure is `None`: a cover is a nicety.
pub async fn fetch_jpg(url: &str, connect: Duration, total: Duration, guard: &Guard) -> Option<Vec<u8>> {
    let url = url::Url::parse(url).ok()?;
    if !(guard.url_ok)(&url) {
        return None;
    }
    let client = guard.client(connect, total).ok()?;
    let mut response = client.get(url).send().await.ok()?.error_for_status().ok()?;
    if response.content_length().is_some_and(|n| n > COVER_MAX_BYTES as u64) {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > COVER_MAX_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    is_jpeg(&body).then_some(body)
}

// ---- the context and the events --------------------------------------------

#[derive(Clone)]
pub struct Limits {
    pub probe: Duration,
    pub video: Duration,
    pub fs: Duration,
    pub fetch_cover: CoverFetch,
}

impl Default for Limits {
    fn default() -> Self {
        let (connect, total) = (Duration::from_secs(5), Duration::from_secs(20));
        Self {
            probe: Duration::from_secs(120),
            video: Duration::from_secs(30 * 60),
            fs: Duration::from_secs(10),
            fetch_cover: Arc::new(move |url| {
                Box::pin(async move {
                    fetch_jpg(&url, connect, total, &Guard::system()).await
                })
            }),
        }
    }
}

pub struct Ctx {
    pub ytdlp: Arc<Ytdlp>,
    pub lists: ListCache,
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum VideoState {
    Pending,
    Running,
    Done,
    /// With yt-dlp's last line of stderr.
    Failed(String),
    /// Unavailable, or an id that is not one.
    Skipped(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Planned(Vec<Planned>),
    Started(usize),
    Progress(usize, f32),
    Ended(usize, VideoState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetEnd {
    Finished,
    Cancelled,
    Failed(String),
    /// A video failed and the share no longer holds (gone, read-only, not
    /// answering): the failed video carries the share's reason, the ones
    /// left are skipped with it, and the job stops there rather than
    /// resolving every remaining video only to fail its write.
    ShareLost(String),
}

/// What [`ShareCheck`] answers.
pub type ShareFuture = Pin<Box<dyn Future<Output = Result<(), TargetError>> + Send>>;

/// Whether the share a job writes to still holds, judged as when the job
/// started (the job's resolver again: declared, mounted `rw`, the folder
/// confined), every filesystem call under a deadline. Asked after a video
/// fails, never before one: a healthy share costs nothing.
pub type ShareCheck = dyn Fn() -> ShareFuture + Send + Sync;

/// What is known of a preset's videos: a fresh probe, or failing that what a
/// playlist's cache holds, or the single video the preset itself names.
async fn contents(ctx: &Ctx, entry: &Entry) -> Result<Probe, String> {
    let err = match ctx.ytdlp.probe(&entry.url, ctx.limits.probe).await {
        Ok(p) => return Ok(p),
        // What was cached or saved cannot make a live stream end.
        Err(ToolError::LiveStream) => return Err(reason::LIVE_STREAM.into()),
        Err(e) => e,
    };
    let link = parse_link(&entry.url).map_err(|_| reason::BAD_ADDRESS.to_string())?;
    let (id, videos) = match entry.kind {
        Kind::Playlist => {
            let key = link.cache_key().ok_or(reason::NO_PLAYLIST)?;
            let videos = ctx.lists.load(&key).ok_or_else(|| tool_message(&err))?;
            (link.list_id().unwrap_or(&key).to_string(), videos)
        }
        Kind::Video => {
            if matches!(err, ToolError::Missing) {
                return Err(tool_message(&err));
            }
            let url = link.watch_url().ok_or(reason::NO_VIDEO)?;
            // YouTube's video is rebuilt from its id; another site's plays
            // its link, judged again by `play_url`.
            // The stored identity is `extractor:id`; the id alone is what
            // yt-dlp's progress lines name the video by.
            let (extractor, id) = match (link.watch_id(), entry.identity.as_deref().and_then(|i| i.split_once(':'))) {
                (Some(id), _) => (crate::ytdlp::YOUTUBE.to_string(), id.to_string()),
                (None, Some((extractor, id))) => (extractor.to_string(), id.to_string()),
                (None, None) => (String::new(), url.clone()),
            };
            let video = Video {
                extractor,
                id: id.clone(),
                title: entry.title.clone(),
                duration: None,
                available: true,
                thumbnail_jpg: None,
                url: Some(url),
            };
            (id, vec![video])
        }
    };
    Ok(Probe {
        kind: entry.kind,
        extractor: entry.site.clone().unwrap_or_default(),
        id,
        title: entry.title.clone(),
        channel: entry.channel.clone(),
        url: Some(entry.url.clone()),
        entries: videos,
        thumbnail_jpg: None,
    })
}

fn tool_message(e: &ToolError) -> String {
    match e {
        ToolError::Missing => reason::NOT_INSTALLED.into(),
        ToolError::Timeout => reason::TOOL_TIMEOUT.into(),
        ToolError::Failed(m) => m.clone(),
        ToolError::LiveStream => reason::LIVE_STREAM.into(),
    }
}

/// The playlist's own folder under the target, created if need be and
/// confined: an existing symlink of that name must not lead out of the base.
async fn subfolder(resolved: &Resolved, name: String, deadline: Duration) -> Result<PathBuf, TargetError> {
    let (base, made) = (resolved.base.clone(), resolved.dir.join(name));
    let dir = blocking(deadline, move || {
        std::fs::create_dir_all(&made)?;
        let real = std::fs::canonicalize(&made)?;
        Ok((real.starts_with(&base) && !crate::targets::has_dollar(&real)).then_some(real))
    })
    .await?;
    dir.ok_or(TargetError::Escapes)
}

async fn place_cover(ctx: &Ctx, dir: &Path, url: Option<&str>, replace: bool) {
    let Some(url) = url else { return };
    let path = dir.join("cover.jpg");
    if !replace {
        let probe = path.clone();
        // A folder that already has a cover keeps it; an unreadable one is
        // left alone.
        match blocking(ctx.limits.fs, move || probe.try_exists()).await {
            Ok(false) => {}
            _ => return,
        }
    }
    let Some(bytes) = (ctx.limits.fetch_cover)(url.to_string()).await else {
        tracing::info!("no cover fetched for {}", dir.display());
        return;
    };
    let written = blocking(ctx.limits.fs, move || write_atomically(&path, &bytes).map_err(std::io::Error::other)).await;
    if let Err(e) = written {
        tracing::warn!("cover not written in {}: {e}", dir.display());
    }
}

/// Downloads one preset (one video, or every available one of a playlist).
/// Progress is reported through `sink`; a video that fails is an event, not an
/// end.
pub async fn download_preset(
    ctx: &Ctx,
    entry: &Entry,
    resolved: &Resolved,
    share: &ShareCheck,
    cancel: &Cancel,
    sink: &mut (dyn FnMut(Event) + Send),
) -> PresetEnd {
    if !ctx.ytdlp.installed() {
        return PresetEnd::Failed(reason::NOT_INSTALLED.into());
    }
    let probe = match contents(ctx, entry).await {
        Ok(p) => p,
        Err(m) => return PresetEnd::Failed(m),
    };
    if cancel.is_cancelled() {
        return PresetEnd::Cancelled;
    }
    let playlist = probe.kind == Kind::Playlist;
    let planned = plan(&probe.entries, playlist);
    sink(Event::Planned(planned.clone()));
    let mut any = false;
    for (i, p) in planned.iter().enumerate() {
        match &p.action {
            Action::Skip(reason) => sink(Event::Ended(i, VideoState::Skipped(reason.clone()))),
            Action::Download { .. } => any = true,
        }
    }
    if !any {
        return PresetEnd::Finished;
    }
    let dir = if playlist {
        match subfolder(resolved, sanitize(&probe.title, &probe.id), ctx.limits.fs).await {
            Ok(d) => d,
            Err(e) => return PresetEnd::Failed(reason::target_error(&e)),
        }
    } else {
        resolved.dir.clone()
    };
    place_cover(ctx, &dir, probe.thumbnail_jpg.as_deref(), playlist).await;

    for (i, p) in planned.iter().enumerate() {
        let Action::Download { stem, url } = &p.action else { continue };
        if cancel.is_cancelled() {
            return PresetEnd::Cancelled;
        }
        sink(Event::Started(i));
        let args = download_args(&dir, stem, url);
        let mut on_line = |line: &str| {
            if let Some((percent, id)) = parse_progress(line)
                && id == p.id
            {
                sink(Event::Progress(i, percent));
            }
        };
        let end = run_ytdlp(ctx.ytdlp.bin(), &args, cancel, ctx.limits.video, &mut on_line).await;
        let state = match end {
            RunEnd::Exited { success: true, .. } => VideoState::Done,
            RunEnd::Exited { success: false, last_error } => VideoState::Failed(last_error.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| reason::SILENT.into())),
            RunEnd::TimedOut => VideoState::Failed(reason::TIMED_OUT.into()),
            RunEnd::Spawn(m) => VideoState::Failed(m),
            RunEnd::Cancelled => {
                sink(Event::Ended(i, VideoState::Cancelled));
                return PresetEnd::Cancelled;
            }
        };
        if matches!(state, VideoState::Failed(_))
            && let Err(e) = share().await
        {
            let why = reason::target_error(&e);
            sink(Event::Ended(i, VideoState::Failed(why.clone())));
            for (j, rest) in planned.iter().enumerate().skip(i + 1) {
                if matches!(rest.action, Action::Download { .. }) {
                    sink(Event::Ended(j, VideoState::Skipped(why.clone())));
                }
            }
            return PresetEnd::ShareLost(why);
        }
        sink(Event::Ended(i, state));
    }
    PresetEnd::Finished
}

#[cfg(test)]
mod tests;
