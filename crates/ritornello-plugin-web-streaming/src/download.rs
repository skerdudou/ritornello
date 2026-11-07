//! Downloading the audio of presets into a folder of a writable share.
//!
//! **One video at a time**, so that one failure never stops a playlist. The
//! command is `<bin> -f <selector> --no-playlist --downloader native
//! --hls-prefer-native --ffmpeg-location /dev/null/ffmpeg --newline
//! --progress-template ... -o <template> -- <address>`: `bin` is the wrapper
//! that already carries the configuration flags. The address is the video's
//! [`Video::play_url`]: YouTube's rebuilt `watch` URL, another site's address
//! judged like an operator's link. One stream, no merge, and **yt-dlp never
//! finds ffmpeg**: no thumbnail is asked of it, no conversion, nothing
//! embedded by it. The selector is the [`Format`]'s: `bestaudio`, or for m4a
//! `bestaudio[ext=m4a]/bestaudio` (the site's m4a stream when it has one).
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
//! **A conversion is the plugin's own, and touches only a local file.** When
//! the operator asked for mp3 (or for m4a and the site had none), the plugin
//! itself runs the system's ffmpeg on the file yt-dlp wrote, once it is
//! complete: `-nostdin -protocol_whitelist file`, both paths spelled
//! `file:...`, into a hidden temporary that is renamed onto `<stem>.<ext>`
//! only if that name is still free, the original then removed. It runs in its
//! own process group, under a deadline, and a cancel kills it like a
//! download. No ffmpeg on the device: the site's own file is kept and the
//! row says so ([`Note`]).
//!
//! **A cut is the same local step** ([`Trim`]): only a single video takes
//! one, the whole video is downloaded (yt-dlp cuts only with ffmpeg, which
//! it never finds), then the plugin's ffmpeg keeps the part asked. When the
//! format does not change, the stream is copied, never re-encoded: an audio
//! packet is ~20 ms, so the cut lands within that of the time asked
//! (measured with ffmpeg 6.1 on webm/opus, m4a/aac, mp3 and ogg/opus: a cut
//! of 3.5 s gave 3.48 to 3.52 s, starting at 0). When it changes, the cut
//! goes into the conversion, which encodes anyway: one pass, not two. A cut
//! is never started without ffmpeg (the admin refuses it, and the preset
//! fails before any download if ffmpeg went away since), nor from a start
//! past the video's known end (ffmpeg would write an empty file and say
//! nothing). A cut that fails keeps the whole file and says so.
//!
//! **The names are the plugin's.** yt-dlp is handed a finished path, not a
//! template to fill from remote data: [`sanitize`] turns a title, or the name
//! the operator typed, into a name that cannot leave its folder, and `%` is
//! doubled so that a title cannot write an output-template field of its own.
//! A name already taken is never overwritten: yt-dlp leaves an existing file
//! alone, and the row says the file was already there.
//!
//! **The cover is fetched here**, from the `jpg` the probe named, with
//! timeouts, through the network guard (`netguard.rs`: never the local
//! network, every redirect judged again). A playlist's is written as
//! `cover.jpg` in its folder, replacing the previous one. Each track's — a
//! single video's, or each video's of a playlist — is its own thumbnail,
//! never the folder's: written into the file with `lofty` (m4a, mp3, opus,
//! ogg) or, as the operator chose or when the container takes none (webm),
//! beside it as `<stem>.jpg`, never over an existing one. `files` reads a
//! track's own picture before the folder's, so each track of a playlist
//! shows its own and the folder keeps a face of its own.
//! A missing or failed cover is never a failure of the download.
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

// ---- what the operator chose -------------------------------------------------

/// The format a download is kept in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// As the site gives it (webm/opus on YouTube): no conversion.
    #[default]
    Original,
    /// The site's m4a stream when it has one, else converted by the plugin
    /// when ffmpeg is there, else the original kept.
    M4a,
    /// The best stream, converted by the plugin with ffmpeg.
    Mp3,
}

impl Format {
    /// What `-f` is handed.
    pub fn selector(self) -> &'static str {
        match self {
            Format::Original | Format::Mp3 => "bestaudio",
            Format::M4a => "bestaudio[ext=m4a]/bestaudio",
        }
    }

    /// The extension the file must end up with; `None` takes what comes.
    pub fn wanted_ext(self) -> Option<&'static str> {
        match self {
            Format::Original => None,
            Format::M4a => Some("m4a"),
            Format::Mp3 => Some("mp3"),
        }
    }
}

/// Where each track's own cover goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverMode {
    /// Written into the audio file, where its container takes one.
    Embedded,
    /// `<stem>.jpg` beside the track: read by `files` whatever the container.
    #[default]
    Separate,
}

/// What the operator chose for one preset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choices {
    pub format: Format,
    pub cover: CoverMode,
    /// The name typed for the file (a video) or the folder (a playlist):
    /// cleaned by [`sanitize`]; empty once cleaned, the default stands.
    pub name: Option<String>,
    /// The part of a single video to keep; `None` keeps it whole.
    pub trim: Option<Trim>,
}

/// The part of a video to keep, in whole seconds from its start: either
/// bound may be left out (from the start, to the end).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trim {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
}

impl Trim {
    /// Keeps everything: no cut to make.
    pub fn is_whole(self) -> bool {
        self.start.unwrap_or(0) == 0 && self.end.is_none()
    }

    /// Keeps nothing: an end at or before the start.
    pub fn is_empty(self) -> bool {
        self.end.is_some_and(|end| end <= self.start.unwrap_or(0))
    }

    /// ffmpeg's output options for the cut, placed after the input: the
    /// stream is read from its start and what falls outside is dropped,
    /// whole packets when copied, exact samples when encoded.
    fn args(self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(start) = self.start.filter(|s| *s > 0) {
            args.extend(["-ss".to_string(), start.to_string()]);
        }
        if let Some(end) = self.end {
            args.extend(["-to".to_string(), end.to_string()]);
        }
        args
    }
}

/// What a row says besides its state: a download that went through, but not
/// quite as asked. Carried to the page as a code (and, for what ffmpeg said,
/// its own words as `detail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum Note {
    /// A file of that name was already there: left as it was, nothing
    /// converted, no cover written.
    AlreadyPresent,
    /// m4a or mp3 was asked and ffmpeg is not installed: the site's own
    /// format was kept.
    NoFfmpeg,
    /// The converted file's name is taken: the original was kept.
    ConvertedNameTaken,
    /// ffmpeg failed, with its last line: the original was kept.
    ConversionFailed(String),
    /// The cut asked could not be made, with why: the whole file was kept.
    TrimFailed(String),
    /// The cover could not go into this container: it was written beside
    /// the track instead.
    CoverBeside,
    /// `<stem>.jpg` exists already: it was kept, no cover written.
    CoverNameTaken,
}

/// The name a preset's file (a video) or folder (a playlist) gets when the
/// operator types none: its title, cleaned, else its id. Computed from the
/// saved preset alone (no network): the page shows it before a download.
pub fn default_name(entry: &Entry) -> String {
    let link = parse_link(&entry.url).ok();
    let fallback = match entry.kind {
        Kind::Video => link
            .as_ref()
            .and_then(|l| l.watch_id().map(str::to_string))
            .or_else(|| entry.identity.as_deref().and_then(|i| i.split_once(':')).map(|(_, id)| id.to_string())),
        Kind::Playlist => link.as_ref().and_then(|l| l.list_id().map(str::to_string).or_else(|| l.cache_key())),
    };
    sanitize(&entry.title, &fallback.unwrap_or_default())
}

/// The arguments of one video's download.
pub fn download_args(dir: &Path, stem: &str, url: &str, format: Format) -> Vec<String> {
    let template = format!("{}/{}.%(ext)s", template_escape(&dir.to_string_lossy()), template_escape(stem));
    [
        "-f",
        format.selector(),
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

/// The containers `lofty` writes a picture into: m4a (`covr`), mp3 (an ID3v2
/// APIC frame), opus and ogg (a METADATA_BLOCK_PICTURE comment). webm, what
/// YouTube's original usually is, is not one of them.
pub const EMBEDDABLE: [&str; 4] = ["m4a", "mp3", "opus", "ogg"];

/// Writes `jpg` into the audio file at `path` as its front cover, in place.
/// Refused before anything is read when the extension is not one of
/// [`EMBEDDABLE`].
pub fn embed_cover(path: &Path, jpg: &[u8]) -> Result<(), String> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, FileType, TaggedFileExt};
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::Tag;
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if !EMBEDDABLE.contains(&ext.as_str()) {
        return Err(format!("no cover is written into a .{ext} file"));
    }
    let mut file = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    if !matches!(file.file_type(), FileType::Mp4 | FileType::Mpeg | FileType::Opus | FileType::Vorbis) {
        return Err(format!("{:?} takes no cover here", file.file_type()));
    }
    let tag_type = file.primary_tag_type();
    if file.tag(tag_type).is_none() {
        file.insert_tag(Tag::new(tag_type));
    }
    let tag = file.tag_mut(tag_type).ok_or("the tag could not be made")?;
    tag.push_picture(Picture::unchecked(jpg.to_vec()).pic_type(PictureType::CoverFront).mime_type(MimeType::Jpeg).build());
    file.save_to_path(path, WriteOptions::default()).map_err(|e| e.to_string())
}

// ---- what yt-dlp wrote, and converting it ----------------------------------

/// Whether `ext`, the part of a name after `<stem>.`, names a finished
/// download: one extension (not yt-dlp's `.webm.part` or `.webm.ytdl`), and
/// not a picture.
fn is_output_ext(ext: &str) -> bool {
    !ext.is_empty()
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && !matches!(ext.to_ascii_lowercase().as_str(), "part" | "ytdl" | "temp" | "tmp" | "jpg" | "jpeg" | "png" | "webp")
}

/// The extensions of the files named `<stem>.<ext>` in `dir`.
fn outputs_of(dir: &Path, stem: &str) -> std::io::Result<std::collections::BTreeSet<String>> {
    let prefix = format!("{stem}.");
    let mut found = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if let Some(ext) = entry.file_name().to_str().and_then(|n| n.strip_prefix(&prefix))
            && is_output_ext(ext)
        {
            found.insert(ext.to_string());
        }
    }
    Ok(found)
}

/// The hidden temporary a conversion writes into, renamed once complete.
fn conversion_temporary(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    dir.join(format!(".{stem}.{ext}.converting"))
}

/// ffmpeg's arguments for turning `from` into `format` at `to`. Only local
/// files: the protocol whitelist and the `file:` spelling keep a name that
/// looks like a protocol from being one, and nothing is read from stdin.
pub fn conversion_args(from: &Path, to: &Path, format: Format, trim: Option<Trim>) -> Vec<String> {
    let codec: &[&str] = match format {
        Format::Mp3 => &["-c:a", "libmp3lame", "-q:a", "2", "-f", "mp3"],
        // AAC in the container yt-dlp itself writes an m4a with.
        Format::M4a | Format::Original => &["-c:a", "aac", "-b:a", "192k", "-f", "ipod"],
    };
    ffmpeg_args(from, to, trim, codec)
}

/// ffmpeg's arguments for keeping `trim` of `from` at `to`, the stream
/// copied into `muxer`: nothing re-encoded. Same guards as a conversion.
pub fn trim_args(from: &Path, to: &Path, trim: Trim, muxer: &str) -> Vec<String> {
    ffmpeg_args(from, to, Some(trim), &["-c", "copy", "-f", muxer])
}

fn ffmpeg_args(from: &Path, to: &Path, trim: Option<Trim>, codec: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-protocol_whitelist", "file", "-i"]
        .map(str::to_string)
        .to_vec();
    args.push(format!("file:{}", from.display()));
    args.extend(["-vn", "-map", "0:a:0"].map(str::to_string));
    args.extend(trim.map(Trim::args).unwrap_or_default());
    args.extend(codec.iter().map(|a| a.to_string()));
    // Never over an existing file, the temporary included.
    args.push("-n".into());
    args.push(format!("file:{}", to.display()));
    args
}

/// The muxer a stream copy of a `.<ext>` file is written with: named, since
/// the temporary's own name tells ffmpeg nothing. Only the containers a cut
/// was measured in; another keeps its whole file, and says so.
fn copy_muxer(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "webm" => "webm",
        "m4a" => "ipod",
        "mp3" => "mp3",
        "opus" | "ogg" => "ogg",
        _ => return None,
    })
}

/// The hidden temporary a cut writes into, renamed over the file once
/// complete.
fn trim_temporary(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    dir.join(format!(".{stem}.{ext}.trimming"))
}

enum Trimmed {
    Done,
    Kept(Note),
    Cancelled,
}

/// Keeps `trim` of `<stem>.<ext>`, the stream copied, in place: the file is
/// the one this download just wrote, so it is replaced, through a hidden
/// temporary renamed over it only once ffmpeg succeeded.
async fn trim_in_place(ctx: &Ctx, ffmpeg: &Path, dir: &Path, stem: &str, ext: &str, trim: Trim, cancel: &Cancel) -> Trimmed {
    let Some(muxer) = copy_muxer(ext) else {
        return Trimmed::Kept(Note::TrimFailed(format!("a .{ext} file is not cut here")));
    };
    let (source, tmp) = (dir.join(format!("{stem}.{ext}")), trim_temporary(dir, stem, ext));
    // A temporary a crash left is ours to clear.
    let leftover = tmp.clone();
    let cleared = blocking(ctx.limits.fs, move || match std::fs::remove_file(&leftover) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    })
    .await;
    if let Err(e) = cleared {
        return Trimmed::Kept(Note::TrimFailed(e.to_string()));
    }
    let args = trim_args(&source, &tmp, trim, muxer);
    let end = run_ytdlp(ffmpeg, &args, cancel, ctx.limits.convert, &mut |_| {}).await;
    let clear = |tmp: PathBuf| async move {
        let _ = blocking(ctx.limits.fs, move || std::fs::remove_file(tmp)).await;
    };
    let why = match end {
        RunEnd::Exited { success: true, .. } => None,
        RunEnd::Cancelled => {
            clear(tmp).await;
            return Trimmed::Cancelled;
        }
        RunEnd::Exited { success: false, last_error } => Some(last_error.unwrap_or_default()),
        RunEnd::TimedOut => Some(reason::TIMED_OUT.to_string()),
        RunEnd::Spawn(m) => Some(m),
    };
    if let Some(why) = why {
        clear(tmp).await;
        return Trimmed::Kept(Note::TrimFailed(why));
    }
    let left = tmp.clone();
    match blocking(ctx.limits.fs, move || std::fs::rename(&tmp, &source)).await {
        Ok(()) => Trimmed::Done,
        Err(e) => {
            clear(left).await;
            Trimmed::Kept(Note::TrimFailed(e.to_string()))
        }
    }
}

enum Converted {
    /// The file is now `<stem>.<ext>`.
    Done(String),
    /// The original is kept, for this reason.
    Kept(Note),
    Cancelled,
}

/// Converts `<stem>.<from>` into the format's extension with ffmpeg; never
/// over an existing file, the original removed only once the new one is in
/// place.
#[allow(clippy::too_many_arguments)]
async fn convert(ctx: &Ctx, ffmpeg: &Path, dir: &Path, stem: &str, from: &str, format: Format, trim: Option<Trim>, cancel: &Cancel) -> Converted {
    let Some(to) = format.wanted_ext() else { return Converted::Done(from.to_string()) };
    let (source, target, tmp) = (dir.join(format!("{stem}.{from}")), dir.join(format!("{stem}.{to}")), conversion_temporary(dir, stem, to));
    let (check, leftover) = (target.clone(), tmp.clone());
    // The name must be free; a temporary a crash left is ours to clear.
    let free = blocking(ctx.limits.fs, move || {
        if check.try_exists()? {
            return Ok(false);
        }
        match std::fs::remove_file(&leftover) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(true),
        }
    })
    .await;
    if free != Ok(true) {
        return Converted::Kept(Note::ConvertedNameTaken);
    }
    let args = conversion_args(&source, &tmp, format, trim);
    let end = run_ytdlp(ffmpeg, &args, cancel, ctx.limits.convert, &mut |_| {}).await;
    let clear = |tmp: PathBuf| async move {
        let _ = blocking(ctx.limits.fs, move || std::fs::remove_file(tmp)).await;
    };
    let why = match end {
        RunEnd::Exited { success: true, .. } => None,
        RunEnd::Cancelled => {
            clear(tmp).await;
            return Converted::Cancelled;
        }
        RunEnd::Exited { success: false, last_error } => Some(last_error.unwrap_or_default()),
        RunEnd::TimedOut => Some(reason::TIMED_OUT.to_string()),
        RunEnd::Spawn(m) => Some(m),
    };
    if let Some(why) = why {
        clear(tmp).await;
        return Converted::Kept(Note::ConversionFailed(why));
    }
    let placed = blocking(ctx.limits.fs, move || {
        // Judged again: the name may have been taken while ffmpeg ran.
        if target.try_exists()? {
            std::fs::remove_file(&tmp)?;
            return Ok(false);
        }
        std::fs::rename(&tmp, &target)?;
        if let Err(e) = std::fs::remove_file(&source) {
            tracing::warn!("original {} not removed after its conversion: {e}", source.display());
        }
        Ok(true)
    })
    .await;
    match placed {
        Ok(true) => Converted::Done(to.to_string()),
        Ok(false) => Converted::Kept(Note::ConvertedNameTaken),
        Err(e) => Converted::Kept(Note::ConversionFailed(e.to_string())),
    }
}

/// A track's own cover: into the file, or beside it as `<stem>.jpg`, never
/// over an existing one. What did not go as asked is said.
async fn place_track_cover(ctx: &Ctx, dir: &Path, stem: &str, ext: &str, mode: CoverMode, url: Option<&str>) -> Vec<Note> {
    let Some(url) = url else { return Vec::new() };
    let mut notes = Vec::new();
    let embeddable = EMBEDDABLE.contains(&ext.to_ascii_lowercase().as_str());
    let beside = dir.join(format!("{stem}.jpg"));
    let beside_free = || {
        let probe = beside.clone();
        // A name that cannot be judged is not free either.
        async move { matches!(blocking(ctx.limits.fs, move || probe.try_exists()).await, Ok(false)) }
    };
    if mode == CoverMode::Separate || !embeddable {
        if mode == CoverMode::Embedded {
            notes.push(Note::CoverBeside);
        }
        if !beside_free().await {
            notes.push(Note::CoverNameTaken);
            return notes;
        }
    }
    let Some(bytes) = (ctx.limits.fetch_cover)(url.to_string()).await else {
        tracing::info!("no cover fetched for {stem}");
        return notes;
    };
    if mode == CoverMode::Embedded && embeddable {
        let (track, jpg) = (dir.join(format!("{stem}.{ext}")), bytes.clone());
        match blocking(ctx.limits.embed, move || embed_cover(&track, &jpg).map_err(std::io::Error::other)).await {
            Ok(()) => return notes,
            Err(e) => {
                tracing::warn!("cover not embedded into {stem}.{ext}: {e}");
                notes.push(Note::CoverBeside);
                if !beside_free().await {
                    notes.push(Note::CoverNameTaken);
                    return notes;
                }
            }
        }
    }
    let written = blocking(ctx.limits.fs, move || {
        // Judged again at the write: a cover is never written over a file.
        if beside.try_exists()? {
            return Ok(false);
        }
        write_atomically(&beside, &bytes).map_err(std::io::Error::other)?;
        Ok(true)
    })
    .await;
    match written {
        Ok(true) => {}
        Ok(false) => notes.push(Note::CoverNameTaken),
        Err(e) => tracing::warn!("cover not written beside {stem}: {e}"),
    }
    notes
}

// ---- the context and the events --------------------------------------------

/// Where the system's ffmpeg is, as the last probe of the machine found it
/// (`tool/runtime.rs`); `None` when it has none.
pub type FfmpegLookup = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

#[derive(Clone)]
pub struct Limits {
    pub probe: Duration,
    pub video: Duration,
    pub fs: Duration,
    /// One conversion by ffmpeg (a long mix on a Pi 2 takes minutes).
    pub convert: Duration,
    /// Writing a cover into a file: `lofty` may rewrite the whole file, on a
    /// share.
    pub embed: Duration,
    pub fetch_cover: CoverFetch,
    pub ffmpeg: FfmpegLookup,
}

impl Default for Limits {
    fn default() -> Self {
        let (connect, total) = (Duration::from_secs(5), Duration::from_secs(20));
        Self {
            probe: crate::ytdlp::PROBE_DEADLINE,
            video: Duration::from_secs(30 * 60),
            fs: Duration::from_secs(10),
            convert: Duration::from_secs(60 * 60),
            embed: Duration::from_secs(120),
            ffmpeg: Arc::new(|| None),
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
    /// Downloaded; ffmpeg turns it into the format asked.
    Converting,
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
    Converting(usize),
    /// Said of a video before it ends: see [`Note`].
    Noted(usize, Note),
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

/// A playlist's cover: `cover.jpg` in its own folder, replacing the previous
/// one (the folder is the playlist's).
async fn place_cover(ctx: &Ctx, dir: &Path, url: Option<&str>) {
    let Some(url) = url else { return };
    let path = dir.join("cover.jpg");
    let Some(bytes) = (ctx.limits.fetch_cover)(url.to_string()).await else {
        tracing::info!("no cover fetched for {}", dir.display());
        return;
    };
    let written = blocking(ctx.limits.fs, move || write_atomically(&path, &bytes).map_err(std::io::Error::other)).await;
    if let Err(e) = written {
        tracing::warn!("cover not written in {}: {e}", dir.display());
    }
}

/// The extensions of `<stem>.*` in `dir`, or `None` when the share did not
/// say (then nothing yt-dlp wrote can be told from what was there).
async fn outputs(ctx: &Ctx, dir: &Path, stem: &str) -> Option<std::collections::BTreeSet<String>> {
    let (dir, stem) = (dir.to_path_buf(), stem.to_string());
    blocking(ctx.limits.fs, move || outputs_of(&dir, &stem)).await.ok()
}

/// What follows a download that went through: the conversion asked, then the
/// track's own cover. `Err(())` is a cancel.
#[allow(clippy::too_many_arguments)]
async fn finish_video(
    ctx: &Ctx,
    dir: &Path,
    stem: &str,
    before: Option<std::collections::BTreeSet<String>>,
    choices: &Choices,
    cover: Option<&str>,
    cancel: &Cancel,
    i: usize,
    sink: &mut (dyn FnMut(Event) + Send),
) -> Result<(), ()> {

    // The file yt-dlp wrote: the one `<stem>.<ext>` that was not there
    // before. None new: the name was taken, and yt-dlp left it alone.
    let Some(before) = before else { return Ok(()) };
    let Some(after) = outputs(ctx, dir, stem).await else { return Ok(()) };
    let new: Vec<&String> = after.difference(&before).collect();
    let mut ext = match new.as_slice() {
        [one] => (*one).clone(),
        [] => {
            sink(Event::Noted(i, Note::AlreadyPresent));
            return Ok(());
        }
        _ => {
            tracing::warn!("several new files for {stem}: {new:?}");
            return Ok(());
        }
    };
    // A cut still to make: done by the conversion when there is one.
    let mut trim = choices.trim.filter(|t| !t.is_whole());
    if let Some(wanted) = choices.format.wanted_ext()
        && !ext.eq_ignore_ascii_case(wanted)
    {
        match (ctx.limits.ffmpeg)() {
            None => sink(Event::Noted(i, Note::NoFfmpeg)),
            Some(ffmpeg) => {
                sink(Event::Converting(i));
                match convert(ctx, &ffmpeg, dir, stem, &ext, choices.format, trim, cancel).await {
                    Converted::Done(to) => {
                        ext = to;
                        trim = None;
                    }
                    Converted::Kept(why) => sink(Event::Noted(i, why)),
                    Converted::Cancelled => return Err(()),
                }
            }
        }
    }
    // The format stays (or its conversion failed): the stream is copied.
    if let Some(trim) = trim {
        match (ctx.limits.ffmpeg)() {
            None => sink(Event::Noted(i, Note::TrimFailed(reason::NO_FFMPEG.into()))),
            Some(ffmpeg) => {
                sink(Event::Converting(i));
                match trim_in_place(ctx, &ffmpeg, dir, stem, &ext, trim, cancel).await {
                    Trimmed::Done => {}
                    Trimmed::Kept(why) => sink(Event::Noted(i, why)),
                    Trimmed::Cancelled => return Err(()),
                }
            }
        }
    }
    for n in place_track_cover(ctx, dir, stem, &ext, choices.cover, cover).await {
        sink(Event::Noted(i, n));
    }
    Ok(())
}

/// Downloads one preset (one video, or every available one of a playlist).
/// Progress is reported through `sink`; a video that fails is an event, not an
/// end.
pub async fn download_preset(
    ctx: &Ctx,
    entry: &Entry,
    choices: &Choices,
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
    // A cut is judged before anything is downloaded: the admin refused what
    // it could, but the preset or the machine may have changed since.
    if let Some(trim) = choices.trim.filter(|t| !t.is_whole()) {
        if playlist {
            return PresetEnd::Failed(reason::TRIM_PLAYLIST.into());
        }
        if (ctx.limits.ffmpeg)().is_none() {
            return PresetEnd::Failed(reason::NO_FFMPEG.into());
        }
        let duration = probe.entries.first().and_then(|v| v.duration);
        if trim.is_empty() || duration.is_some_and(|d| trim.start.unwrap_or(0) >= d) {
            return PresetEnd::Failed(reason::TRIM_OUTSIDE.into());
        }
    }
    let typed = choices.name.as_deref().unwrap_or("");
    let mut planned = plan(&probe.entries, playlist);
    // A single video's file takes the name typed, cleaned like a title; one
    // that cleans to nothing leaves the default.
    if !playlist
        && let [Planned { action: Action::Download { stem, .. }, .. }] = planned.as_mut_slice()
    {
        *stem = sanitize(typed, stem);
    }
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
        // The folder takes the name typed, the same way.
        match subfolder(resolved, sanitize(typed, &sanitize(&probe.title, &probe.id)), ctx.limits.fs).await {
            Ok(d) => d,
            Err(e) => return PresetEnd::Failed(reason::target_error(&e)),
        }
    } else {
        resolved.dir.clone()
    };
    if playlist {
        place_cover(ctx, &dir, probe.thumbnail_jpg.as_deref()).await;
    }
    // Each track's cover is its own thumbnail, never the folder's
    // `cover.jpg`: every other track of the folder would then show it. A
    // playlist's entries carry theirs, and `plan` keeps their order, one line
    // per entry.
    let track_cover = |i: usize| {
        if playlist { probe.entries.get(i).and_then(|v| v.thumbnail_jpg.as_deref()) } else { probe.thumbnail_jpg.as_deref() }
    };

    for (i, p) in planned.iter().enumerate() {
        let Action::Download { stem, url } = &p.action else { continue };
        if cancel.is_cancelled() {
            return PresetEnd::Cancelled;
        }
        sink(Event::Started(i));
        let before = outputs(ctx, &dir, stem).await;
        let args = download_args(&dir, stem, url, choices.format);
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
        if state == VideoState::Done
            && finish_video(ctx, &dir, stem, before, choices, track_cover(i), cancel, i, sink).await.is_err()
        {
            sink(Event::Ended(i, VideoState::Cancelled));
            return PresetEnd::Cancelled;
        }
        sink(Event::Ended(i, state));
    }
    PresetEnd::Finished
}

#[cfg(test)]
mod tests;
