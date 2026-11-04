use super::*;
use crate::source::WebStreamingSource;
use ritornello_files_mount::roots::{Root, RootKind, Roots};
use ritornello_plugin_sdk::SourcePlugin;
use ritornello_plugin_web_streaming::download::{Ctx, Limits};
use ritornello_plugin_web_streaming::reason;
use ritornello_plugin_web_streaming::targets::{Resolved, TargetError};
use ritornello_plugin_web_streaming::tool::fetch::{BoxFuture, Fetch, Limit};
use ritornello_plugin_web_streaming::tool::{self, verify, Deps};
use ritornello_proto::SourceAction;
use serde_json::{json, Value};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Instant;

const VIDEO: &str = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
const LIST_ID: &str = "PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI";
const LIST: &str = "https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A fetcher that never answers in time: an update that reached the network
/// would hold for ten seconds.
struct SlowFetch;

impl Fetch for SlowFetch {
    fn get(&self, _url: &str, _limit: Limit) -> BoxFuture<Result<Vec<u8>, String>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Err("the network is slow".to_string())
        })
    }

    fn redirect_of(&self, _url: &str, _deadline: Duration) -> BoxFuture<Result<String, String>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Err("the network is slow".to_string())
        })
    }
}

fn install_tool(data: &Path, body: &str) {
    let bin = data.join("bin/yt-dlp");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A yt-dlp that answers a probe with the captures, and "downloads" nothing.
fn answering() -> String {
    format!(
        "case \"$4\" in *playlist*) cat '{dir}/playlist.json';; *) cat '{dir}/video.json';; esac",
        dir = fixtures().display()
    )
}

struct World {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    admin: WebStreamingAdmin,
    saved: tokio::sync::watch::Receiver<()>,
    entries: Arc<AsyncRwLock<Entries>>,
    mounts: Arc<Mutex<String>>,
    /// How long reading the mount table takes (a stalled system).
    mounts_delay_ms: Arc<std::sync::atomic::AtomicU64>,
}

fn smb_root(name: &str, writable: bool) -> Root {
    Root {
        name: name.into(),
        kind: RootKind::Smb,
        path: None,
        host: "nas".into(),
        share: "music".into(),
        subpath: None,
        user: String::new(),
        domain: String::new(),
        writable,
    }
}

fn write_roots(tmp: &Path, roots: Vec<Root>) {
    std::fs::create_dir_all(tmp.join("files")).unwrap();
    std::fs::write(tmp.join("files/media-roots.toml"), toml::to_string(&Roots { root: roots }).unwrap()).unwrap();
}

/// A world around a fake yt-dlp whose body is `tool_body`; the downloads land
/// in a temporary folder whatever the target.
async fn world(tool_body: &str) -> World {
    world_with(tool_body, Duration::from_secs(30)).await
}

async fn world_with(tool_body: &str, targets_max_age: Duration) -> World {
    world_on(tool_body, targets_max_age, "arm").await
}

async fn world_on(tool_body: &str, targets_max_age: Duration, arch: &str) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("web-streaming");
    std::fs::create_dir_all(&data).unwrap();
    install_tool(&data, tool_body);
    let entries = Arc::new(AsyncRwLock::new(Entries::default()));
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let resolved = Resolved { base: out.clone(), dir: out };
    let ctx = Ctx {
        ytdlp: Arc::new(Ytdlp::new(&data)),
        lists: ListCache::new(&data),
        limits: Limits { fetch_cover: Arc::new(|_| Box::pin(async { None })), ..Limits::default() },
    };
    let jobs = Jobs::start(
        &data,
        entries.clone(),
        ctx,
        Arc::new(move |_| {
            let r = resolved.clone();
            Box::pin(async move { Ok(r) })
        }),
    );
    let deps = Deps {
        fetch: Arc::new(SlowFetch),
        now: Arc::new(|| 1_000_000),
        search: vec![],
        arch: arch.into(),
        gate: jobs.tool_gate(),
        key: verify::ytdlp_key().unwrap(),
        node_keys: verify::node_keys().unwrap(),
    };
    let tool = Tool::with_deps(&data, deps);
    let (tx, saved) = tokio::sync::watch::channel(());
    let mounts = Arc::new(Mutex::new(String::new()));
    let seen = mounts.clone();
    let mounts_delay_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let delay = mounts_delay_ms.clone();
    let env = Environment {
        roots_path: files_roots_path(&data),
        mounts: Arc::new(move || {
            std::thread::sleep(Duration::from_millis(delay.load(std::sync::atomic::Ordering::SeqCst)));
            seen.lock().unwrap().clone()
        }),
        arch: arch.into(),
        targets_max_age,
    };
    let admin = WebStreamingAdmin::new(data.clone(), entries.clone(), tool, jobs, tx, env);
    World { _tmp: tmp, data, admin, saved, entries, mounts, mounts_delay_ms }
}

/// A call that must come back at once, whatever the tool or the share does.
async fn quickly<T>(what: &str, f: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let out = f.await;
    assert!(started.elapsed() < Duration::from_millis(100), "{what} took {:?}", started.elapsed());
    out
}

impl World {
    async fn op(&mut self, v: Value) -> Result<(), Text> {
        self.admin.set_data(v).await
    }

    /// Polls `get_data` until `done` holds.
    async fn until(&self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..200 {
            let d = self.admin.get_data().await;
            if done(&d) {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("never: {what}: {}", self.admin.get_data().await);
    }
}

fn entry(preset: u8, url: &str, kind: Kind) -> Entry {
    Entry { preset, url: url.into(), kind, title: format!("t{preset}"), channel: None, count: None, site: None, identity: None }
}

fn key_of(r: Result<(), Text>) -> String {
    match r {
        Err(Text::Keyed { key, .. }) => {
            let en = ritornello_i18n::try_parse(ritornello_plugin_web_streaming::WEB_STREAMING_EN).unwrap();
            assert!(en.contains_key(&key), "key {key} is not in the English catalogue");
            key
        }
        other => panic!("expected a keyed refusal, got {other:?}"),
    }
}

// ---- no route waits ---------------------------------------------------------

#[tokio::test]
async fn no_admin_op_waits_for_yt_dlp() {
    // Every yt-dlp this plugin runs sleeps ten seconds; every update that
    // reaches the network does too.
    let mut w = world("exec sleep 10").await;
    tokio::spawn(tool::schedule(w.admin.tool.clone()));
    w.entries.write().await.entry = vec![entry(1, VIDEO, Kind::Video)];
    let target = json!({ "root": "nas", "path": "" });

    quickly("probe", w.op(json!({ "op": "probe", "text": VIDEO }))).await.unwrap();
    quickly("download", w.op(json!({ "op": "download", "presets": [1], "target": target }))).await.unwrap();
    quickly("update_tool", w.op(json!({ "op": "update_tool", "force": true }))).await.unwrap();
    quickly("set_channel", w.op(json!({ "op": "set_channel", "channel": "nightly" }))).await.unwrap();
    quickly("reprobe", w.op(json!({ "op": "reprobe" }))).await.unwrap();
    quickly("list_targets", w.op(json!({ "op": "list_targets" }))).await.unwrap();
    quickly("list_dirs", w.op(json!({ "op": "list_dirs", "target": target }))).await.unwrap();
    quickly("create_dir", w.op(json!({ "op": "create_dir", "target": target, "name": "x" }))).await.unwrap();
    quickly("cancel", w.op(json!({ "op": "cancel", "job": 1 }))).await.unwrap();
    let data = quickly("get_data while all that runs", w.admin.get_data()).await;
    assert_eq!(data["probe"]["running"], true, "the probe is still waiting for the tool");
    assert_eq!(data["tool"]["updating"], true);
}

// ---- probe ------------------------------------------------------------------

#[tokio::test]
async fn each_refused_line_has_its_own_error_and_the_valid_ones_are_probed() {
    let mut w = world(&answering()).await;
    let text = format!(
        "{VIDEO}\nnot a link\nhttps://studio.youtube.com/watch?v=jNQXAC9IVRw\nhttps://www.youtube.com/live/jNQXAC9IVRw\n{LIST}\nhttp://192.168.1.1/x\n"
    );
    w.op(json!({ "op": "probe", "text": text })).await.unwrap();
    let d = w.until("probe done", |d| d["probe"]["running"] == false).await;
    let results = d["probe"]["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    assert_eq!(results[0]["state"], "found");
    assert_eq!(results[0]["choices"][0]["kind"], "video");
    assert_eq!(results[0]["choices"][0]["title"], "Me at the zoo");
    assert_eq!((results[1]["state"].clone(), results[1]["code"].clone()), (json!("error"), json!("not_a_url")));
    assert_eq!(results[2]["code"], "foreign_host");
    assert_eq!(results[2]["detail"], "studio.youtube.com");
    assert_eq!(results[3]["code"], "live_stream");
    assert_eq!(results[4]["choices"][0]["kind"], "playlist");
    assert_eq!(results[4]["choices"][0]["count"], 12);
    assert_eq!(results[4]["choices"][0]["url"], LIST);
    assert_eq!(results[4]["choices"][0]["site"], "YoutubeTab");
    assert_eq!(results[4]["choices"][0]["identity"], Value::Null);
    assert_eq!((results[5]["code"].clone(), results[5]["detail"].clone()), (json!("local_network"), json!("192.168.1.1")));
}

/// A yt-dlp that answers with the SoundCloud and Bandcamp captures, by the
/// address it is given, and writes each address it is given to `bin/calls`.
fn answering_web() -> String {
    format!(
        "echo \"$4\" >> \"$(dirname \"$0\")/calls\"\ncase \"$4\" in *sets*) cat '{dir}/soundcloud-set.json';; *album*) cat '{dir}/bandcamp-album.json';; *live*) echo '{{\"_type\":\"video\",\"id\":\"1\",\"title\":\"t\",\"is_live\":true}}';; *) cat '{dir}/soundcloud-track.json';; esac",
        dir = fixtures().display()
    )
}

const SC_TRACK: &str = "https://soundcloud.com/forss/flickermood";
const SC_SET: &str = "https://soundcloud.com/the-concept-band/sets/the-royal-concept-ep";

#[tokio::test]
async fn another_site_s_link_is_whatever_yt_dlp_says_it_is() {
    let mut w = world(&answering_web()).await;
    let text = format!("{SC_TRACK}#t=1\n{SC_SET}\nhttps://example.org/live\n");
    w.op(json!({ "op": "probe", "text": text })).await.unwrap();
    let d = w.until("probe done", |d| d["probe"]["running"] == false).await;
    let results = d["probe"]["results"].as_array().unwrap();
    let track = &results[0]["choices"];
    assert_eq!(track.as_array().unwrap().len(), 1, "probed once, one choice");
    assert_eq!(track[0]["kind"], "video");
    assert_eq!(track[0]["url"], SC_TRACK, "normalised: no fragment");
    assert_eq!(track[0]["title"], "Flickermood");
    assert_eq!(track[0]["site"], "Soundcloud");
    assert_eq!(track[0]["identity"], "Soundcloud:293");
    let set = &results[1]["choices"];
    assert_eq!(set.as_array().unwrap().len(), 1);
    assert_eq!((set[0]["kind"].clone(), set[0]["count"].clone()), (json!("playlist"), json!(6)));
    assert_eq!(set[0]["site"], "SoundcloudSet");
    assert_eq!(set[0]["identity"], Value::Null);
    assert_eq!(results[2]["code"], "live_stream");
    let calls = std::fs::read_to_string(w.data.join("bin/calls")).unwrap();
    assert_eq!(calls, format!("{SC_TRACK}\n{SC_SET}\nhttps://example.org/live\n"), "each link probed once");

    // Saving the set caches it under its own key: the first select plays.
    let mut found = entry(1, SC_SET, Kind::Playlist);
    found.site = Some("SoundcloudSet".into());
    let mut video = entry(2, SC_TRACK, Kind::Video);
    video.identity = Some("Soundcloud:293".into());
    w.op(json!({ "op": "save", "entries": [found, video] })).await.unwrap();
    let key = ritornello_plugin_web_streaming::link::parse_link(SC_SET).unwrap().cache_key().unwrap();
    assert_eq!(ListCache::new(&w.data).load(&key).unwrap().len(), 6);
    let mut source = WebStreamingSource::new(w.data.clone(), w.entries.clone(), Some(w.saved.clone()));
    assert!(matches!(source.select(1).await.action, SourceAction::Play { .. }));
    let d = w.admin.get_data().await;
    assert_eq!(d["entries"][0]["site"], "SoundcloudSet");
    assert_eq!(d["entries"][1]["identity"], "Soundcloud:293");
}

#[tokio::test]
async fn a_web_link_fits_either_kind_and_a_local_one_does_not_save() {
    let mut w = world("exit 1").await;
    let both = vec![entry(1, SC_TRACK, Kind::Video), entry(2, SC_SET, Kind::Playlist)];
    w.op(json!({ "op": "save", "entries": both })).await.unwrap();
    let local = vec![entry(1, "http://192.168.1.1/x", Kind::Video)];
    assert_eq!(key_of(w.op(json!({ "op": "save", "entries": local })).await), "bad_entry");
}

#[tokio::test]
async fn a_link_naming_a_video_and_its_playlist_offers_both() {
    let mut w = world(&answering()).await;
    w.op(json!({ "op": "probe", "text": format!("{VIDEO}&list={LIST_ID}") })).await.unwrap();
    let d = w.until("probe done", |d| d["probe"]["running"] == false).await;
    let kinds: Vec<&Value> = d["probe"]["results"][0]["choices"].as_array().unwrap().iter().map(|c| &c["kind"]).collect();
    assert_eq!(kinds, [&json!("video"), &json!("playlist")]);
}

#[tokio::test]
async fn a_failing_tool_is_a_line_error_with_its_last_line() {
    let mut w = world("echo 'ERROR: Video unavailable' >&2; exit 1").await;
    w.op(json!({ "op": "probe", "text": VIDEO })).await.unwrap();
    let d = w.until("probe done", |d| d["probe"]["running"] == false).await;
    let line = &d["probe"]["results"][0];
    assert_eq!((line["state"].clone(), line["code"].clone()), (json!("error"), json!("failed")));
    assert!(line["detail"].as_str().unwrap().contains("Video unavailable"));
}

/// What the plugin itself wrote reaches the page as a code, never as English
/// behind "yt-dlp: ": a silent exit, the guard's refusal of a name that
/// resolved locally.
#[tokio::test]
async fn the_plugins_own_probe_failures_reach_the_page_as_codes() {
    let mut w = world("exit 1").await;
    w.op(json!({ "op": "probe", "text": VIDEO })).await.unwrap();
    let d = w.until("probe done", |d| d["probe"]["running"] == false).await;
    let line = &d["probe"]["results"][0];
    assert_eq!((line["code"].clone(), line["detail"].clone()), (json!("exited"), json!("exit status: 1")));

    let refused = "echo 'ERROR: [generic] Unable to download webpage: [Errno 13] local network refused: 10.0.0.7' >&2; exit 1";
    let mut w = world(refused).await;
    w.op(json!({ "op": "probe", "text": "https://router.example/x" })).await.unwrap();
    let d = w.until("probe done", |d| d["probe"]["running"] == false).await;
    let line = &d["probe"]["results"][0];
    assert_eq!((line["code"].clone(), line["detail"].clone()), (json!("local_network"), json!("10.0.0.7")));
}

#[tokio::test]
async fn a_second_probe_while_one_runs_is_refused_and_too_many_links_are() {
    let mut w = world("exec sleep 10").await;
    w.op(json!({ "op": "probe", "text": VIDEO })).await.unwrap();
    assert_eq!(key_of(w.op(json!({ "op": "probe", "text": VIDEO })).await), "probe_busy");
    let many = (0..=MAX_LINKS).map(|_| VIDEO).collect::<Vec<_>>().join("\n");
    assert_eq!(key_of(w.op(json!({ "op": "probe", "text": many })).await), "too_many_links");
    assert_eq!(key_of(w.op(json!({ "op": "probe", "text": "  \n " })).await), "no_links");
}

// ---- save -------------------------------------------------------------------

#[tokio::test]
async fn save_refuses_more_than_99() {
    let mut w = world("exit 1").await;
    let hundred: Vec<Entry> = (1..=100).map(|n| entry(n as u8, VIDEO, Kind::Video)).collect();
    let r = w.op(json!({ "op": "save", "entries": hundred })).await;
    assert_eq!(key_of(r), "too_many_entries");
    let ninety_nine: Vec<Entry> = (1..=99).map(|n| entry(n, VIDEO, Kind::Video)).collect();
    w.op(json!({ "op": "save", "entries": ninety_nine })).await.unwrap();
    assert_eq!(w.entries.read().await.entry.len(), 99);
    assert!(w.data.join("entries.toml").is_file());
}

#[tokio::test]
async fn preset_zero_is_not_said_to_be_the_hundredth() {
    let mut w = world("exit 1").await;
    let r = w.op(json!({ "op": "save", "entries": [entry(0, VIDEO, Kind::Video)] })).await;
    assert_eq!(key_of(r), "preset_out_of_range");
}

/// The playlist was probed, saved (so its videos are on disk), and is saved
/// again later in another order without being probed again: its cache stays.
#[tokio::test]
async fn a_cached_playlist_not_probed_this_session_keeps_its_cache_when_the_table_is_saved_again() {
    let mut w = world(&answering()).await;
    w.op(json!({ "op": "probe", "text": LIST })).await.unwrap();
    w.until("probe done", |d| d["probe"]["running"] == false).await;
    w.op(json!({ "op": "save", "entries": [entry(1, LIST, Kind::Playlist)] })).await.unwrap();
    let cached = ListCache::new(&w.data).load(LIST_ID).unwrap();
    assert_eq!(cached.len(), 12);

    // Reordered, nothing probed in between: the memory of the probe is gone.
    let reordered = vec![entry(1, VIDEO, Kind::Video), entry(2, LIST, Kind::Playlist)];
    w.op(json!({ "op": "save", "entries": reordered })).await.unwrap();
    assert_eq!(ListCache::new(&w.data).load(LIST_ID), Some(cached));
}

#[tokio::test]
async fn save_refuses_what_it_could_not_play_and_writes_nothing() {
    let mut w = world("exit 1").await;
    let twice = vec![entry(1, VIDEO, Kind::Video), entry(1, VIDEO, Kind::Video)];
    assert_eq!(key_of(w.op(json!({ "op": "save", "entries": twice })).await), "duplicate_preset");
    let wrong_kind = vec![entry(1, VIDEO, Kind::Playlist)];
    assert_eq!(key_of(w.op(json!({ "op": "save", "entries": wrong_kind })).await), "bad_entry");
    let foreign = vec![entry(1, "https://studio.youtube.com/watch?v=jNQXAC9IVRw", Kind::Video)];
    assert_eq!(key_of(w.op(json!({ "op": "save", "entries": foreign })).await), "bad_entry");
    assert!(!w.data.join("entries.toml").exists());
    assert!(w.entries.read().await.entry.is_empty());
    assert!(!w.saved.has_changed().unwrap(), "no notification for a refusal");
}

#[tokio::test]
async fn a_save_notifies_the_source_and_survives_a_restart() {
    let mut w = world("exit 1").await;
    w.op(json!({ "op": "save", "entries": [entry(2, VIDEO, Kind::Video)] })).await.unwrap();
    assert!(w.saved.has_changed().unwrap(), "the source half is told");
    assert_eq!(Entries::load(&w.data.join("entries.toml")).unwrap().entry.len(), 1);
    let d = w.admin.get_data().await;
    assert_eq!(d["entries"][0]["preset"], 2);
}

#[tokio::test]
async fn a_saved_playlist_is_cached_so_the_first_select_plays() {
    let mut w = world(&answering()).await;
    w.op(json!({ "op": "probe", "text": LIST })).await.unwrap();
    w.until("probe done", |d| d["probe"]["running"] == false).await;
    assert!(ListCache::new(&w.data).load(LIST_ID).is_none(), "nothing is cached by a probe alone");

    let mut found = entry(1, LIST, Kind::Playlist);
    found.count = Some(12);
    w.op(json!({ "op": "save", "entries": [found] })).await.unwrap();
    assert_eq!(ListCache::new(&w.data).load(LIST_ID).unwrap().len(), 12);

    // The very first select, before any background refresh could have run.
    let mut source = WebStreamingSource::new(w.data.clone(), w.entries.clone(), Some(w.saved.clone()));
    let outcome = source.select(1).await;
    assert!(matches!(outcome.action, SourceAction::Play { .. }), "{:?}", outcome.status_text);
}

#[tokio::test]
async fn a_playlist_that_was_never_probed_is_saved_without_a_cache() {
    let mut w = world(&answering()).await;
    w.op(json!({ "op": "save", "entries": [entry(1, LIST, Kind::Playlist)] })).await.unwrap();
    assert!(ListCache::new(&w.data).load(LIST_ID).is_none());
}

// ---- downloads and targets --------------------------------------------------

const MOUNTED: &str = "//nas/music /mnt/ritornello/nas cifs rw,relatime 0 0\n//nas/music /mnt/ritornello/ro cifs ro 0 0\n";

#[tokio::test]
async fn only_a_writable_mounted_share_is_a_target() {
    let mut w = world("exit 1").await;
    write_roots(w._tmp.path(), vec![smb_root("nas", true), smb_root("ro", true), smb_root("locked", false)]);
    *w.mounts.lock().unwrap() = MOUNTED.to_string();
    w.op(json!({ "op": "list_targets" })).await.unwrap();
    let d = w.until("targets", |d| d["targets"] == json!(["nas"])).await;
    assert_eq!(d["last_target"], Value::Null);
}

/// At boot the share is mounted after this plugin starts: the page must see
/// it without a restart, and get_data must never be the one reading it.
#[tokio::test]
async fn a_stale_target_cache_is_refreshed_in_a_task_and_never_on_the_get_data_path() {
    let w = world_with("exit 1", Duration::ZERO).await;
    write_roots(w._tmp.path(), vec![smb_root("nas", true)]);
    *w.mounts.lock().unwrap() = MOUNTED.to_string();
    // Reading the mount table now takes half a second: a get_data that read
    // it itself would show.
    w.mounts_delay_ms.store(500, std::sync::atomic::Ordering::SeqCst);
    let first = quickly("get_data with nothing cached", w.admin.get_data()).await;
    assert_eq!(first["targets"], json!([]), "the cache is what it answers with");
    let second = quickly("get_data while the refresh runs", w.admin.get_data()).await;
    assert_eq!(second["targets"], json!([]));
    // And the refresh that get_data started lands by itself.
    w.until("the share appears", |d| d["targets"] == json!(["nas"])).await;
    // Later the share goes away: noticed by the next reads, no restart.
    *w.mounts.lock().unwrap() = String::new();
    w.until("the share is gone", |d| d["targets"] == json!([])).await;
}

/// A roots file that cannot be read for a moment says nothing about the
/// shares: the list the page had stays.
#[tokio::test]
async fn a_failed_read_of_the_shares_keeps_the_known_list() {
    let mut w = world_with("exit 1", Duration::ZERO).await;
    write_roots(w._tmp.path(), vec![smb_root("nas", true)]);
    *w.mounts.lock().unwrap() = MOUNTED.to_string();
    w.op(json!({ "op": "list_targets" })).await.unwrap();
    w.until("targets", |d| d["targets"] == json!(["nas"])).await;

    std::fs::write(w._tmp.path().join("files/media-roots.toml"), "this is [not toml").unwrap();
    // The cache is stale at once (max age zero): the next reads try again,
    // fail, and must not forget the share.
    for _ in 0..4 {
        w.admin.get_data().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(w.admin.get_data().await["targets"], json!(["nas"]));

    // A file that is gone is a different thing: no roots declared, no share.
    std::fs::remove_file(w._tmp.path().join("files/media-roots.toml")).unwrap();
    w.op(json!({ "op": "list_targets" })).await.unwrap();
    w.until("no roots, no share", |d| d["targets"] == json!([])).await;
}

#[tokio::test]
async fn a_fresh_target_cache_is_not_read_again() {
    let mut w = world("exit 1").await;
    write_roots(w._tmp.path(), vec![smb_root("nas", true)]);
    *w.mounts.lock().unwrap() = MOUNTED.to_string();
    w.op(json!({ "op": "list_targets" })).await.unwrap();
    w.until("targets", |d| d["targets"] == json!(["nas"])).await;
    *w.mounts.lock().unwrap() = String::new();
    for _ in 0..5 {
        assert_eq!(w.admin.get_data().await["targets"], json!(["nas"]), "30 s have not passed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_last_target_is_remembered_and_dropped_when_gone() {
    let mut w = world("exit 1").await;
    write_roots(w._tmp.path(), vec![smb_root("nas", true)]);
    *w.mounts.lock().unwrap() = MOUNTED.to_string();
    w.op(json!({ "op": "list_targets" })).await.unwrap();
    w.until("targets", |d| d["targets"] == json!(["nas"])).await;
    w.entries.write().await.entry = vec![entry(1, VIDEO, Kind::Video)];

    w.op(json!({ "op": "download", "presets": [1], "target": { "root": "nas", "path": "rock/live" } }))
        .await
        .unwrap();
    let d = w.until("remembered", |d| d["last_target"] != Value::Null).await;
    assert_eq!(d["last_target"], json!({ "root": "nas", "path": "rock/live" }));
    let settings = std::fs::read_to_string(w.data.join("settings.toml")).unwrap();
    assert!(settings.contains("last_target"), "one settings file: {settings}");

    // The share goes away (unmounted): the page must not preselect it.
    *w.mounts.lock().unwrap() = String::new();
    w.op(json!({ "op": "list_targets" })).await.unwrap();
    let d = w.until("dropped", |d| d["targets"] == json!([])).await;
    assert_eq!(d["last_target"], Value::Null);

    // And it comes back with the share.
    *w.mounts.lock().unwrap() = MOUNTED.to_string();
    w.op(json!({ "op": "list_targets" })).await.unwrap();
    let d = w.until("back", |d| d["targets"] == json!(["nas"])).await;
    assert_eq!(d["last_target"], json!({ "root": "nas", "path": "rock/live" }));
}

#[tokio::test]
async fn a_download_is_queued_and_shows_in_the_jobs() {
    let mut w = world(&answering()).await;
    w.entries.write().await.entry = vec![entry(1, VIDEO, Kind::Video)];
    w.op(json!({ "op": "download", "presets": [1], "target": { "root": "nas", "path": "" } })).await.unwrap();
    let d = w.until("a job", |d| !d["jobs"].as_array().unwrap().is_empty()).await;
    assert_eq!(d["jobs"][0]["presets"], json!([1]));
    assert_eq!(key_of(w.op(json!({ "op": "download", "presets": [], "target": { "root": "nas", "path": "" } })).await), "nothing_selected");
    assert_eq!(
        key_of(w.op(json!({ "op": "download", "presets": [1, 1], "target": { "root": "nas", "path": "" } })).await),
        "duplicate_preset"
    );
}

/// Jobs are shown with codes for what the plugin wrote and raw text, as
/// `detail`, for what yt-dlp or the system said.
#[test]
fn a_reason_the_plugin_wrote_is_a_code_and_an_outside_voice_stays_raw() {
    use ritornello_plugin_web_streaming::download::VideoState;
    use ritornello_plugin_web_streaming::jobs::{Job, JobState, Row};
    let target = Target { root: "nas".into(), path: String::new() };
    let row = |state| Row { preset: 1, id: "x".into(), title: "t".into(), state, percent: None };
    let job = Job {
        id: 1,
        presets: vec![1],
        target,
        state: JobState::Failed(TargetError::NotMounted.to_string()),
        rows: vec![
            row(VideoState::Failed("ERROR: Video unavailable".into())),
            row(VideoState::Failed(reason::INTERRUPTED.into())),
            row(VideoState::Skipped(reason::UNAVAILABLE.into())),
            row(VideoState::Done),
        ],
    };
    let v = jobs_for_page(&[job]);
    assert_eq!(v[0]["detail"], json!({ "code": "not_mounted" }));
    assert_eq!(v[0]["rows"][0]["detail"], json!({ "code": "failed", "detail": "ERROR: Video unavailable" }));
    assert_eq!(v[0]["rows"][1]["detail"], json!({ "code": "interrupted" }));
    assert_eq!(v[0]["rows"][2]["detail"], json!({ "code": "unavailable" }));
    assert!(v[0]["rows"][3].get("detail").is_none());
}

/// A real failure, end to end: the update runs (no python3 on this machine's
/// empty search path) and the page's data carries its code, not its English.
#[tokio::test]
async fn the_tool_error_reaches_the_page_as_a_code() {
    let mut w = world("exit 1").await;
    tokio::spawn(tool::schedule(w.admin.tool.clone()));
    w.op(json!({ "op": "update_tool", "force": false })).await.unwrap();
    let d = w.until("the update failed", |d| d["tool"]["error_code"]["code"] == "python3_missing").await;
    assert!(d["tool"].get("error").is_none(), "the English text stays out of the page's data");
    assert_eq!(d["tool"]["forceable"], false);
    assert!(d["tool"].get("runtime_error").is_none());
}

#[tokio::test]
async fn folders_of_an_unusable_share_come_back_as_an_error_not_a_hang() {
    let mut w = world("exit 1").await;
    write_roots(w._tmp.path(), vec![smb_root("nas", true)]);
    // Declared, but the kernel does not list it.
    w.op(json!({ "op": "list_dirs", "target": { "root": "nas", "path": "" } })).await.unwrap();
    let d = w.until("an answer", |d| d["explore"]["busy"] == false).await;
    assert_eq!(d["explore"]["error"]["code"], "not_mounted");
    w.op(json!({ "op": "list_dirs", "target": { "root": "ghost", "path": "" } })).await.unwrap();
    let d = w.until("an answer", |d| d["explore"]["busy"] == false).await;
    assert_eq!(d["explore"]["error"]["code"], "unknown_root");
    // `$` is never a folder name, whatever the page asks.
    *w.mounts.lock().unwrap() = MOUNTED.to_string();
    w.op(json!({ "op": "create_dir", "target": { "root": "nas", "path": "" }, "name": "a$b" })).await.unwrap();
    let d = w.until("an answer", |d| d["explore"]["busy"] == false).await;
    assert_eq!(d["explore"]["error"]["code"], "escapes");
}

// ---- the tool ---------------------------------------------------------------

#[tokio::test]
async fn the_channel_is_switched_in_the_settings_and_an_update_is_asked() {
    let mut w = world("exit 1").await;
    w.op(json!({ "op": "set_channel", "channel": "nightly" })).await.unwrap();
    let d = w.until("channel", |d| d["tool"]["channel"] == "nightly").await;
    assert_eq!(d["tool"]["updating"], true, "the switch asks for the new channel's release");
    assert!(std::fs::read_to_string(w.data.join("settings.toml")).unwrap().contains("nightly"));
}

#[tokio::test]
async fn the_capabilities_panel_says_what_is_missing() {
    // No python3 and no JavaScript runtime on this (empty) search path. On
    // armv7 the plugin installs its own node, on x86_64 its own deno: one is
    // on its way. Elsewhere nothing could help.
    for (arch, hopeless) in [("arm", false), ("x86_64", false), ("riscv64", true)] {
        let mut w = world_on("exit 1", Duration::from_secs(30), arch).await;
        w.op(json!({ "op": "reprobe" })).await.unwrap();
        let d = w.until("probed", |d| d["capabilities"]["checking"] == false && d["capabilities"]["apt_hint"] != Value::Null).await;
        let caps = &d["capabilities"];
        assert_eq!(caps["python3"], Value::Null);
        assert_eq!(caps["apt_hint"], "sudo apt install python3");
        assert_eq!(caps["js_runtime"], Value::Null);
        assert_eq!(caps["js_hopeless"], hopeless, "{arch}");
        assert_eq!(caps["js_runtime_unmaintained"], false, "{arch}");
    }
}

#[tokio::test]
async fn an_unknown_op_is_a_keyed_refusal() {
    let mut w = world("exit 1").await;
    assert_eq!(key_of(w.op(json!({ "op": "explode" })).await), "bad_request");
}

#[tokio::test]
async fn the_admin_serves_its_two_assets() {
    let w = world("exit 1").await;
    assert!(w.admin.asset("ui.js").is_some() && w.admin.asset("ui.css").is_some());
    assert!(w.admin.asset("other").is_none());
}

#[test]
fn the_english_catalogue_has_every_message_the_page_derives_from_a_code() {
    let en = ritornello_i18n::try_parse(ritornello_plugin_web_streaming::WEB_STREAMING_EN).unwrap();
    for code in [
        "not_a_url", "foreign_host", "no_media", "live_stream", "tool_missing", "timeout", "failed", "unknown_root",
        "not_writable", "not_mounted", "escapes", "unresponsive", "io", "wrong_kind", "unavailable", "invalid_id",
        "timed_out", "interrupted", "job_lost", "no_such_preset", "preset_no_playlist", "preset_no_video",
        "bad_entry", "failed_silent", "exited", "local_network",
    ] {
        assert!(en.contains_key(&format!("fault_{code}")), "fault_{code}");
    }
    for code in [
        "busy", "python3_missing", "python3_unusable", "fetch", "refused_bad_signature", "refused_not_listed",
        "refused_mismatch", "rollback_older", "rollback_other_build", "rollback_unreadable", "install",
    ] {
        assert!(en.contains_key(&format!("update_{code}")), "update_{code}");
    }
    assert!(en["fault_live_stream"].contains("live streams are not supported"));
}

/// The page is served the last failed playback the wrapper recorded, as it
/// was written, and nothing before there is one.
#[tokio::test]
async fn the_last_failed_playback_is_served_to_the_page() {
    let w = world("exit 1").await;
    assert_eq!(quickly("get_data", w.admin.get_data()).await["last_failure"], Value::Null);
    let record = json!({"time": 1730000000.5, "host": "www.youtube.com", "code": "ytdlp_failed", "detail": "ERROR: gone"});
    std::fs::write(w.data.join(ritornello_plugin_web_streaming::playfail::FILE), record.to_string()).unwrap();
    assert_eq!(quickly("get_data", w.admin.get_data()).await["last_failure"], record);
}
