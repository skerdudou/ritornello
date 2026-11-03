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
    let args = download_args(Path::new("/mnt/ritornello/nas/Rock"), "01 - Song", "https://www.youtube.com/watch?v=jNQXAC9IVRw");
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
    let args = download_args(Path::new("/d"), "n", "https://www.youtube.com/watch?v=jNQXAC9IVRw").join(" ");
    for banned in ["thumbnail", "embed", "convert", "extract-audio", "merge", "-x"] {
        assert!(!args.contains(banned), "{banned}");
    }
}

#[test]
fn a_percent_in_a_name_cannot_write_a_template_field() {
    let args = download_args(Path::new("/d/100%"), "%(id)s - 50%", "https://www.youtube.com/watch?v=jNQXAC9IVRw");
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
/// writing the file the `-o` template names, after `hook`.
fn wrapper(probe_json: &Path, log: &Path, hook: &str) -> String {
    format!(
        r#"case "$1" in -J) cat '{probe}'; exit 0;; esac
out=; url=
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift;; --) url="$2"; break;; esac; shift; done
echo "$url" >> '{log}'
echo "$out" >> '{log}.out'
id="${{url##*=}}"
{hook}
echo "  42.0% $id"
file=$(printf '%s' "$out" | sed 's/%(ext)s/m4a/; s/%%/%/g')
printf 'audio' > "$file"
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
    let mut events = Vec::new();
    let end = download_preset(&s.ctx, entry, &s.resolved, &share_holds, cancel, &mut |e| events.push(e)).await;
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
    assert_eq!(files.len(), 12, "11 audio files and the cover: {files:?}");
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
    assert_eq!(calls.len(), 1);
    assert!(calls[0].starts_with("https://") && calls[0].contains(".jpg"), "{calls:?}");
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

#[tokio::test]
async fn a_single_video_goes_next_to_the_target_and_its_cover_only_if_none_exists() {
    let s = setup(&fixture_path("video.json"), "", Some(JPEG.to_vec()));
    let entry = Entry { url: "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(), kind: Kind::Video, title: "Me at the zoo".into(), ..playlist_entry() };
    run(&s, &entry, &Cancel::new()).await;
    assert_eq!(files_in(&s.resolved.dir), ["Me at the zoo.m4a", "cover.jpg"], "no subfolder for one video");
    // A second video next to it must not overwrite the cover.
    std::fs::write(s.resolved.dir.join("cover.jpg"), b"mine").unwrap();
    let before = s.calls.lock().unwrap().len();
    run(&s, &entry, &Cancel::new()).await;
    assert_eq!(std::fs::read(s.resolved.dir.join("cover.jpg")).unwrap(), b"mine");
    assert_eq!(s.calls.lock().unwrap().len(), before, "not even fetched");
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
    let end = download_preset(&s.ctx, &entry, &s.resolved, &share_holds, &cancel, &mut |e| {
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
    let args = download_args(Path::new("/d"), &sanitize("$HOME ${PATH}", "id"), "https://www.youtube.com/watch?v=jNQXAC9IVRw");
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
    let args = download_args(Path::new("/d"), "n", "https://soundcloud.com/forss/flickermood");
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
