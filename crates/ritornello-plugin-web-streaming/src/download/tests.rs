use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::time::Instant;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn video(id: &str, title: &str, available: bool) -> Video {
    Video { extractor: "Youtube".into(), id: id.into(), title: title.into(), duration: Some(1), available, thumbnail_jpg: None, url: None }
}

// ---- sanitize --------------------------------------------------------------

#[test]
fn hostile_titles_stay_inside_the_target() {
    for (title, expected) in [
        ("../../etc/x", "_.._etc_x"),
        ("a/b", "a_b"),
        ("a\\b", "a_b"),
        ("\u{0}x", "_x"),
        (".", "FALLBACK"),
        ("..", "FALLBACK"),
        ("...", "FALLBACK"),
        ("", "FALLBACK"),
        ("   ", "FALLBACK"),
        (".hidden", "hidden"),
        ("a\nb\tc", "a_b_c"),
        ("What? A: \"title\" <x>|*", "What_ A_ _title_ _x___"),
        ("Mr. Brightside.", "Mr. Brightside"),
        ("Plain title", "Plain title"),
    ] {
        let got = sanitize(title, "FALLBACK");
        assert_eq!(got, expected, "{title:?}");
        assert!(!got.is_empty());
        assert!(!got.contains(['/', '\\', '\0']));
        assert!(got != "." && got != "..");
    }
}

#[test]
fn a_name_is_cut_at_120_bytes_on_a_character_boundary() {
    let long = "é".repeat(100); // 200 bytes
    let got = sanitize(&long, "id");
    assert_eq!(got.len(), 120);
    assert_eq!(got.chars().count(), 60);
    // Three-byte characters: 120 is a boundary, 119 is not.
    let got = sanitize(&"€".repeat(50), "id");
    assert_eq!(got.len(), 120);
    // A cut that lands inside a character backs off to the one before it.
    let got = sanitize(&format!("a{}", "é".repeat(100)), "id");
    assert_eq!(got.len(), 119);
    assert!(got.is_char_boundary(got.len()));
    // Exactly at the limit: untouched.
    assert_eq!(sanitize(&"x".repeat(120), "id").len(), 120);
    assert_eq!(sanitize(&"x".repeat(121), "id").len(), 120);
}

#[test]
fn a_cut_never_ends_on_a_dot_or_a_space() {
    // The 120th byte is a dot: cutting there must not leave it at the end.
    let title = format!("{}.yyy", "x".repeat(119));
    assert_eq!(sanitize(&title, "id"), "x".repeat(119));
    let title = format!("{}  yyy", "x".repeat(119));
    assert_eq!(sanitize(&title, "id"), "x".repeat(119));
}

#[test]
fn the_fallback_is_cleaned_too_and_nothing_is_ever_empty() {
    assert_eq!(sanitize("", "a/b"), "a_b");
    assert_eq!(sanitize("", ""), "untitled");
    assert_eq!(sanitize("..", "."), "untitled");
}

// ---- the command line ------------------------------------------------------

#[test]
fn the_download_command_is_exactly_the_documented_one() {
    let args = download_args(Path::new("/mnt/ritornello/nas/Rock"), "01 - Song", "https://www.youtube.com/watch?v=jNQXAC9IVRw", Format::Original);
    assert_eq!(
        args,
        [
            "-f",
            "bestaudio",
            "--no-playlist",
            "--downloader",
            "native",
            "--hls-prefer-native",
            "--ffmpeg-location",
            "/dev/null/ffmpeg",
            "--newline",
            "--progress-template",
            "download:%(progress._percent_str)s %(info.id)s",
            "-o",
            "/mnt/ritornello/nas/Rock/01 - Song.%(ext)s",
            "--",
            "https://www.youtube.com/watch?v=jNQXAC9IVRw",
        ]
    );
}

#[test]
fn no_ffmpeg_dependent_flag_is_ever_passed() {
    let args = download_args(Path::new("/d"), "n", "https://www.youtube.com/watch?v=jNQXAC9IVRw", Format::Original).join(" ");
    for banned in ["thumbnail", "embed", "convert", "extract-audio", "merge", "-x"] {
        assert!(!args.contains(banned), "{banned}");
    }
}

#[test]
fn a_percent_in_a_name_cannot_write_a_template_field() {
    let args = download_args(Path::new("/d/100%"), "%(id)s - 50%", "https://www.youtube.com/watch?v=jNQXAC9IVRw", Format::Original);
    let template = &args[args.iter().position(|a| a == "-o").unwrap() + 1];
    assert_eq!(template, "/d/100%%/%%(id)s - 50%%.%(ext)s");
}

// ---- progress --------------------------------------------------------------

#[test]
fn progress_lines_are_parsed() {
    // The shapes yt-dlp prints for the template: the `download:` selector is
    // consumed by yt-dlp, the percentage is padded to six characters.
    assert_eq!(parse_progress("  0.0% jNQXAC9IVRw"), Some((0.0, "jNQXAC9IVRw".into())));
    assert_eq!(parse_progress(" 45.3% jNQXAC9IVRw"), Some((45.3, "jNQXAC9IVRw".into())));
    assert_eq!(parse_progress("100.0% jNQXAC9IVRw"), Some((100.0, "jNQXAC9IVRw".into())));
    assert_eq!(parse_progress("download:  7.5% a-b_c-d_e-f"), Some((7.5, "a-b_c-d_e-f".into())));
    for noise in [
        "[youtube] jNQXAC9IVRw: Downloading webpage",
        "[download] Destination: x.m4a",
        "[download] 100% of 1.2MiB",
        "Unknown% jNQXAC9IVRw",
        "N/A% jNQXAC9IVRw",
        "50.0%",
        "150.0% jNQXAC9IVRw",
        "-3.0% jNQXAC9IVRw",
        "50.0% a b",
        "",
    ] {
        assert_eq!(parse_progress(noise), None, "{noise:?}");
    }
}

// ---- the plan --------------------------------------------------------------

#[test]
fn a_playlist_is_numbered_over_its_downloaded_videos_in_order() {
    let videos = [
        video("aaaaaaaaaaa", "First", true),
        video("bbbbbbbbbbb", "[Private video]", false),
        video("not an id!!", "Bad", true),
        video("ccccccccccc", "Third/One", true),
    ];
    let planned = plan(&videos, true);
    let stems: Vec<_> = planned
        .iter()
        .map(|p| match &p.action {
            Action::Download { stem, url } => format!("{stem} <{url}>"),
            Action::Skip(r) => format!("skip {r}"),
        })
        .collect();
    assert_eq!(
        stems,
        [
            "01 - First <https://www.youtube.com/watch?v=aaaaaaaaaaa>",
            "skip unavailable",
            "skip invalid id",
            "02 - Third_One <https://www.youtube.com/watch?v=ccccccccccc>",
        ]
    );
}

#[test]
fn a_single_video_is_not_numbered() {
    let planned = plan(&[video("aaaaaaaaaaa", "Song", true)], false);
    assert_eq!(
        planned[0].action,
        Action::Download { stem: "Song".into(), url: "https://www.youtube.com/watch?v=aaaaaaaaaaa".into() }
    );
}

#[test]
fn numbers_take_three_digits_beyond_99_and_two_up_to_99() {
    let list = |n: usize| -> Vec<Video> { (0..n).map(|i| video(&format!("{i:011}"), "t", true)).collect() };
    let first_and_last = |n: usize| {
        let planned = plan(&list(n), true);
        let stem = |p: &Planned| match &p.action {
            Action::Download { stem, .. } => stem.clone(),
            Action::Skip(_) => unreachable!(),
        };
        (stem(&planned[0]), stem(planned.last().unwrap()))
    };
    assert_eq!(first_and_last(99), ("01 - t".to_string(), "99 - t".to_string()));
    assert_eq!(first_and_last(100), ("001 - t".to_string(), "100 - t".to_string()));
    // The width counts the videos that have an address, not the list.
    let mut hundred = list(100);
    hundred[0].id = "not an id!!".into();
    let planned = plan(&hundred, true);
    assert_eq!(planned[0].action, Action::Skip(reason::INVALID_ID.into()));
    assert!(matches!(&planned[1].action, Action::Download { stem, .. } if stem == "01 - t"), "{:?}", planned[1]);
}

// ---- the cover fetch, against a local server -------------------------------

async fn serve(headers_extra: &'static str, body: Vec<u8>, stall: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                if stall {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                let head = format!("HTTP/1.1 200 OK\r\n{headers_extra}Connection: close\r\n\r\n");
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/c.jpg")
}

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];

/// A guard that lets every address through, so the tests can reach their
/// own server on 127.0.0.1 (a literal: no resolver is called). The resolver
/// stays the guarded one.
fn trusting() -> Guard {
    Guard { url_ok: Arc::new(|_| true), lookup: netguard::system_lookup() }
}

#[tokio::test]
async fn a_jpeg_is_fetched() {
    let url = serve("Content-Type: image/jpeg\r\n", JPEG.to_vec(), false).await;
    assert_eq!(fetch_jpg(&url, Duration::from_secs(2), Duration::from_secs(5), &trusting()).await, Some(JPEG.to_vec()));
}

#[tokio::test]
async fn something_that_is_not_a_jpeg_is_refused() {
    let url = serve("", b"<html>not an image</html>".to_vec(), false).await;
    assert_eq!(fetch_jpg(&url, Duration::from_secs(2), Duration::from_secs(5), &trusting()).await, None);
}

#[tokio::test]
async fn a_server_that_does_not_answer_is_given_up_on() {
    let url = serve("", JPEG.to_vec(), true).await;
    let started = Instant::now();
    assert_eq!(fetch_jpg(&url, Duration::from_millis(200), Duration::from_millis(300), &trusting()).await, None);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn a_picture_that_declares_itself_huge_is_refused() {
    let url = serve("Content-Length: 99999999\r\n", JPEG.to_vec(), false).await;
    assert_eq!(fetch_jpg(&url, Duration::from_secs(2), Duration::from_secs(5), &trusting()).await, None);
}

#[tokio::test]
async fn a_picture_over_the_cap_is_refused_even_when_it_does_not_say_how_big_it_is() {
    // No Content-Length: the body is counted as it arrives.
    let mut body = JPEG.to_vec();
    body.resize(5 * 1024 * 1024 + 1, 7);
    let url = serve("", body, false).await;
    assert_eq!(fetch_jpg(&url, Duration::from_secs(2), Duration::from_secs(10), &trusting()).await, None);
    let mut body = JPEG.to_vec();
    body.resize(5 * 1024 * 1024, 7);
    let url = serve("", body.clone(), false).await;
    assert_eq!(fetch_jpg(&url, Duration::from_secs(2), Duration::from_secs(10), &trusting()).await, Some(body), "exactly the cap is fine");
}

#[tokio::test]
async fn an_address_nobody_listens_on_is_none() {
    assert_eq!(fetch_jpg("http://127.0.0.1:9/x.jpg", Duration::from_millis(300), Duration::from_millis(500), &trusting()).await, None);
}

#[tokio::test]
async fn the_default_fetcher_only_goes_to_https() {
    let url = serve("", JPEG.to_vec(), false).await;
    assert_eq!((Limits::default().fetch_cover)(url).await, None, "a plain http address is not fetched");
}

// ---- running the tool ------------------------------------------------------

struct Fake {
    dir: tempfile::TempDir,
    ytdlp: Arc<Ytdlp>,
}

impl Fake {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let ytdlp = Ytdlp::new(dir.path());
        std::fs::write(ytdlp.bin(), format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(ytdlp.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, ytdlp: Arc::new(ytdlp) }
    }

}

/// Whether a process is gone (a zombie waiting to be reaped counts as gone).
fn gone(pid: &str) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
    }
}

async fn wait_for_file(path: &Path) -> String {
    for _ in 0..100 {
        if let Ok(t) = std::fs::read_to_string(path)
            && !t.trim().is_empty()
        {
            return t.trim().to_string();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never appeared", path.display());
}

async fn assert_gone(pid: &str) {
    for _ in 0..100 {
        if gone(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("process {pid} is still running");
}

fn nobody(_: &str) {}

#[tokio::test]
async fn cancelling_kills_the_whole_process_group_not_just_the_wrapper() {
    // The wrapper is a shell; the "download" is its grandchild.
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let fake = Fake::new(&format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()));
    let cancel = Cancel::new();
    let started = Instant::now();
    let mut sink = nobody;
    let (end, ()) = tokio::join!(
        run_ytdlp(fake.ytdlp.bin(), &[], &cancel, Duration::from_secs(60), &mut sink),
        async {
            wait_for_file(&pidfile).await;
            cancel.cancel();
        }
    );
    assert_eq!(end, RunEnd::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(wait_for_file(&pidfile).await.as_str()).await;
}

#[tokio::test]
async fn the_deadline_kills_the_whole_process_group_too() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let fake = Fake::new(&format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()));
    let started = Instant::now();
    let end = run_ytdlp(fake.ytdlp.bin(), &[], &Cancel::new(), Duration::from_millis(400), &mut nobody).await;
    assert_eq!(end, RunEnd::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(wait_for_file(&pidfile).await.as_str()).await;
}

#[tokio::test]
async fn a_cancel_requested_before_the_run_is_seen() {
    let fake = Fake::new("exec sleep 30");
    let cancel = Cancel::new();
    cancel.cancel();
    let end = run_ytdlp(fake.ytdlp.bin(), &[], &cancel, Duration::from_secs(60), &mut nobody).await;
    assert_eq!(end, RunEnd::Cancelled);
}

#[tokio::test]
async fn a_refusal_reports_the_last_non_empty_line_of_stderr() {
    let fake = Fake::new("echo 'WARNING: x' >&2\necho 'ERROR: Video unavailable' >&2\necho >&2\nexit 1");
    let end = run_ytdlp(fake.ytdlp.bin(), &[], &Cancel::new(), Duration::from_secs(10), &mut nobody).await;
    assert_eq!(end, RunEnd::Exited { success: false, last_error: Some("ERROR: Video unavailable".into()) });
}

#[tokio::test]
async fn a_silent_failure_still_says_something() {
    let fake = Fake::new("exit 3");
    let end = run_ytdlp(fake.ytdlp.bin(), &[], &Cancel::new(), Duration::from_secs(10), &mut nobody).await;
    let RunEnd::Exited { success: false, last_error: Some(m) } = end else { panic!("{end:?}") };
    assert!(m.contains('3'), "{m}");
}

#[tokio::test]
async fn stdout_lines_reach_the_callback_even_those_written_just_before_the_exit() {
    let fake = Fake::new("echo '  5.0% aaaaaaaaaaa'\necho ' 99.0% aaaaaaaaaaa'");
    let mut seen = Vec::new();
    let end = run_ytdlp(fake.ytdlp.bin(), &[], &Cancel::new(), Duration::from_secs(10), &mut |l: &str| seen.push(l.to_string())).await;
    assert_eq!(end, RunEnd::Exited { success: true, last_error: None });
    assert_eq!(seen, ["  5.0% aaaaaaaaaaa", " 99.0% aaaaaaaaaaa"]);
}

#[tokio::test]
async fn a_straggler_holding_the_pipe_does_not_hold_the_run() {
    // The wrapper exits, a background process keeps stdout open.
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("straggler");
    let fake = Fake::new(&format!("sleep 5 &\necho $! > '{}'\nexit 0", pidfile.display()));
    let started = Instant::now();
    let end = run_ytdlp(fake.ytdlp.bin(), &[], &Cancel::new(), Duration::from_secs(30), &mut nobody).await;
    assert_eq!(end, RunEnd::Exited { success: true, last_error: None });
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
}

#[tokio::test]
async fn a_missing_binary_is_a_spawn_failure() {
    let end = run_ytdlp(Path::new("/nonexistent/yt-dlp"), &[], &Cancel::new(), Duration::from_secs(1), &mut nobody).await;
    assert!(matches!(end, RunEnd::Spawn(_)), "{end:?}");
}

// ---- a preset, end to end, against a fake yt-dlp ---------------------------

const PLAYLIST_URL: &str = "https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI";

/// A wrapper that answers a probe with `probe_json` and "downloads" by
/// writing the file the `-o` template names, after `hook`. The hook may set
/// `EXT` (the extension the site gave, `m4a` by default) and `SRC` (a real
/// file copied instead of the word `audio`). Like yt-dlp, it leaves a file
/// that is already there alone. Every call's arguments are logged, one line
/// each, in `<log>.args`.
fn wrapper(probe_json: &Path, log: &Path, hook: &str) -> String {
    format!(
        r#"case "$1" in -J) cat '{probe}'; exit 0;; esac
echo "$*" >> '{log}.args'
out=; url=
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift;; --) url="$2"; break;; esac; shift; done
echo "$url" >> '{log}'
echo "$out" >> '{log}.out'
id="${{url##*=}}"
EXT=m4a; SRC=
{hook}
echo "  42.0% $id"
file=$(printf '%s' "$out" | sed "s/%(ext)s/$EXT/; s/%%/%/g")
[ -e "$file" ] && exit 0
if [ -n "$SRC" ]; then cp "$SRC" "$file"; else printf 'audio' > "$file"; fi
"#,
        probe = probe_json.display(),
        log = log.display(),
    )
}

fn fetch_returning(bytes: Option<Vec<u8>>, calls: Arc<Mutex<Vec<String>>>) -> CoverFetch {
    Arc::new(move |url| {
        calls.lock().unwrap().push(url);
        let bytes = bytes.clone();
        Box::pin(async move { bytes })
    })
}

struct Setup {
    fake: Fake,
    _out: tempfile::TempDir,
    ctx: Ctx,
    resolved: Resolved,
    calls: Arc<Mutex<Vec<String>>>,
    log: PathBuf,
}

fn setup(probe_json: &Path, hook: &str, cover: Option<Vec<u8>>) -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("calls.log");
    let fake = Fake::new(&wrapper(probe_json, &log, hook));
    std::mem::forget(tmp); // the log lives as long as the process; it is in /tmp
    let out = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(out.path()).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let limits = Limits { fetch_cover: fetch_returning(cover, calls.clone()), video: Duration::from_secs(30), ..Limits::default() };
    let ctx = Ctx { ytdlp: fake.ytdlp.clone(), lists: ListCache::new(fake.dir.path()), limits };
    let resolved = Resolved { base: real.clone(), dir: real };
    Setup { fake, _out: out, ctx, resolved, calls, log }
}

fn playlist_entry() -> Entry {
    Entry { preset: 1, url: PLAYLIST_URL.into(), kind: Kind::Playlist, title: "Popular Music Videos".into(), channel: None, count: None, site: None, identity: None }
}

/// A share that holds, whatever happens.
fn share_holds() -> ShareFuture {
    Box::pin(async { Ok(()) })
}

async fn run(s: &Setup, entry: &Entry, cancel: &Cancel) -> (PresetEnd, Vec<Event>) {
    run_choosing(s, entry, &Choices::default(), cancel).await
}

async fn run_choosing(s: &Setup, entry: &Entry, choices: &Choices, cancel: &Cancel) -> (PresetEnd, Vec<Event>) {
    let mut events = Vec::new();
    let end = download_preset(&s.ctx, entry, choices, &s.resolved, &share_holds, cancel, &mut |e| events.push(e)).await;
    (end, events)
}

fn ended(events: &[Event]) -> Vec<(usize, VideoState)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Ended(i, s) => Some((*i, s.clone())),
            _ => None,
        })
        .collect()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_failed_entry_does_not_stop_the_job() {
    // The second video's id, from the capture.
    let probe: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(fixture_path("playlist.json")).unwrap()).unwrap();
    let second = probe["entries"][1]["id"].as_str().unwrap().to_string();
    let s = setup(&fixture_path("playlist.json"), &format!("case \"$url\" in *{second}) echo 'ERROR: boom' >&2; exit 1;; esac"), Some(JPEG.to_vec()));
    let (end, events) = run(&s, &playlist_entry(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    let results = ended(&events);
    assert_eq!(results.len(), 12, "every video ended, the failure included");
    assert_eq!(results[0], (0, VideoState::Done));
    assert_eq!(results[1], (1, VideoState::Failed("ERROR: boom".into())));
    assert!(results[2..].iter().all(|(_, st)| *st == VideoState::Done));
    // Files: in the playlist's own folder, numbered; none for the failure.
    let folder = s.resolved.dir.join("Popular Music Videos");
    let files = files_in(&folder);
    assert_eq!(files.len(), 23, "11 audio files, each with its own picture, and the folder's cover: {files:?}");
    assert!(files.contains(&"cover.jpg".to_string()));
    assert!(files.iter().any(|f| f.starts_with("01 - ") && f.ends_with(".m4a")));
    assert!(!files.iter().any(|f| f.starts_with("02 - ")), "{files:?}");
    assert!(files.iter().any(|f| f.starts_with("03 - ")), "{files:?}");
    // Progress was reported against the right row.
    assert!(events.contains(&Event::Progress(0, 42.0)));
}

#[tokio::test]
async fn the_cover_is_the_probes_jpg_written_as_cover_jpg_in_the_playlist_folder() {
    let s = setup(&fixture_path("playlist.json"), "", Some(JPEG.to_vec()));
    run(&s, &playlist_entry(), &Cancel::new()).await;
    let calls = s.calls.lock().unwrap().clone();
    let probe = crate::ytdlp::parse_probe(&std::fs::read_to_string(fixture_path("playlist.json")).unwrap()).unwrap();
    // The folder's first, then each track's own.
    assert_eq!(calls[0], probe.thumbnail_jpg.unwrap());
    assert!(calls[0].starts_with("https://") && calls[0].contains(".jpg"), "{calls:?}");
    assert_eq!(calls.len(), 1 + probe.entries.len(), "{calls:?}");
    assert_eq!(std::fs::read(s.resolved.dir.join("Popular Music Videos/cover.jpg")).unwrap(), JPEG);
    // No temporary file left beside it.
    assert!(!files_in(&s.resolved.dir.join("Popular Music Videos")).iter().any(|f| f.contains(".tmp.")));
}

#[tokio::test]
async fn a_playlist_cover_replaces_the_previous_one() {
    let s = setup(&fixture_path("playlist.json"), "", Some(JPEG.to_vec()));
    let folder = s.resolved.dir.join("Popular Music Videos");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("cover.jpg"), b"old").unwrap();
    run(&s, &playlist_entry(), &Cancel::new()).await;
    assert_eq!(std::fs::read(folder.join("cover.jpg")).unwrap(), JPEG);
}

#[tokio::test]
async fn a_cover_that_cannot_be_fetched_is_not_a_failure() {
    let s = setup(&fixture_path("playlist.json"), "", None);
    let (end, events) = run(&s, &playlist_entry(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert!(ended(&events).iter().all(|(_, st)| *st == VideoState::Done));
    assert!(!s.resolved.dir.join("Popular Music Videos/cover.jpg").exists());
}

#[tokio::test]
async fn unavailable_videos_are_skipped_and_not_numbered() {
    let s = setup(&fixture_path("playlist-with-unavailable.json"), "", None);
    let entry = Entry { url: "https://www.youtube.com/playlist?list=PLYwq8WOe86_xGmR7FrcJq8Sb7VW8K3Tt2".into(), ..playlist_entry() };
    let (end, events) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    let results = ended(&events);
    let skipped: Vec<usize> = results.iter().filter(|(_, st)| matches!(st, VideoState::Skipped(_))).map(|(i, _)| *i).collect();
    assert_eq!(skipped, [1, 5, 6]);
    assert!(results.iter().any(|(_, st)| *st == VideoState::Skipped("unavailable".into())));
    let log = std::fs::read_to_string(&s.log).unwrap();
    assert_eq!(log.lines().count(), 4, "only the four available ones were run: {log}");
    // The real title ends in dots, which a Windows share refuses: trimmed.
    let folder = s.resolved.dir.join("The Memes Of 2010s");
    let nums: Vec<String> = files_in(&folder).into_iter().filter(|f| f.ends_with(".m4a")).map(|f| f[..2].to_string()).collect();
    assert_eq!(nums, ["01", "02", "03", "04"]);
}

#[tokio::test]
async fn an_id_that_is_not_a_video_id_is_never_run() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("p.json");
    std::fs::write(
        &probe,
        r#"{"_type":"playlist","id":"PL1","title":"L","entries":[
            {"ie_key":"Youtube","id":"x y; rm -rf /","title":"Evil","duration":1},
            {"ie_key":"Youtube","id":"aaaaaaaaaaa","title":"Fine","duration":1}]}"#,
    )
    .unwrap();
    let s = setup(&probe, "", None);
    let entry = Entry { url: "https://www.youtube.com/playlist?list=PL1".into(), ..playlist_entry() };
    let (_, events) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(ended(&events)[0], (0, VideoState::Skipped("invalid id".into())));
    let log = std::fs::read_to_string(&s.log).unwrap();
    assert_eq!(log.trim(), "https://www.youtube.com/watch?v=aaaaaaaaaaa");
}

#[tokio::test]
async fn titles_that_try_to_climb_or_template_stay_in_the_folder_and_literal() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("p.json");
    std::fs::write(
        &probe,
        r#"{"_type":"video","extractor_key":"Youtube","id":"aaaaaaaaaaa","title":"../../escape 100% %(id)s","thumbnails":[]}"#,
    )
    .unwrap();
    let s = setup(&probe, "", None);
    let entry = Entry { url: "https://www.youtube.com/watch?v=aaaaaaaaaaa".into(), kind: Kind::Video, ..playlist_entry() };
    let (end, _) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(files_in(&s.resolved.dir), ["_.._escape 100% %(id)s.m4a"]);
}

fn zoo() -> Entry {
    Entry { url: "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(), kind: Kind::Video, title: "Me at the zoo".into(), ..playlist_entry() }
}

fn notes(events: &[Event]) -> Vec<Note> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Noted(_, n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

fn choosing(format: Format, cover: CoverMode) -> Choices {
    Choices { format, cover, ..Choices::default() }
}

#[tokio::test]
async fn a_single_video_s_cover_is_its_own_picture_and_never_the_folder_s() {
    let s = setup(&fixture_path("video.json"), "", Some(JPEG.to_vec()));
    let (end, events) = run(&s, &zoo(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    // No `cover.jpg`: every other track of the folder would show it.
    assert_eq!(files_in(&s.resolved.dir), ["Me at the zoo.jpg", "Me at the zoo.m4a"], "no subfolder for one video");
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.jpg")).unwrap(), JPEG);
    assert!(notes(&events).is_empty(), "{events:?}");
}

#[tokio::test]
async fn a_name_already_taken_is_left_alone_and_said() {
    let s = setup(&fixture_path("video.json"), "", Some(JPEG.to_vec()));
    std::fs::write(s.resolved.dir.join("Me at the zoo.m4a"), b"mine").unwrap();
    let (end, events) = run(&s, &zoo(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(ended(&events), [(0, VideoState::Done)]);
    assert_eq!(notes(&events), [Note::AlreadyPresent]);
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.m4a")).unwrap(), b"mine");
    // Nothing was put beside what the plugin did not write.
    assert_eq!(files_in(&s.resolved.dir), ["Me at the zoo.m4a"]);
    assert!(s.calls.lock().unwrap().is_empty(), "no cover fetched");
}

#[tokio::test]
async fn a_separate_cover_never_overwrites_a_picture_already_there() {
    let s = setup(&fixture_path("video.json"), "", Some(JPEG.to_vec()));
    std::fs::write(s.resolved.dir.join("Me at the zoo.jpg"), b"mine").unwrap();
    let (_, events) = run(&s, &zoo(), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::CoverNameTaken]);
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.jpg")).unwrap(), b"mine");
    assert!(s.calls.lock().unwrap().is_empty(), "not even fetched");
}

#[tokio::test]
async fn a_picture_that_appears_while_the_cover_is_fetched_is_not_overwritten() {
    // The name was free when looked at; someone writes it during the fetch.
    let mut s = setup(&fixture_path("video.json"), "", None);
    let beside = s.resolved.dir.join("Me at the zoo.jpg");
    s.ctx.limits.fetch_cover = Arc::new(move |_| {
        std::fs::write(&beside, b"mine").unwrap();
        Box::pin(async { Some(JPEG.to_vec()) })
    });
    let (_, events) = run(&s, &zoo(), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::CoverNameTaken]);
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.jpg")).unwrap(), b"mine");
}

#[tokio::test]
async fn an_embedded_cover_goes_beside_a_webm_which_takes_none() {
    let s = setup(&fixture_path("video.json"), "EXT=webm", Some(JPEG.to_vec()));
    let (_, events) = run_choosing(&s, &zoo(), &choosing(Format::Original, CoverMode::Embedded), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::CoverBeside]);
    assert_eq!(files_in(&s.resolved.dir), ["Me at the zoo.jpg", "Me at the zoo.webm"]);
    // And beside it never over a picture already there.
    let s = setup(&fixture_path("video.json"), "EXT=webm", Some(JPEG.to_vec()));
    std::fs::write(s.resolved.dir.join("Me at the zoo.jpg"), b"mine").unwrap();
    let (_, events) = run_choosing(&s, &zoo(), &choosing(Format::Original, CoverMode::Embedded), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::CoverBeside, Note::CoverNameTaken]);
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.jpg")).unwrap(), b"mine");
}

#[tokio::test]
async fn a_cover_that_cannot_be_embedded_goes_beside_the_file() {
    // An m4a that is not one (the fake writes the word `audio`): lofty
    // refuses it, the picture goes beside it.
    let s = setup(&fixture_path("video.json"), "", Some(JPEG.to_vec()));
    let (_, events) = run_choosing(&s, &zoo(), &choosing(Format::Original, CoverMode::Embedded), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::CoverBeside]);
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.jpg")).unwrap(), JPEG);
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.m4a")).unwrap(), b"audio", "the file is untouched");
}

#[tokio::test]
async fn each_track_of_a_playlist_gets_its_own_thumbnail_as_chosen_and_the_folder_its_cover() {
    // The regression: a playlist's tracks had no picture of their own, only
    // the folder's `cover.jpg`, whatever the operator chose.
    let probe = crate::ytdlp::parse_probe(&std::fs::read_to_string(fixture_path("playlist.json")).unwrap()).unwrap();
    let own: Vec<String> = probe.entries.iter().map(|v| v.thumbnail_jpg.clone().unwrap()).collect();
    let s = setup(&fixture_path("playlist.json"), "", Some(JPEG.to_vec()));
    run_choosing(&s, &playlist_entry(), &choosing(Format::Original, CoverMode::Separate), &Cancel::new()).await;
    let folder = s.resolved.dir.join("Popular Music Videos");
    let files = files_in(&folder);
    assert!(files.contains(&"cover.jpg".to_string()), "{files:?}");
    let tracks: Vec<&String> = files.iter().filter(|f| f.ends_with(".m4a")).collect();
    assert_eq!(tracks.len(), probe.entries.len());
    for t in tracks {
        let picture = folder.join(t.replace(".m4a", ".jpg"));
        assert_eq!(std::fs::read(&picture).unwrap(), JPEG, "{}", picture.display());
    }
    // Each track's picture is fetched from its own entry's thumbnail, in order.
    assert_eq!(s.calls.lock().unwrap()[1..], own[..]);

    // Into the file: tried for each track (the fake's m4a takes none, so each
    // one says its picture went beside it).
    let s = setup(&fixture_path("playlist.json"), "", Some(JPEG.to_vec()));
    let (_, events) = run_choosing(&s, &playlist_entry(), &choosing(Format::Original, CoverMode::Embedded), &Cancel::new()).await;
    assert_eq!(notes(&events), vec![Note::CoverBeside; probe.entries.len()]);
}

// ---- the names typed --------------------------------------------------------

#[tokio::test]
async fn a_typed_name_is_cleaned_like_a_title_and_stays_in_the_folder() {
    let s = setup(&fixture_path("video.json"), "", Some(JPEG.to_vec()));
    let choices = Choices { name: Some("../My $HOME/clip 100% %(id)s".into()), ..Choices::default() };
    let (end, _) = run_choosing(&s, &zoo(), &choices, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(files_in(&s.resolved.dir), ["_My _HOME_clip 100% %(id)s.jpg", "_My _HOME_clip 100% %(id)s.m4a"]);
    let template = std::fs::read_to_string(format!("{}.out", s.log.display())).unwrap();
    assert!(template.trim().ends_with("/_My _HOME_clip 100%% %%(id)s.%(ext)s"), "{template}");
    assert!(!template.contains('$'), "{template}");
}

#[tokio::test]
async fn a_typed_name_that_cleans_to_nothing_leaves_the_default() {
    for blank in ["", "   ", "..", " . "] {
        let s = setup(&fixture_path("video.json"), "", None);
        let choices = Choices { name: Some(blank.into()), ..Choices::default() };
        run_choosing(&s, &zoo(), &choices, &Cancel::new()).await;
        assert_eq!(files_in(&s.resolved.dir), ["Me at the zoo.m4a"], "{blank:?}");
    }
}

#[tokio::test]
async fn a_playlist_s_typed_name_is_its_folder_and_its_tracks_keep_their_numbers() {
    let s = setup(&fixture_path("playlist.json"), "", None);
    let choices = Choices { name: Some("Mine/../x".into()), ..Choices::default() };
    run_choosing(&s, &playlist_entry(), &choices, &Cancel::new()).await;
    assert_eq!(files_in(&s.resolved.dir), ["Mine_.._x"]);
    let files = files_in(&s.resolved.dir.join("Mine_.._x"));
    assert!(files[0].starts_with("01 - "), "{files:?}");
    // Blank: the playlist's title.
    let s = setup(&fixture_path("playlist.json"), "", None);
    run_choosing(&s, &playlist_entry(), &Choices { name: Some("  ".into()), ..Choices::default() }, &Cancel::new()).await;
    assert_eq!(files_in(&s.resolved.dir), ["Popular Music Videos"]);
}

#[test]
fn the_default_name_is_the_preset_s_title_cleaned_else_its_id() {
    let video = |title: &str| Entry { title: title.into(), ..zoo() };
    assert_eq!(default_name(&video("Me at the zoo")), "Me at the zoo");
    assert_eq!(default_name(&video("a/b: $x")), "a_b_ _x");
    assert_eq!(default_name(&video("..")), "jNQXAC9IVRw");
    assert_eq!(default_name(&Entry { title: String::new(), ..playlist_entry() }), "PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI");
    let other = Entry { url: "https://soundcloud.com/forss/flickermood".into(), title: String::new(), identity: Some("Soundcloud:293".into()), ..zoo() };
    assert_eq!(default_name(&other), "293");
}

// ---- formats and ffmpeg -----------------------------------------------------

#[test]
fn each_format_asks_its_stream_and_never_hands_ffmpeg_to_yt_dlp() {
    let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
    for (format, selector) in [
        (Format::Original, "bestaudio"),
        (Format::M4a, "bestaudio[ext=m4a]/bestaudio"),
        (Format::Mp3, "bestaudio"),
    ] {
        let args = download_args(Path::new("/d"), "n", url, format);
        assert_eq!(args[..2], ["-f".to_string(), selector.to_string()], "{format:?}");
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
        assert_eq!(after("--ffmpeg-location"), Some(NO_FFMPEG), "{format:?}");
        let joined = args.join(" ");
        for banned in ["thumbnail", "embed", "convert", "extract-audio", "audio-format", "merge", "-x", "postprocessor"] {
            assert!(!joined.contains(banned), "{format:?}: {banned}");
        }
    }
}

#[test]
fn the_conversion_reads_and_writes_only_local_files_and_never_overwrites() {
    let args = conversion_args(Path::new("/d/a:b.webm"), Path::new("/d/.a:b.mp3.converting"), Format::Mp3, None);
    assert_eq!(
        args,
        [
            "-nostdin", "-hide_banner", "-loglevel", "error", "-protocol_whitelist", "file", "-i", "file:/d/a:b.webm",
            "-vn", "-map", "0:a:0", "-c:a", "libmp3lame", "-q:a", "2", "-f", "mp3", "-n", "file:/d/.a:b.mp3.converting",
        ]
    );
    let args = conversion_args(Path::new("/d/x.webm"), Path::new("/d/.x.m4a.converting"), Format::M4a, None);
    let joined = args.join(" ");
    assert!(joined.contains("-c:a aac") && joined.contains("-f ipod"), "{joined}");
}

/// A fake ffmpeg that logs its arguments to `log` and writes `converted` to
/// the path of its last argument, after `hook`; refusing, like `-n`, a path
/// that exists.
fn fake_ffmpeg(dir: &Path, log: &Path, hook: &str) -> PathBuf {
    crate::tool::testkit::write_script(
        dir,
        "ffmpeg",
        &format!(
            "echo \"$*\" >> '{log}'\nfor a; do last=\"$a\"; done\nout=\"${{last#file:}}\"\n{hook}\n[ -e \"$out\" ] && exit 1\nprintf converted > \"$out\"",
            log = log.display()
        ),
    )
}

struct WithFfmpeg {
    s: Setup,
    log: PathBuf,
    _bin: tempfile::TempDir,
}

fn with_ffmpeg(hook: &str, ffmpeg_hook: &str) -> WithFfmpeg {
    let mut s = setup(&fixture_path("video.json"), hook, Some(JPEG.to_vec()));
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("ffmpeg.log");
    let ffmpeg = fake_ffmpeg(bin.path(), &log, ffmpeg_hook);
    s.ctx.limits.ffmpeg = Arc::new(move || Some(ffmpeg.clone()));
    WithFfmpeg { s, log, _bin: bin }
}

#[tokio::test]
async fn mp3_is_converted_by_the_plugin_and_the_original_removed() {
    let w = with_ffmpeg("EXT=webm", "");
    let (end, events) = run_choosing(&w.s, &zoo(), &choosing(Format::Mp3, CoverMode::Separate), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert!(events.contains(&Event::Converting(0)), "{events:?}");
    assert_eq!(ended(&events), [(0, VideoState::Done)]);
    assert!(notes(&events).is_empty(), "{events:?}");
    assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.jpg", "Me at the zoo.mp3"]);
    assert_eq!(std::fs::read(w.s.resolved.dir.join("Me at the zoo.mp3")).unwrap(), b"converted");
    // ffmpeg was the plugin's: yt-dlp never saw it.
    let dir = w.s.resolved.dir.display().to_string();
    let ran = std::fs::read_to_string(&w.log).unwrap();
    assert_eq!(
        ran.trim(),
        format!(
            "-nostdin -hide_banner -loglevel error -protocol_whitelist file -i file:{dir}/Me at the zoo.webm -vn -map 0:a:0 -c:a libmp3lame -q:a 2 -f mp3 -n file:{dir}/.Me at the zoo.mp3.converting"
        )
    );
    let ytdlp = std::fs::read_to_string(format!("{}.args", w.s.log.display())).unwrap();
    assert!(ytdlp.contains("--ffmpeg-location /dev/null/ffmpeg") && !ytdlp.contains(&w._bin.path().display().to_string()), "{ytdlp}");
}

#[tokio::test]
async fn without_ffmpeg_the_site_s_format_is_kept_and_said() {
    for format in [Format::Mp3, Format::M4a] {
        let s = setup(&fixture_path("video.json"), "EXT=webm", None);
        let (end, events) = run_choosing(&s, &zoo(), &choosing(format, CoverMode::Separate), &Cancel::new()).await;
        assert_eq!(end, PresetEnd::Finished);
        assert_eq!(notes(&events), [Note::NoFfmpeg], "{format:?}");
        assert_eq!(files_in(&s.resolved.dir), ["Me at the zoo.webm"], "{format:?}");
    }
}

#[tokio::test]
async fn m4a_from_the_site_is_kept_as_it_is_and_another_format_converted() {
    let w = with_ffmpeg("EXT=m4a", "");
    let (_, events) = run_choosing(&w.s, &zoo(), &choosing(Format::M4a, CoverMode::Separate), &Cancel::new()).await;
    assert!(!events.contains(&Event::Converting(0)) && notes(&events).is_empty(), "{events:?}");
    assert!(!w.log.exists(), "ffmpeg was not run");
    assert_eq!(std::fs::read(w.s.resolved.dir.join("Me at the zoo.m4a")).unwrap(), b"audio");
    let w = with_ffmpeg("EXT=webm", "");
    let (_, events) = run_choosing(&w.s, &zoo(), &choosing(Format::M4a, CoverMode::Separate), &Cancel::new()).await;
    assert!(events.contains(&Event::Converting(0)), "{events:?}");
    assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.jpg", "Me at the zoo.m4a"]);
    assert!(std::fs::read_to_string(&w.log).unwrap().contains("-c:a aac"));
    // The original format: nothing converted, whatever came.
    let w = with_ffmpeg("EXT=webm", "");
    run_choosing(&w.s, &zoo(), &choosing(Format::Original, CoverMode::Separate), &Cancel::new()).await;
    assert!(!w.log.exists(), "ffmpeg was not run");
}

#[tokio::test]
async fn a_conversion_never_overwrites_a_file_of_its_name() {
    let w = with_ffmpeg("EXT=webm", "");
    std::fs::write(w.s.resolved.dir.join("Me at the zoo.mp3"), b"mine").unwrap();
    let (_, events) = run_choosing(&w.s, &zoo(), &choosing(Format::Mp3, CoverMode::Separate), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::ConvertedNameTaken]);
    assert_eq!(std::fs::read(w.s.resolved.dir.join("Me at the zoo.mp3")).unwrap(), b"mine");
    assert_eq!(std::fs::read(w.s.resolved.dir.join("Me at the zoo.webm")).unwrap(), b"audio", "the original kept");
    assert!(!w.log.exists(), "ffmpeg was not even run");
}

#[tokio::test]
async fn a_name_taken_while_ffmpeg_ran_is_not_overwritten_either() {
    // ffmpeg writes its temporary, and meanwhile someone writes the name.
    let mut s = setup(&fixture_path("video.json"), "EXT=webm", Some(JPEG.to_vec()));
    let target = s.resolved.dir.join("Me at the zoo.mp3");
    let bin = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(bin.path(), &bin.path().join("log"), &format!("printf mine > '{}'", target.display()));
    s.ctx.limits.ffmpeg = Arc::new(move || Some(ffmpeg.clone()));
    let (_, events) = run_choosing(&s, &zoo(), &choosing(Format::Mp3, CoverMode::Separate), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::ConvertedNameTaken]);
    assert_eq!(std::fs::read(&target).unwrap(), b"mine");
    assert_eq!(std::fs::read(s.resolved.dir.join("Me at the zoo.webm")).unwrap(), b"audio", "the original kept");
    assert!(!files_in(&s.resolved.dir).iter().any(|f| f.contains("converting")), "the temporary is gone");
}

#[tokio::test]
async fn a_failed_conversion_keeps_the_original_and_says_why() {
    let w = with_ffmpeg("EXT=webm", "echo 'Unknown encoder libmp3lame' >&2; printf half > \"$out\"; exit 1");
    let (end, events) = run_choosing(&w.s, &zoo(), &choosing(Format::Mp3, CoverMode::Separate), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(ended(&events), [(0, VideoState::Done)]);
    assert_eq!(notes(&events), [Note::ConversionFailed("Unknown encoder libmp3lame".into())]);
    assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.jpg", "Me at the zoo.webm"], "no temporary left");
}

#[tokio::test]
async fn a_cancel_kills_ffmpeg_s_whole_group_and_keeps_the_original() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let w = with_ffmpeg("EXT=webm", &format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()));
    let cancel = Cancel::new();
    let started = Instant::now();
    let (entry, choices) = (zoo(), choosing(Format::Mp3, CoverMode::Separate));
    let ((end, events), ()) = tokio::join!(run_choosing(&w.s, &entry, &choices, &cancel), async {
        wait_for_file(&pidfile).await;
        cancel.cancel();
    });
    assert_eq!(end, PresetEnd::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(wait_for_file(&pidfile).await.as_str()).await;
    assert!(events.contains(&Event::Ended(0, VideoState::Cancelled)), "{events:?}");
    assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.webm"]);
}

#[tokio::test]
async fn a_conversion_that_runs_out_of_time_is_killed_and_said() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let mut w = with_ffmpeg("EXT=webm", &format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()));
    w.s.ctx.limits.convert = Duration::from_millis(500);
    let (end, events) = run_choosing(&w.s, &zoo(), &choosing(Format::Mp3, CoverMode::Separate), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(notes(&events), [Note::ConversionFailed(reason::TIMED_OUT.into())]);
    assert_gone(wait_for_file(&pidfile).await.as_str()).await;
}

// ---- cutting ----------------------------------------------------------------

fn cutting(format: Format, start: Option<u32>, end: Option<u32>) -> Choices {
    Choices { trim: Some(Trim { start, end }), ..choosing(format, CoverMode::Separate) }
}

#[test]
fn a_cut_is_two_output_options_and_leaves_out_what_keeps_everything() {
    let cut = |start, end| Trim { start, end }.args().join(" ");
    assert_eq!(cut(Some(83), Some(300)), "-ss 83 -to 300");
    assert_eq!(cut(Some(83), None), "-ss 83");
    assert_eq!(cut(None, Some(300)), "-to 300");
    assert_eq!(cut(Some(0), Some(300)), "-to 300");
    assert!(Trim { start: Some(0), end: None }.is_whole() && Trim::default().is_whole());
    assert!(!Trim { start: Some(1), end: None }.is_whole() && !Trim { start: None, end: Some(9) }.is_whole());
    for (start, end, empty) in [(Some(5), Some(5), true), (Some(6), Some(5), true), (None, Some(0), true), (Some(4), Some(5), false), (Some(9), None, false)] {
        assert_eq!(Trim { start, end }.is_empty(), empty, "{start:?} {end:?}");
    }
}

#[test]
fn a_cut_copies_the_stream_and_a_conversion_takes_it_in_its_one_pass() {
    let args = trim_args(Path::new("/d/a.webm"), Path::new("/d/.a.webm.trimming"), Trim { start: Some(2), end: Some(5) }, "webm");
    assert_eq!(
        args,
        [
            "-nostdin", "-hide_banner", "-loglevel", "error", "-protocol_whitelist", "file", "-i", "file:/d/a.webm",
            "-vn", "-map", "0:a:0", "-ss", "2", "-to", "5", "-c", "copy", "-f", "webm", "-n", "file:/d/.a.webm.trimming",
        ]
    );
    let args = conversion_args(Path::new("/d/a.webm"), Path::new("/d/.a.mp3.converting"), Format::Mp3, Some(Trim { start: Some(2), end: None }));
    assert!(args.join(" ").contains("-map 0:a:0 -ss 2 -c:a libmp3lame"), "{args:?}");
}

#[tokio::test]
async fn a_cut_of_the_site_s_format_copies_the_stream_in_place() {
    for (hook, format, ext, muxer) in [("EXT=webm", Format::Original, "webm", "webm"), ("EXT=m4a", Format::M4a, "m4a", "ipod")] {
        let w = with_ffmpeg(hook, "");
        let (end, events) = run_choosing(&w.s, &zoo(), &cutting(format, Some(2), Some(5)), &Cancel::new()).await;
        assert_eq!(end, PresetEnd::Finished);
        assert_eq!(ended(&events), [(0, VideoState::Done)]);
        assert!(events.contains(&Event::Converting(0)) && notes(&events).is_empty(), "{events:?}");
        let file = format!("Me at the zoo.{ext}");
        assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.jpg", file.as_str()], "no temporary left");
        assert_eq!(std::fs::read(w.s.resolved.dir.join(&file)).unwrap(), b"converted", "{ext}: the cut replaced the whole");
        let dir = w.s.resolved.dir.display().to_string();
        assert_eq!(
            std::fs::read_to_string(&w.log).unwrap().trim(),
            format!(
                "-nostdin -hide_banner -loglevel error -protocol_whitelist file -i file:{dir}/{file} -vn -map 0:a:0 -ss 2 -to 5 -c copy -f {muxer} -n file:{dir}/.{file}.trimming"
            )
        );
    }
}

#[tokio::test]
async fn a_cut_with_a_conversion_is_one_ffmpeg_run() {
    let w = with_ffmpeg("EXT=webm", "");
    let (_, events) = run_choosing(&w.s, &zoo(), &cutting(Format::Mp3, None, Some(5)), &Cancel::new()).await;
    assert!(notes(&events).is_empty(), "{events:?}");
    assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.jpg", "Me at the zoo.mp3"]);
    let ran = std::fs::read_to_string(&w.log).unwrap();
    assert_eq!(ran.lines().count(), 1, "{ran}");
    assert!(ran.contains("-map 0:a:0 -to 5 -c:a libmp3lame") && ran.contains(".Me at the zoo.mp3.converting"), "{ran}");
}

#[tokio::test]
async fn a_cut_that_fails_keeps_the_whole_file_and_says_why() {
    let w = with_ffmpeg("EXT=webm", "echo 'Invalid duration' >&2; printf half > \"$out\"; exit 1");
    let (end, events) = run_choosing(&w.s, &zoo(), &cutting(Format::Original, Some(2), None), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(ended(&events), [(0, VideoState::Done)]);
    assert_eq!(notes(&events), [Note::TrimFailed("Invalid duration".into())]);
    assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.jpg", "Me at the zoo.webm"], "no temporary left");
    assert_eq!(std::fs::read(w.s.resolved.dir.join("Me at the zoo.webm")).unwrap(), b"audio", "the whole kept");
    // A failed conversion still leaves the cut to try, on the original.
    let w = with_ffmpeg("EXT=webm", "echo 'Unknown encoder libmp3lame' >&2; exit 1");
    let (_, events) = run_choosing(&w.s, &zoo(), &cutting(Format::Mp3, Some(2), None), &Cancel::new()).await;
    assert_eq!(
        notes(&events),
        [Note::ConversionFailed("Unknown encoder libmp3lame".into()), Note::TrimFailed("Unknown encoder libmp3lame".into())]
    );
    assert_eq!(std::fs::read_to_string(&w.log).unwrap().lines().count(), 2);
}

#[tokio::test]
async fn a_container_never_measured_is_not_cut_and_said() {
    let w = with_ffmpeg("EXT=flac", "");
    let (_, events) = run_choosing(&w.s, &zoo(), &cutting(Format::Original, Some(2), None), &Cancel::new()).await;
    assert_eq!(notes(&events), [Note::TrimFailed("a .flac file is not cut here".into())]);
    assert!(!w.log.exists(), "ffmpeg was not run");
    assert_eq!(std::fs::read(w.s.resolved.dir.join("Me at the zoo.flac")).unwrap(), b"audio");
}

#[tokio::test]
async fn a_cut_that_cannot_be_made_fails_the_preset_before_any_download() {
    // A playlist, no ffmpeg, a start at or past the video's 19 s (the
    // capture's duration), nothing kept: yt-dlp never downloads.
    // What the preset is comes from its probe: the playlist's capture.
    let w = with_ffmpeg("", "");
    let mut list = setup(&fixture_path("playlist.json"), "", None);
    list.ctx.limits.ffmpeg = w.s.ctx.limits.ffmpeg.clone();
    let s = setup(&fixture_path("video.json"), "", None);
    let cases: [(&Setup, Entry, Choices, &str); 5] = [
        (&list, playlist_entry(), cutting(Format::Original, Some(2), None), reason::TRIM_PLAYLIST),
        (&s, zoo(), cutting(Format::Original, Some(2), None), reason::NO_FFMPEG),
        (&w.s, zoo(), cutting(Format::Original, Some(19), None), reason::TRIM_OUTSIDE),
        (&w.s, zoo(), cutting(Format::Original, Some(25), Some(30)), reason::TRIM_OUTSIDE),
        (&w.s, zoo(), cutting(Format::Original, Some(5), Some(5)), reason::TRIM_OUTSIDE),
    ];
    for (setup, entry, choices, why) in cases {
        let (end, events) = run_choosing(setup, &entry, &choices, &Cancel::new()).await;
        assert_eq!(end, PresetEnd::Failed(why.into()), "{choices:?}");
        assert!(events.is_empty(), "{events:?}");
    }
    for setup in [&w.s, &list, &s] {
        assert!(!setup.log.exists(), "nothing downloaded");
    }
    // Just inside the video, it goes through.
    let (end, _) = run_choosing(&w.s, &zoo(), &cutting(Format::Original, Some(18), None), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
}

#[tokio::test]
async fn a_cut_that_keeps_everything_runs_nothing() {
    let w = with_ffmpeg("EXT=webm", "");
    let (_, events) = run_choosing(&w.s, &zoo(), &cutting(Format::Original, Some(0), None), &Cancel::new()).await;
    assert!(!events.contains(&Event::Converting(0)) && notes(&events).is_empty(), "{events:?}");
    assert!(!w.log.exists(), "ffmpeg was not run");
}

#[tokio::test]
async fn a_cancel_during_a_cut_keeps_the_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let w = with_ffmpeg("EXT=webm", &format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()));
    let cancel = Cancel::new();
    let (entry, choices) = (zoo(), cutting(Format::Original, Some(2), None));
    let ((end, _), ()) = tokio::join!(run_choosing(&w.s, &entry, &choices, &cancel), async {
        wait_for_file(&pidfile).await;
        cancel.cancel();
    });
    assert_eq!(end, PresetEnd::Cancelled);
    assert_gone(wait_for_file(&pidfile).await.as_str()).await;
    assert_eq!(files_in(&w.s.resolved.dir), ["Me at the zoo.webm"], "no temporary left");
    assert_eq!(std::fs::read(w.s.resolved.dir.join("Me at the zoo.webm")).unwrap(), b"audio");
}

// ---- the real ffmpeg and lofty ------------------------------------------------

/// The system's ffmpeg, when the machine has one with the encoders used here
/// (CI installs it; elsewhere these tests say so and pass).
fn real_ffmpeg() -> Option<PathBuf> {
    let path: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
    let bin = crate::tool::runtime::which("ffmpeg", &path)?;
    let ok = std::process::Command::new(&bin).args(["-hide_banner", "-encoders"]).output().ok()?;
    let list = String::from_utf8_lossy(&ok.stdout).into_owned();
    (list.contains("libopus") && list.contains("libmp3lame")).then_some(bin)
}

/// A real 16x16 jpeg, made by the real ffmpeg.
fn real_jpeg(ffmpeg: &Path, dir: &Path) -> Vec<u8> {
    let out = dir.join("cover.jpg");
    let status = std::process::Command::new(ffmpeg)
        .args(["-nostdin", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=red:s=16x16", "-frames:v", "1"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(out).unwrap()
}

/// One second of a tone, made by the real ffmpeg with `codec` into `muxer`.
fn tone(ffmpeg: &Path, out: &Path, codec: &str, muxer: &str) {
    let status = std::process::Command::new(ffmpeg)
        .args(["-nostdin", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1", "-c:a", codec, "-f", muxer])
        .arg(out)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn a_real_cut_keeps_the_part_asked_copied_or_converted() {
    let Some(ffmpeg) = real_ffmpeg() else {
        eprintln!("ffmpeg missing: skipping test");
        return;
    };
    use lofty::file::AudioFile;
    let src = tempfile::tempdir().unwrap();
    // Ten seconds, of which 2 to 5 are kept: three, give or take a packet.
    for (codec, muxer, ext, format, kept) in [
        ("libopus", "ogg", "opus", Format::Original, "opus"),
        ("aac", "ipod", "m4a", Format::M4a, "m4a"),
        ("libmp3lame", "mp3", "mp3", Format::Original, "mp3"),
        ("libopus", "webm", "webm", Format::Mp3, "mp3"),
    ] {
        let source = src.path().join(format!("ten.{ext}"));
        let status = std::process::Command::new(&ffmpeg)
            .args(["-nostdin", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=10", "-c:a", codec, "-f", muxer, "-y"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let mut s = setup(&fixture_path("video.json"), &format!("EXT={ext}; SRC='{}'", source.display()), None);
        let bin = ffmpeg.clone();
        s.ctx.limits.ffmpeg = Arc::new(move || Some(bin.clone()));
        let (end, events) = run_choosing(&s, &zoo(), &cutting(format, Some(2), Some(5)), &Cancel::new()).await;
        assert_eq!(end, PresetEnd::Finished);
        assert!(notes(&events).is_empty(), "{ext}: {events:?}");
        let file = format!("Me at the zoo.{kept}");
        assert_eq!(files_in(&s.resolved.dir), [file.as_str()], "{ext}");
        let ms = lofty::read_from_path(s.resolved.dir.join(&file)).unwrap().properties().duration().as_millis();
        assert!((2900..=3100).contains(&ms), "{ext} -> {kept}: {ms} ms kept of 10 s, 3 s asked");
    }
}

fn front_cover(path: &Path) -> Option<(lofty::file::FileType, Vec<u8>)> {
    use lofty::file::TaggedFileExt;
    let file = lofty::read_from_path(path).unwrap();
    let picture = file.tags().iter().flat_map(|t| t.pictures()).next()?.data().to_vec();
    Some((file.file_type(), picture))
}

#[tokio::test]
async fn a_real_webm_becomes_a_real_mp3_or_m4a_with_its_cover_inside() {
    let Some(ffmpeg) = real_ffmpeg() else {
        eprintln!("ffmpeg missing: skipping test");
        return;
    };
    let src = tempfile::tempdir().unwrap();
    let webm = src.path().join("tone.webm");
    tone(&ffmpeg, &webm, "libopus", "webm");
    let jpeg = real_jpeg(&ffmpeg, src.path());
    for (format, ext, kind) in [(Format::Mp3, "mp3", lofty::file::FileType::Mpeg), (Format::M4a, "m4a", lofty::file::FileType::Mp4)] {
        let mut s = setup(&fixture_path("video.json"), &format!("EXT=webm; SRC='{}'", webm.display()), Some(jpeg.clone()));
        let bin = ffmpeg.clone();
        s.ctx.limits.ffmpeg = Arc::new(move || Some(bin.clone()));
        let (end, events) = run_choosing(&s, &zoo(), &choosing(format, CoverMode::Embedded), &Cancel::new()).await;
        assert_eq!(end, PresetEnd::Finished);
        assert!(notes(&events).is_empty(), "{format:?}: {events:?}");
        let name = format!("Me at the zoo.{ext}");
        assert_eq!(files_in(&s.resolved.dir), std::slice::from_ref(&name), "{format:?}: only the converted file, its cover inside");
        assert_eq!(front_cover(&s.resolved.dir.join(&name)), Some((kind, jpeg.clone())), "{format:?}");
    }
}

#[tokio::test]
async fn a_real_opus_takes_its_cover_inside_as_it_is() {
    let Some(ffmpeg) = real_ffmpeg() else {
        eprintln!("ffmpeg missing: skipping test");
        return;
    };
    let src = tempfile::tempdir().unwrap();
    let opus = src.path().join("tone.opus");
    tone(&ffmpeg, &opus, "libopus", "ogg");
    let jpeg = real_jpeg(&ffmpeg, src.path());
    let s = setup(&fixture_path("video.json"), &format!("EXT=opus; SRC='{}'", opus.display()), Some(jpeg.clone()));
    let (_, events) = run_choosing(&s, &zoo(), &choosing(Format::Original, CoverMode::Embedded), &Cancel::new()).await;
    assert!(notes(&events).is_empty(), "{events:?}");
    assert_eq!(files_in(&s.resolved.dir), ["Me at the zoo.opus"]);
    assert_eq!(front_cover(&s.resolved.dir.join("Me at the zoo.opus")), Some((lofty::file::FileType::Opus, jpeg)));
}

#[test]
fn only_the_containers_lofty_writes_into_are_tried() {
    let dir = tempfile::tempdir().unwrap();
    for ext in ["webm", "mka", "flac", "wav"] {
        let p = dir.path().join(format!("x.{ext}"));
        std::fs::write(&p, b"audio").unwrap();
        let e = embed_cover(&p, JPEG).unwrap_err();
        assert!(e.contains(&format!(".{ext}")), "{e}");
        assert_eq!(std::fs::read(&p).unwrap(), b"audio");
    }
}

#[tokio::test]
async fn a_failing_probe_falls_back_to_what_the_preset_knows() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-probe.json"); // `cat` fails: the probe fails
    let s = setup(&missing, "", None);
    let entry = Entry { url: "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(), kind: Kind::Video, title: "Me at the zoo".into(), ..playlist_entry() };
    let (end, events) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(ended(&events), [(0, VideoState::Done)]);
    // A playlist with nothing cached has nothing to fall back to.
    let (end, _) = run(&s, &playlist_entry(), &Cancel::new()).await;
    assert!(matches!(end, PresetEnd::Failed(_)), "{end:?}");
    // Cached, it goes on.
    s.ctx.lists.store("PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI", &[video("aaaaaaaaaaa", "Cached", true)]).unwrap();
    let (end, events) = run(&s, &playlist_entry(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(ended(&events), [(0, VideoState::Done)]);
}

#[tokio::test]
async fn a_playlist_folder_that_is_a_symlink_out_of_the_base_is_refused() {
    let s = setup(&fixture_path("playlist.json"), "", Some(JPEG.to_vec()));
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), s.resolved.dir.join("Popular Music Videos")).unwrap();
    let (end, _) = run(&s, &playlist_entry(), &Cancel::new()).await;
    assert!(matches!(end, PresetEnd::Failed(_)), "{end:?}");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0, "nothing was written outside");
}

#[tokio::test]
async fn without_the_tool_a_preset_fails_at_once() {
    let s = setup(&fixture_path("video.json"), "", None);
    std::fs::remove_file(s.fake.ytdlp.bin()).unwrap();
    let (end, events) = run(&s, &playlist_entry(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Failed("yt-dlp is not installed".into()));
    assert!(events.is_empty());
}

#[tokio::test]
async fn cancel_kills_the_running_download() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let hook = format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display());
    let s = setup(&fixture_path("playlist.json"), &hook, None);
    let cancel = Cancel::new();
    let entry = playlist_entry();
    let started = Instant::now();
    let ((end, events), ()) = tokio::join!(run(&s, &entry, &cancel), async {
        wait_for_file(&pidfile).await;
        cancel.cancel();
    });
    assert_eq!(end, PresetEnd::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_gone(wait_for_file(&pidfile).await.as_str()).await;
    assert!(events.contains(&Event::Ended(0, VideoState::Cancelled)));
    let log = std::fs::read_to_string(&s.log).unwrap();
    assert_eq!(log.lines().count(), 1, "the next videos were not started: {log}");
}

#[tokio::test]
async fn progress_of_another_video_is_not_ours() {
    // A line naming another id (a stray line, an older run) moves no bar.
    let s = setup(&fixture_path("video.json"), "echo '  77.0% zzzzzzzzzzz'", None);
    let entry = Entry { url: "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(), kind: Kind::Video, title: "Me at the zoo".into(), ..playlist_entry() };
    let (_, events) = run(&s, &entry, &Cancel::new()).await;
    assert!(!events.contains(&Event::Progress(0, 77.0)), "{events:?}");
    assert!(events.contains(&Event::Progress(0, 42.0)));
}

#[tokio::test]
async fn a_cancel_between_two_videos_starts_no_further_one() {
    let s = setup(&fixture_path("playlist.json"), "", None);
    let cancel = Cancel::new();
    let mut events = Vec::new();
    let entry = playlist_entry();
    let end = download_preset(&s.ctx, &entry, &Choices::default(), &s.resolved, &share_holds, &cancel, &mut |e| {
        if matches!(e, Event::Ended(0, VideoState::Done)) {
            cancel.cancel();
        }
        events.push(e);
    })
    .await;
    assert_eq!(end, PresetEnd::Cancelled);
    assert!(!events.iter().any(|e| matches!(e, Event::Started(1) | Event::Ended(1, _))), "{events:?}");
    assert_eq!(std::fs::read_to_string(&s.log).unwrap().lines().count(), 1);
}

#[test]
fn a_dollar_in_a_title_never_reaches_a_template() {
    // Measured with the real yt-dlp: `$HOME` and `${PATH}` in `-o` are
    // expanded (and `$$` is no escape), so none may survive.
    for (title, expected) in [("$HOME", "_HOME"), ("${PATH}/x", "_{PATH}_x"), ("cost $5", "cost _5"), ("$$", "__")] {
        assert_eq!(sanitize(title, "id"), expected, "{title:?}");
    }
    let args = download_args(Path::new("/d"), &sanitize("$HOME ${PATH}", "id"), "https://www.youtube.com/watch?v=jNQXAC9IVRw", Format::Original);
    assert!(!args[7].contains('$'), "{}", args[7]);
}

#[tokio::test]
async fn the_output_template_given_to_the_tool_holds_no_dollar() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("p.json");
    std::fs::write(
        &probe,
        r#"{"_type":"playlist","id":"PL1","title":"$HOME and ${PATH}","entries":[
            {"ie_key":"Youtube","id":"aaaaaaaaaaa","title":"$HOME ${PATH} ~","duration":1}]}"#,
    )
    .unwrap();
    let s = setup(&probe, "", None);
    let entry = Entry { url: "https://www.youtube.com/playlist?list=PL1".into(), ..playlist_entry() };
    let (end, _) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    let logged = std::fs::read_to_string(format!("{}.out", s.log.display())).unwrap();
    assert_eq!(logged.lines().count(), 1);
    assert!(!logged.contains('$'), "{logged}");
    // One flat folder, one file in it: nothing was split into nested folders.
    assert_eq!(files_in(&s.resolved.dir), ["_HOME and _{PATH}"]);
    assert_eq!(files_in(&s.resolved.dir.join("_HOME and _{PATH}")), ["01 - _HOME _{PATH} ~.m4a"]);
}

#[test]
fn a_cover_is_fetched_from_any_public_https_host_and_never_from_a_local_literal() {
    // The hosts of every capture in tests/fixtures.
    for fixture in ["video.json", "video-formats.json", "playlist.json", "playlist-with-unavailable.json"] {
        let text = std::fs::read_to_string(fixture_path(fixture)).unwrap();
        let probe = crate::ytdlp::parse_probe(&text).unwrap();
        let url = probe.thumbnail_jpg.expect("a captured thumbnail");
        assert!(cover_url_ok(&url), "{url}");
        for v in probe.entries.iter().filter_map(|v| v.thumbnail_jpg.as_deref()) {
            assert!(cover_url_ok(v), "{v}");
        }
    }
    for ok in [
        "https://i.ytimg.com/vi/x/hq.jpg",
        "https://i1.ytimg.com/vi/x/hq.jpg",
        "https://i9.ytimg.com/vi/x/hq.jpg",
        "https://img.youtube.com/vi/x/0.jpg",
        "https://yt3.ggpht.com/a/b.jpg",
        // Any site now: the guard is the network's, not a list of hosts.
        "https://f4.bcbits.com/img/a1_10.jpg",
        "https://i1.sndcdn.com/artworks-x-t500x500.jpg",
        // A name is judged when it resolves (the resolver drops it).
        "https://localhost/hq.jpg",
    ] {
        assert!(cover_url_ok(ok), "{ok}");
    }
    for bad in [
        "http://i.ytimg.com/vi/x/hq.jpg",
        "https://i.ytimg.com@192.168.0.1/hq.jpg",
        "https://user:pw@i.ytimg.com/hq.jpg",
        "https://user@i.ytimg.com/hq.jpg",
        "https://i.ytimg.com:8443/hq.jpg",
        "https://i.ytimg.com:443x/hq.jpg",
        "https://192.168.0.1/hq.jpg",
        "https://127.0.0.1/hq.jpg",
        "https://[::ffff:192.168.0.1]/hq.jpg",
        "https://[64:ff9b::7f00:1]/hq.jpg",
        "https://[2002:a00:1::]/hq.jpg",
        "ftp://i.ytimg.com/hq.jpg",
        "not a url",
        "",
    ] {
        assert!(!cover_url_ok(bad), "{bad}");
    }
}

#[tokio::test]
async fn a_playlist_folder_that_is_a_symlink_to_a_folder_with_a_dollar_is_refused() {
    // Inside the base, but a real path yt-dlp would expand.
    let s = setup(&fixture_path("playlist.json"), "", None);
    let real = s.resolved.dir.join("real$dir");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, s.resolved.dir.join("Popular Music Videos")).unwrap();
    let (end, _) = run(&s, &playlist_entry(), &Cancel::new()).await;
    assert!(matches!(end, PresetEnd::Failed(_)), "{end:?}");
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
}

// ---- the network guard, end to end ------------------------------------------

/// A server on 127.0.0.1 where `/r/<n>` redirects to `/r/<n - 1>`,
/// `/to/<address>` redirects to that address, and `/r/0` serves a jpeg.
async fn redirecting() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let location = if let Some(to) = path.strip_prefix("/to/") {
                    Some(to.to_string())
                } else {
                    match path.strip_prefix("/r/").and_then(|n| n.parse::<u32>().ok()) {
                        Some(0) | None => None,
                        Some(n) => Some(format!("/r/{}", n - 1)),
                    }
                };
                let head = match &location {
                    Some(l) => format!("HTTP/1.1 302 Found\r\nLocation: {l}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                    None => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", JPEG.len()),
                };
                let _ = sock.write_all(head.as_bytes()).await;
                if location.is_none() {
                    let _ = sock.write_all(JPEG).await;
                }
            });
        }
    });
    port
}

async fn fetch_with(url: &str, guard: &Guard) -> Option<Vec<u8>> {
    fetch_jpg(url, Duration::from_secs(2), Duration::from_secs(5), guard).await
}

#[tokio::test]
async fn redirects_are_followed_up_to_five() {
    let port = redirecting().await;
    let url = |n| format!("http://127.0.0.1:{port}/r/{n}");
    assert_eq!(fetch_with(&url(0), &trusting()).await, Some(JPEG.to_vec()), "no redirect");
    assert_eq!(fetch_with(&url(5), &trusting()).await, Some(JPEG.to_vec()), "five redirects");
    assert_eq!(fetch_with(&url(6), &trusting()).await, None, "six");
}

#[tokio::test]
async fn every_redirect_is_judged_by_the_guard() {
    let port = redirecting().await;
    // A judge that refuses only the last hop: the first address is fetched,
    // the redirect is refused.
    let guard = Guard { url_ok: Arc::new(|u: &url::Url| u.path() != "/r/0"), lookup: netguard::system_lookup() };
    assert_eq!(fetch_with(&format!("http://127.0.0.1:{port}/r/1"), &guard).await, None);
    assert_eq!(fetch_with(&format!("http://127.0.0.1:{port}/r/1"), &trusting()).await, Some(JPEG.to_vec()));
    // The production judge, behind a first address it is told to let
    // through: the hop to a local literal (which no resolver would see) is
    // refused.
    let first = format!("http://127.0.0.1:{port}/to/http://127.0.0.1:{port}/r/0");
    let allowed = first.clone();
    let guard = Guard {
        url_ok: Arc::new(move |u: &url::Url| u.as_str() == allowed || netguard::url_ok(u)),
        lookup: netguard::system_lookup(),
    };
    assert_eq!(fetch_with(&first, &guard).await, None);
    assert_eq!(fetch_with(&first, &trusting()).await, Some(JPEG.to_vec()), "the hop itself works");
}

#[tokio::test]
async fn a_name_that_resolves_to_the_local_network_is_not_reached() {
    let port = redirecting().await;
    // The literal is reached (the trusting judge lets it through); the same
    // server by a name is not: the resolver drops 127.0.0.1 and ::1.
    assert_eq!(fetch_with(&format!("http://127.0.0.1:{port}/r/0"), &trusting()).await, Some(JPEG.to_vec()));
    assert_eq!(fetch_with(&format!("http://localhost:{port}/r/0"), &trusting()).await, None);
    // Nor through a redirect to that name.
    let hop = format!("http://127.0.0.1:{port}/to/http://localhost:{port}/r/0");
    assert_eq!(fetch_with(&hop, &trusting()).await, None);
}

// ---- other sites than YouTube ----------------------------------------------

const ALBUM_URL: &str = "https://blazo.bandcamp.com/album/jazz-format-mixtape-vol-1";

fn album_entry() -> Entry {
    Entry { url: ALBUM_URL.into(), title: "Jazz Format Mixtape vol.1".into(), site: Some("BandcampAlbum".into()), ..playlist_entry() }
}

#[test]
fn only_yt_dlp_s_own_downloader_may_run_and_ffmpeg_is_never_found() {
    let args = download_args(Path::new("/d"), "n", "https://soundcloud.com/forss/flickermood", Format::Original);
    let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
    assert_eq!(after("--downloader"), Some("native"));
    assert!(args.iter().any(|a| a == "--hls-prefer-native"));
    assert_eq!(after("--ffmpeg-location"), Some(NO_FFMPEG));
    assert!(!Path::new(NO_FFMPEG).exists() && !Path::new(NO_FFMPEG).parent().unwrap().is_dir(), "a path that cannot exist");
    assert_eq!(args.iter().filter(|a| a.contains("downloader")).count(), 1, "{args:?}");
}

#[test]
fn another_site_s_entries_download_from_their_own_validated_address() {
    let p = crate::ytdlp::parse_probe(&std::fs::read_to_string(fixture_path("bandcamp-album.json")).unwrap()).unwrap();
    let planned = plan(&p.entries, true);
    assert_eq!(planned.len(), 22);
    assert_eq!(
        planned[1].action,
        Action::Download {
            stem: "02 - Kero One - Keep It Alive (Blazo remix)".into(),
            url: "https://blazo.bandcamp.com/track/kero-one-keep-it-alive-blazo-remix".into(),
        }
    );
    let mut trapped = p.entries.clone();
    trapped[0].url = Some("https://blazo.bandcamp.com/track/intro\n#EXTINF:1,x".into());
    trapped[2].url = Some("file:///etc/passwd".into());
    let planned = plan(&trapped, true);
    assert_eq!(planned[0].action, Action::Skip(reason::INVALID_ID.into()));
    assert_eq!(planned[2].action, Action::Skip(reason::INVALID_ID.into()));
    assert!(matches!(&planned[1].action, Action::Download { stem, .. } if stem.starts_with("01 - ")), "numbered over the downloaded ones");
}

#[tokio::test]
async fn a_bandcamp_album_downloads_each_track_into_its_folder() {
    let s = setup(&fixture_path("bandcamp-album.json"), "", None);
    let (end, events) = run(&s, &album_entry(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert!(ended(&events).iter().all(|(_, st)| *st == VideoState::Done), "{events:?}");
    let log = std::fs::read_to_string(&s.log).unwrap();
    let urls: Vec<&str> = log.lines().collect();
    assert_eq!(urls.len(), 22);
    assert_eq!(urls[0], "https://blazo.bandcamp.com/track/intro");
    assert_eq!(urls[21], "https://blazo.bandcamp.com/track/nujabes-the-final-view");
    let files = files_in(&s.resolved.dir.join("Jazz Format Mixtape vol.1"));
    assert_eq!(files.len(), 22, "{files:?}");
    assert_eq!(files[0], "01 - Intro.m4a");
}

#[tokio::test]
async fn a_trapped_entry_address_is_never_handed_to_the_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("p.json");
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_path("bandcamp-album.json")).unwrap()).unwrap();
    raw["entries"][0]["url"] = "https://blazo.bandcamp.com/track/intro\n#EXTINF:1,x\nfile:///etc/passwd".into();
    raw["entries"][1]["url"] = "file:///etc/passwd".into();
    raw["entries"][2]["url"] = "http://192.168.1.1/admin".into();
    std::fs::write(&probe, raw.to_string()).unwrap();
    let s = setup(&probe, "", None);
    let (end, events) = run(&s, &album_entry(), &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    let results = ended(&events);
    for (i, result) in results.iter().take(3).enumerate() {
        assert_eq!(*result, (i, VideoState::Skipped(reason::INVALID_ID.into())));
    }
    let log = std::fs::read_to_string(&s.log).unwrap();
    assert_eq!(log.lines().count(), 19, "{log}");
    assert!(!log.contains("passwd") && !log.contains("EXTINF") && !log.contains("192.168"), "{log}");
}

#[tokio::test]
async fn a_live_stream_is_not_downloaded_even_from_what_the_preset_knows() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("p.json");
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_path("soundcloud-track.json")).unwrap()).unwrap();
    raw["live_status"] = "is_live".into();
    std::fs::write(&probe, raw.to_string()).unwrap();
    let s = setup(&probe, "", None);
    let entry = Entry {
        url: "https://soundcloud.com/forss/flickermood".into(),
        kind: Kind::Video,
        title: "Flickermood".into(),
        ..playlist_entry()
    };
    let (end, events) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Failed(reason::LIVE_STREAM.into()));
    assert!(events.is_empty(), "{events:?}");
    assert!(!s.log.exists(), "nothing was run");
}

#[tokio::test]
async fn another_site_s_video_falls_back_to_its_own_link() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-probe.json");
    let s = setup(&missing, "", None);
    let entry = Entry {
        url: "https://soundcloud.com/forss/flickermood".into(),
        kind: Kind::Video,
        title: "Flickermood".into(),
        identity: Some("Soundcloud:293".into()),
        ..playlist_entry()
    };
    let (end, events) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert_eq!(ended(&events), [(0, VideoState::Done)]);
    assert_eq!(std::fs::read_to_string(&s.log).unwrap().trim(), "https://soundcloud.com/forss/flickermood");
    assert_eq!(files_in(&s.resolved.dir), ["Flickermood.m4a"]);
}



// ---- fix round 1: untitled tracks, progress of a fallback ----

#[test]
fn a_set_s_untitled_tracks_are_filed_under_their_address_then_their_id() {
    let raw = std::fs::read_to_string(fixture_path("soundcloud-set.json")).unwrap();
    let p = crate::ytdlp::parse_probe(&raw).unwrap();
    let planned = plan(&p.entries, true);
    let stems: Vec<String> = planned
        .iter()
        .map(|p| match &p.action {
            Action::Download { stem, .. } => stem.clone(),
            Action::Skip(r) => format!("skip {r}"),
        })
        .collect();
    assert_eq!(
        stems,
        [
            "01 - world-on-fire-1",
            "02 - gimme-twice-mastered",
            "03 - goldrushed-mastered",
            "04 - the-concept-d-d-dance",
            "05 - in-the-end-mastered",
            "06 - 47127631",
        ]
    );
    assert_eq!(planned[0].title, "world-on-fire-1", "the row is named too");
    assert_eq!(planned[5].title, "", "nothing to name the last one by");
}

#[tokio::test]
async fn another_site_s_video_fallback_shows_its_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-probe.json");
    // The fake prints the percentage for the id after the last `=`: give it
    // the address yt-dlp would name `293` by.
    let s = setup(&missing, "id=293", None);
    let entry = Entry {
        url: "https://soundcloud.com/forss/flickermood".into(),
        kind: Kind::Video,
        title: "Flickermood".into(),
        identity: Some("Soundcloud:293".into()),
        ..playlist_entry()
    };
    let (end, events) = run(&s, &entry, &Cancel::new()).await;
    assert_eq!(end, PresetEnd::Finished);
    assert!(events.contains(&Event::Progress(0, 42.0)), "{events:?}");
}
