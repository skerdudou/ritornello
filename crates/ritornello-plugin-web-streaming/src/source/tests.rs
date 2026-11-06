use super::*;
use ritornello_plugin_web_streaming::entries::{Entry, ListCache};
use ritornello_plugin_web_streaming::ytdlp::Kind;
use ritornello_proto::IdentityUpdate;
use serde_json::json;

const ID: &str = "dQw4w9WgXcQ";
const WATCH: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

fn watch(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

fn video_entry(preset: u8) -> Entry {
    Entry { preset, url: WATCH.into(), kind: Kind::Video, title: "Never gonna".into(), channel: None, count: None, site: None, identity: None }
}

fn list_entry(preset: u8) -> Entry {
    Entry {
        preset,
        url: "https://www.youtube.com/playlist?list=PLx".into(),
        kind: Kind::Playlist,
        title: "A list".into(),
        channel: None,
        count: None,
        site: None,
        identity: None,
    }
}

fn vid(id: &str, title: &str, available: bool) -> Video {
    Video { extractor: "Youtube".into(), id: id.into(), title: title.into(), duration: None, available, thumbnail_jpg: None, url: None }
}

struct Fixture {
    dir: tempfile::TempDir,
    source: WebStreamingSource,
}

impl Fixture {
    fn new(entries: Vec<Entry>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        install_tool(dir.path(), "exit 1");
        let source = WebStreamingSource::new(
            dir.path().to_path_buf(),
            Arc::new(AsyncRwLock::new(Entries { entry: entries })),
            None,
        );
        Self { dir, source }
    }

    fn cache(&self, id: &str, videos: &[Video]) {
        ListCache::new(self.dir.path()).store(id, videos).unwrap();
    }

    fn bin(&self) -> String {
        self.dir.path().join("bin/yt-dlp").to_str().unwrap().to_string()
    }
}

/// A stand-in for yt-dlp: the wrapper only has to exist, and to answer.
fn install_tool(data: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let bin = data.join("bin/yt-dlp");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn catalogue_knows(o: &SourceOutcome) -> String {
    let en = ritornello_i18n::try_parse(ritornello_plugin_web_streaming::WEB_STREAMING_EN).unwrap();
    match &o.status_text {
        Some(Text::Keyed { key, .. }) => {
            assert!(en.contains_key(key), "key {key} is not in the English catalogue");
            key.clone()
        }
        other => panic!("expected a keyed status, got {other:?}"),
    }
}

fn is_play(a: &SourceAction) -> bool {
    matches!(a, SourceAction::Play { .. })
}

fn playing(id: &str) -> Option<IdentityUpdate> {
    Some(IdentityUpdate::Playing(json!({"kind": "video", "id": id})))
}

#[test]
fn it_announces_its_icon() {
    let f = Fixture::new(vec![]);
    assert_eq!(f.source.icon(), Some("headphones"));
}

#[tokio::test]
async fn select_on_a_video_plays_its_watch_url_with_the_tool_path() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    let o = f.source.select(1).await;
    assert_eq!(o.action, SourceAction::play(format!("ytdl://{WATCH}")).finite().ytdl_path(f.bin()));
    assert_eq!(o.identity, playing(ID));
    assert_eq!(o.location.as_deref(), Some(WATCH));
    assert_eq!(o.preset, Some(1));
    assert_eq!(o.preset_name.as_deref(), Some("Never gonna"));
    assert_eq!(o.preset_count, Some(2));
    assert_eq!(o.status_text, Some(video_title("Never gonna")));
    assert_eq!(catalogue_knows(&o), "video_title");
    assert!(!o.transient);
}

#[tokio::test]
async fn select_on_a_playlist_writes_only_available_videos_to_the_m3u() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true), vid("bbbbbbbbbbb", "B", false), vid("ccccccccccc", "C", true)]);
    let o = f.source.select(2).await;
    let m3u = f.dir.path().join("playlist.m3u");
    assert_eq!(
        std::fs::read_to_string(&m3u).unwrap(),
        format!("ytdl://{}\nytdl://{}\n", watch("aaaaaaaaaaa"), watch("ccccccccccc"))
    );
    assert_eq!(
        o.action,
        SourceAction::play(m3u.to_str().unwrap()).playlist().starting_at(0).finite().ytdl_path(f.bin())
    );
    assert_eq!(o.identity, playing("aaaaaaaaaaa"));
    assert_eq!(o.location, Some(watch("aaaaaaaaaaa")));
    assert_eq!(o.preset, Some(2));
    assert_eq!(o.preset_name.as_deref(), Some("A list"));
    assert_eq!(o.status_text, Some(video_title("A")));
}

#[tokio::test]
async fn player_track_names_the_video_being_played() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    // The unavailable entry comes first: the m3u is [a, b], so mpv's index 1
    // is b, whereas the cache's own position 1 would be a.
    f.cache("PLx", &[vid("gone0000000", "Gone", false), vid("aaaaaaaaaaa", "A", true), vid("bbbbbbbbbbb", "B", true)]);
    f.source.select(2).await;

    let o = f.source.player_track(1).await;
    assert_eq!(o.action, SourceAction::Noop);
    assert_eq!(o.identity, playing("bbbbbbbbbbb"));
    assert_eq!(o.location, Some(watch("bbbbbbbbbbb")));
    assert_eq!(o.status_text, Some(video_title("B")));

    let o = f.source.player_track(0).await;
    assert_eq!(o.identity, playing("aaaaaaaaaaa"));

    // Out of range, or mpv's -1 (the end, and every reload): nothing declared.
    for n in [-1, 2, 99] {
        let o = f.source.player_track(n).await;
        assert_eq!(o.action, SourceAction::Noop, "{n}");
        assert!(o.identity.is_none() && o.status_text.is_none(), "{n}");
    }
}

#[tokio::test]
async fn player_track_says_nothing_when_no_playlist_plays() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    assert!(f.source.player_track(0).await.identity.is_none(), "stopped");
    f.source.select(1).await;
    assert!(f.source.player_track(0).await.identity.is_none(), "a single video");
}

#[tokio::test]
async fn select_on_an_empty_preset_changes_nothing() {
    let mut f = Fixture::new(vec![video_entry(1)]);
    f.source.select(1).await;
    let o = f.source.select(7).await;
    assert_eq!(o.action, SourceAction::Noop);
    assert!(o.transient);
    assert_eq!(catalogue_knows(&o), "empty_preset");
    assert_eq!(o.preset_count, Some(1));
    // What plays carries on: no identity, no preset, nothing erased.
    assert!(o.identity.is_none() && o.preset.is_none() && o.armed.is_none());
}

#[tokio::test]
async fn select_without_tool_says_so_and_does_not_play() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    std::fs::remove_file(f.dir.path().join("bin/yt-dlp")).unwrap();
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true)]);
    for n in [1, 2] {
        let o = f.source.select(n).await;
        assert!(!is_play(&o.action), "{n}");
        assert_eq!(catalogue_knows(&o), "tool_missing");
        assert!(o.identity.is_none());
    }
    assert!(!f.dir.path().join("playlist.m3u").exists());
}

#[tokio::test]
async fn a_tool_path_that_is_not_utf8_is_not_played() {
    use std::os::unix::ffi::OsStrExt;
    let base = tempfile::tempdir().unwrap();
    let data = base.path().join(std::ffi::OsStr::from_bytes(b"d\xffata"));
    install_tool(&data, "exit 1");
    let mut source = WebStreamingSource::new(data, Arc::new(AsyncRwLock::new(Entries { entry: vec![video_entry(1)] })), None);
    let o = source.select(1).await;
    assert!(!is_play(&o.action));
    assert_eq!(catalogue_knows(&o), "tool_path_invalid");
}

#[tokio::test]
async fn a_playlist_not_cached_yet_or_without_playable_video_plays_nothing() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    let o = f.source.select(2).await;
    assert!(!is_play(&o.action));
    assert_eq!(catalogue_knows(&o), "list_not_cached");
    f.cache("PLx", &[vid("gone0000000", "Gone", false)]);
    let o = f.source.select(2).await;
    assert!(!is_play(&o.action));
    assert_eq!(catalogue_knows(&o), "list_empty");
    assert!(!f.dir.path().join("playlist.m3u").exists());
}

#[tokio::test]
async fn an_unreadable_link_in_a_preset_is_refused_not_played() {
    let mut e = video_entry(1);
    e.url = "https://studio.youtube.com/watch?v=dQw4w9WgXcQ".into();
    let mut f = Fixture::new(vec![e]);
    let o = f.source.select(1).await;
    assert!(!is_play(&o.action));
    assert_eq!(catalogue_knows(&o), "bad_entry");
}

#[tokio::test]
async fn next_and_prev_walk_mpvs_own_list_while_a_playlist_plays() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true), vid("bbbbbbbbbbb", "B", true)]);
    f.source.select(2).await;
    assert_eq!(f.source.next().await.action, SourceAction::PlayerNext);
    assert_eq!(f.source.prev().await.action, SourceAction::PlayerPrev);
}

#[tokio::test]
async fn next_and_prev_move_to_the_adjacent_preset_on_a_single_video() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true)]);
    f.source.select(1).await;
    let o = f.source.next().await;
    assert!(is_play(&o.action));
    assert_eq!(o.preset, Some(2));
    let o = f.source.prev().await; // playlist playing: mpv's own list
    assert_eq!(o.action, SourceAction::PlayerPrev);
}

#[tokio::test]
async fn next_with_one_preset_is_a_noop_that_declares_nothing() {
    let mut f = Fixture::new(vec![video_entry(1)]);
    f.source.select(1).await;
    for o in [f.source.next().await, f.source.prev().await] {
        assert_eq!(o.action, SourceAction::Noop);
        assert!(o.identity.is_none() && o.preset.is_none() && o.status_text.is_none());
    }
}

#[tokio::test]
async fn stopped_next_moves_the_armed_preset_without_playing_and_play_starts_it() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true)]);
    let o = f.source.next().await;
    assert_eq!(o.action, SourceAction::Noop);
    assert_eq!(o.preset, Some(2));
    assert_eq!(o.preset_name.as_deref(), Some("A list"));
    assert!(o.armed.is_some());
    assert_eq!(o.identity, Some(IdentityUpdate::Nothing));
    let o = f.source.play().await;
    assert!(is_play(&o.action));
    assert_eq!(o.preset, Some(2));
}

#[tokio::test]
async fn stop_keeps_the_preset_armed_and_deactivate_forgets_it() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.source.select(1).await;
    let o = f.source.stop().await;
    assert_eq!(o.action, SourceAction::Noop);
    assert!(o.armed.is_some());
    assert_eq!(o.preset, Some(1));
    // Stopped: next is the armed move again, not mpv's.
    assert_eq!(f.source.next().await.action, SourceAction::Noop);

    f.source.select(1).await;
    let o = f.source.deactivate().await;
    assert_eq!(o.action, SourceAction::Stop);
    assert_eq!(o.identity, Some(IdentityUpdate::Nothing));
    assert!(o.armed.is_none());
}

#[tokio::test]
async fn activate_plays_the_armed_preset_or_the_first_one_that_exists() {
    let mut f = Fixture::new(vec![video_entry(3)]);
    let o = f.source.activate().await;
    assert!(is_play(&o.action));
    assert_eq!(o.preset, Some(3));
    let mut empty = Fixture::new(vec![]);
    let o = empty.source.activate().await;
    assert_eq!(o.action, SourceAction::Noop);
    assert_eq!(catalogue_knows(&o), "empty_preset");
}

#[tokio::test]
async fn the_presets_are_listed_by_number() {
    let mut f = Fixture::new(vec![list_entry(5), video_entry(1)]);
    let names: Vec<(u8, String)> = f.source.list_presets().await.into_iter().map(|p| (p.index, p.name)).collect();
    assert_eq!(names, vec![(1, "Never gonna".to_string()), (5, "A list".to_string())]);
}

#[tokio::test]
async fn a_saved_table_is_announced_with_its_count_and_names() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = tokio::sync::watch::channel(());
    let entries = Arc::new(AsyncRwLock::new(Entries::default()));
    let mut source = WebStreamingSource::new(dir.path().to_path_buf(), entries.clone(), Some(rx));
    entries.write().await.entry = vec![video_entry(4)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!(n.preset_count, Some(4));
    assert_eq!(n.presets.unwrap()[0].name, "Never gonna");
    assert!(n.identity.is_none() && n.status_text.is_none() && n.play_request.is_none());
}

#[tokio::test]
async fn a_refresh_replaces_the_cache_and_a_failed_one_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ListCache::new(dir.path());
    let old = vec![vid("oldoldoldol", "Old", true)];
    cache.store("PLx", &old).unwrap();

    install_tool(dir.path(), "echo 'yt-dlp: ERROR: boom' >&2; exit 1");
    assert!(refresh_list(dir.path(), "PLx", "https://www.youtube.com/playlist?list=PLx", Duration::from_secs(5)).await.is_err());
    assert_eq!(cache.load("PLx"), Some(old.clone()), "a failure keeps the old list");

    // A video answer is not a playlist: refused too.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    install_tool(dir.path(), &format!("cat '{}'", fixtures.join("video.json").display()));
    assert!(refresh_list(dir.path(), "PLx", "https://www.youtube.com/playlist?list=PLx", Duration::from_secs(5)).await.is_err());
    assert_eq!(cache.load("PLx"), Some(old));

    install_tool(dir.path(), &format!("cat '{}'", fixtures.join("playlist.json").display()));
    refresh_list(dir.path(), "PLx", "https://www.youtube.com/playlist?list=PLx", Duration::from_secs(5)).await.unwrap();
    let fresh = cache.load("PLx").unwrap();
    assert_eq!(fresh.len(), 12);
    assert_ne!(fresh[0].id, "oldoldoldol");
}

// ---- fix round 1 ----

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A tool that answers with the playlist capture, takes a moment, and counts
/// each run in `bin/calls`.
fn install_counting_tool(data: &Path) {
    let body = format!(
        "echo \"$4\" >> \"$(dirname \"$0\")/calls\"\nsleep 0.4\ncat '{}'",
        fixtures().join("playlist.json").display()
    );
    install_tool(data, &body);
}

/// The refreshes of a playlist the tool ran. Counted by the address probed
/// (`-J --flat-playlist -- <url>`): a single video's page is also probed
/// once, for its cover, and is not a refresh.
fn calls(data: &Path) -> usize {
    probes_of(data, "playlist?list=")
}

fn probes_of(data: &Path, part: &str) -> usize {
    std::fs::read_to_string(data.join("bin/calls")).map(|s| s.lines().filter(|l| l.contains(part)).count()).unwrap_or(0)
}

/// Waits for every background refresh to finish.
async fn settle(source: &WebStreamingSource) {
    for _ in 0..200 {
        if source.refreshing.lock().unwrap().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("a refresh never finished");
}

fn armed_identity(o: &SourceOutcome) -> Option<serde_json::Value> {
    o.armed.as_ref().map(|a| a.identity.clone())
}

#[tokio::test]
async fn two_refreshes_of_one_playlist_run_the_tool_once() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true)]);
    install_counting_tool(f.dir.path());
    f.source.select(2).await;
    f.source.select(2).await;
    settle(&f.source).await;
    assert_eq!(calls(f.dir.path()), 1, "the second refresh found the first one running");
    // Once it is over, a new one may start.
    f.source.select(2).await;
    settle(&f.source).await;
    assert_eq!(calls(f.dir.path()), 2);
}

#[tokio::test]
async fn a_playlist_that_is_not_cached_or_empty_is_fetched_in_the_background() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    install_counting_tool(f.dir.path());
    f.source.select(2).await; // not cached
    settle(&f.source).await;
    assert_eq!(calls(f.dir.path()), 1);
    assert_eq!(ListCache::new(f.dir.path()).load("PLx").unwrap().len(), 12, "and the cache is filled");
}

#[tokio::test]
async fn a_playlist_with_nothing_playable_is_fetched_in_the_background() {
    let mut g = Fixture::new(vec![list_entry(2)]);
    g.cache("PLx", &[vid("gone0000000", "Gone", false)]);
    install_counting_tool(g.dir.path());
    g.source.select(2).await;
    settle(&g.source).await;
    assert_eq!(calls(g.dir.path()), 1);
}

#[tokio::test]
async fn a_select_that_plays_a_playlist_refreshes_it_after_the_fact() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true)]);
    install_counting_tool(f.dir.path());
    let o = f.source.select(2).await;
    assert!(is_play(&o.action), "the select did not wait for the tool");
    settle(&f.source).await;
    assert_eq!(calls(f.dir.path()), 1);
}

#[tokio::test]
async fn stepping_over_a_playlist_that_is_not_cached_fetches_it() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2), video_entry(3)]);
    install_counting_tool(f.dir.path());
    f.source.select(1).await;
    f.source.next().await;
    settle(&f.source).await;
    assert_eq!(calls(f.dir.path()), 1);
    // The video played first was probed once, for its cover, and only it.
    for _ in 0..100 {
        if f.source.covering.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(probes_of(f.dir.path(), "watch?v="), 1);
}

#[tokio::test]
async fn ids_from_outside_are_validated_before_the_m3u_the_identity_and_player_track() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    f.cache(
        "PLx",
        &[
            vid("ab\ncdefghi", "Newline", true),
            vid("aaaaaaaaaaa", "A", true),
            vid("a&b=cdefghi", "Ampersand", true),
            vid("short", "Short", true),
            vid("bbbbbbbbbbb", "B", true),
        ],
    );
    let o = f.source.select(2).await;
    let m3u = std::fs::read_to_string(f.dir.path().join("playlist.m3u")).unwrap();
    assert_eq!(m3u, format!("ytdl://{}\nytdl://{}\n", watch("aaaaaaaaaaa"), watch("bbbbbbbbbbb")));
    assert_eq!(o.identity, playing("aaaaaaaaaaa"));
    // The index space is the m3u's: 1 is B, and 2 is past the end.
    assert_eq!(f.source.player_track(1).await.identity, playing("bbbbbbbbbbb"));
    assert!(f.source.player_track(2).await.identity.is_none());
    assert_eq!(ritornello_plugin_web_streaming::link::watch_url_of("ab\ncdefghi"), None);
    assert_eq!(ritornello_plugin_web_streaming::link::watch_url_of("a&b=cdefghi"), None);
    // A list of nothing but bad ids is empty, not a file of garbage.
    f.cache("PLx", &[vid("ab\ncdefghi", "Newline", true)]);
    let o = f.source.select(2).await;
    assert!(!is_play(&o.action));
    assert_eq!(catalogue_knows(&o), "list_empty");
}

#[tokio::test]
async fn next_and_prev_step_over_presets_that_cannot_play() {
    let mut bad_link = video_entry(4);
    bad_link.url = "https://studio.youtube.com/watch?v=dQw4w9WgXcQ".into();
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2), video_entry(3), bad_link]);
    // 2 is not cached, 4 has an unreadable link.
    f.source.select(1).await;
    let o = f.source.next().await;
    assert!(is_play(&o.action));
    assert_eq!(o.preset, Some(3));
    let o = f.source.next().await; // 4 refused, 1 plays (wraps)
    assert_eq!(o.preset, Some(1));
    assert!(is_play(&o.action));
    let o = f.source.prev().await; // 4 refused, 3 plays
    assert_eq!(o.preset, Some(3));
    let o = f.source.prev().await; // 2 refused, 1 plays
    assert_eq!(o.preset, Some(1));
    assert!(is_play(&o.action));

    // Stopped: the armed preset steps over them too, and nothing plays.
    f.source.stop().await;
    let o = f.source.next().await;
    assert_eq!(o.action, SourceAction::Noop);
    assert_eq!(o.preset, Some(3));
}

#[tokio::test]
async fn when_no_other_preset_can_play_nothing_moves_and_the_refusal_is_said() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.source.select(1).await;
    for o in [f.source.next().await, f.source.prev().await] {
        assert_eq!(o.action, SourceAction::Noop);
        assert!(o.transient);
        assert_eq!(catalogue_knows(&o), "list_not_cached");
        assert!(o.identity.is_none() && o.preset.is_none());
    }
    assert_eq!(f.source.preset, 1);
    assert!(f.source.is_playing(), "what plays carries on");
    // Stopped: same.
    f.source.stop().await;
    let o = f.source.next().await;
    assert_eq!(catalogue_knows(&o), "list_not_cached");
    assert_eq!(f.source.preset, 1);
}

#[tokio::test]
async fn a_playlist_in_the_m3u_does_not_move_when_its_cache_is_replaced() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true), vid("bbbbbbbbbbb", "B", true)]);
    f.source.select(2).await;
    // A refresh lands a different list for the same id.
    f.cache("PLx", &[vid("ccccccccccc", "C", true), vid("ddddddddddd", "D", true), vid("eeeeeeeeeee", "E", true)]);
    let o = f.source.player_track(1).await;
    assert_eq!(o.identity, playing("bbbbbbbbbbb"), "still the second video of the m3u mpv was given");
}

#[tokio::test]
async fn a_refused_select_moves_nothing() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2), video_entry(3)]);
    f.source.select(1).await;
    f.source.select(7).await; // empty
    assert_eq!(f.source.preset, 1);
    f.source.select(2).await; // not cached
    assert_eq!(f.source.preset, 1);
    assert!(f.source.is_playing() && !f.source.plays_a_list(), "what played still plays");
    std::fs::remove_file(f.dir.path().join("bin/yt-dlp")).unwrap();
    f.source.select(3).await; // no tool
    assert_eq!(f.source.preset, 1);
    assert_eq!(ritornello_plugin_web_streaming::state::load(f.dir.path()), None, "nothing was remembered");
}

#[tokio::test]
async fn deactivate_forgets_what_played() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true), vid("bbbbbbbbbbb", "B", true)]);
    f.source.select(2).await;
    f.source.deactivate().await;
    assert!(f.source.player_track(1).await.identity.is_none());
    let o = f.source.next().await;
    assert_ne!(o.action, SourceAction::PlayerNext, "stopped, mpv's list is not ours to walk");
    assert!(!is_play(&o.action));
}

#[tokio::test]
async fn the_armed_preset_survives_a_restart() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2), video_entry(3)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true)]);
    let restarted = |f: &Fixture| WebStreamingSource::new(f.dir.path().to_path_buf(), f.source.entries.clone(), None);
    f.source.select(2).await;
    let o = restarted(&f).activate().await;
    assert!(is_play(&o.action));
    assert_eq!(o.preset, Some(2), "played the last preset, not the first");
    let o = restarted(&f).wake().await;
    assert_eq!(o.preset, Some(2));

    // Arming while stopped is remembered too.
    f.source.stop().await;
    f.source.next().await; // 2 -> 3
    assert_eq!(restarted(&f).activate().await.preset, Some(3));
}

#[tokio::test]
async fn the_armed_identity_of_a_playlist_is_the_video_that_would_play() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.source.select(1).await;
    let o = f.source.stop().await;
    assert_eq!(armed_identity(&o), Some(json!({"kind": "video", "id": ID})));

    // Cached, with unplayable entries first: the first PLAYABLE video.
    f.cache("PLx", &[vid("gone0000000", "Gone", false), vid("ab\ncdefghi", "Bad", true), vid("aaaaaaaaaaa", "A", true)]);
    f.source.set_preset(2);
    let o = f.source.stop().await;
    assert_eq!(armed_identity(&o), Some(json!({"kind": "video", "id": "aaaaaaaaaaa"})));
    // The same value the select declares as playing.
    let played = f.source.select(2).await;
    assert_eq!(played.identity, playing("aaaaaaaaaaa"));

    // Not cached: nothing is armed.
    let mut g = Fixture::new(vec![list_entry(2)]);
    g.source.set_preset(2);
    let o = g.source.stop().await;
    assert!(o.armed.is_none());
    assert_eq!(o.identity, Some(IdentityUpdate::Nothing));
}

#[tokio::test]
async fn a_playlist_file_that_cannot_be_written_says_so_and_plays_nothing() {
    let mut f = Fixture::new(vec![video_entry(1), list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true)]);
    f.source.select(1).await;
    // A directory where the file must go: the rename cannot succeed.
    std::fs::create_dir(f.dir.path().join("playlist.m3u")).unwrap();
    let o = f.source.select(2).await;
    assert!(!is_play(&o.action));
    assert_eq!(catalogue_knows(&o), "playlist_write_failed");
    assert_eq!(f.source.preset, 1, "and nothing moved");
}

// ---- fix round 2 ----

/// Runs `fut` on a thread of its own and gives it five seconds. A walk that
/// never returns (it has no `.await` to be cancelled at) would hang the whole
/// suite if it ran on the test's own runtime and `tokio::time::timeout` could
/// never fire; on its own thread it fails the test and is left behind.
fn within_5s<T: Send + 'static>(fut: impl std::future::Future<Output = T> + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _ = tx.send(rt.block_on(fut));
    });
    rx.recv_timeout(Duration::from_secs(5)).expect("the walk did not return within 5 seconds")
}

/// Steps `forward` from a current preset that is not in the table.
fn step_from_absent(entries: Vec<Entry>, current: u8, forward: bool) -> (SourceOutcome, u8) {
    let Fixture { dir, mut source } = Fixture::new(entries);
    source.preset = current;
    let out = within_5s(async move {
        let o = if forward { source.next().await } else { source.prev().await };
        (o, source.preset)
    });
    drop(dir);
    out
}

#[test]
fn a_current_preset_missing_from_the_table_with_every_other_refused_returns_and_moves_nothing() {
    // Both are playlists that were never cached; the current preset (3) is
    // in neither place.
    for forward in [true, false] {
        let (o, preset) = step_from_absent(vec![list_entry(2), list_entry(5)], 3, forward);
        assert_eq!(o.action, SourceAction::Noop, "forward={forward}");
        assert!(o.transient && o.identity.is_none());
        assert_eq!(catalogue_knows(&o), "list_not_cached");
        assert_eq!(preset, 3, "nothing moved");
    }
    // The case reported: a table of one refused preset.
    for forward in [true, false] {
        let (o, preset) = step_from_absent(vec![list_entry(2)], 3, forward);
        assert_eq!(catalogue_knows(&o), "list_not_cached", "forward={forward}");
        assert_eq!(preset, 3);
    }
}

#[test]
fn a_current_preset_missing_from_the_table_still_reaches_the_one_that_can_play() {
    // Forward from 3: 4 is refused, then the walk wraps to 2.
    let (o, preset) = step_from_absent(vec![video_entry(2), list_entry(4)], 3, true);
    assert!(o.armed.is_some(), "stopped: armed, not played");
    assert_eq!((o.preset, preset), (Some(2), 2));
    // Backward from 3: 2 plays at once.
    let (o, _) = step_from_absent(vec![video_entry(2), list_entry(4)], 3, false);
    assert_eq!(o.preset, Some(2));
    // Backward from 3 where the near one is refused: 2 refused, wraps to 4.
    let (o, preset) = step_from_absent(vec![list_entry(2), video_entry(4)], 3, false);
    assert_eq!((o.preset, preset), (Some(4), 4));
    assert!(o.armed.is_some());
}

#[test]
fn the_bound_still_lets_sparse_tables_and_a_lone_preset_behave() {
    let Fixture { dir, mut source } = Fixture::new(vec![video_entry(1), video_entry(99)]);
    let (a, b, c) = within_5s(async move {
        let a = source.next().await.preset; // 1 -> 99
        let b = source.next().await.preset; // 99 -> 1
        let c = source.prev().await.preset; // 1 -> 99
        (a, b, c)
    });
    assert_eq!((a, b, c), (Some(99), Some(1), Some(99)));
    drop(dir);

    // A lone playable preset, and it is the current one: still nothing to do.
    let Fixture { dir, mut source } = Fixture::new(vec![video_entry(1)]);
    let o = within_5s(async move { source.next().await });
    assert_eq!(o.action, SourceAction::Noop);
    assert!(o.status_text.is_none() && o.identity.is_none());
    drop(dir);
}

#[tokio::test]
async fn a_refresh_slot_is_freed_on_success_a_panic_and_an_abort() {
    let set = InFlight::default();
    let held = || set.lock().unwrap().len();

    // Success: freed exactly once, and a second run for the same id is
    // refused while the first one is running.
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let first = spawn_single_flight(&set, "PLx", async move {
        let _ = wait.await;
    })
    .unwrap();
    assert!(spawn_single_flight(&set, "PLx", async {}).is_none());
    assert_eq!(held(), 1);
    release.send(()).unwrap();
    first.await.unwrap();
    assert_eq!(held(), 0);

    // A panic.
    let h = spawn_single_flight(&set, "PLx", async { panic!("a refresh that dies") }).unwrap();
    assert!(h.await.is_err());
    assert_eq!(held(), 0, "a panic frees the slot");

    // An abort, even one that lands before the task ever ran.
    let h = spawn_single_flight(&set, "PLx", async { tokio::time::sleep(Duration::from_secs(60)).await }).unwrap();
    h.abort();
    let _ = h.await;
    assert_eq!(held(), 0, "an abort frees the slot");

    // And the id is usable again.
    assert!(spawn_single_flight(&set, "PLx", async {}).is_some());
}

// ---- other sites than YouTube ----

const TRACK: &str = "https://soundcloud.com/forss/flickermood";
const ALBUM: &str = "https://blazo.bandcamp.com/album/jazz-format-mixtape-vol-1";

fn web_video_entry(preset: u8, identity: Option<&str>) -> Entry {
    Entry {
        url: TRACK.into(),
        title: "Flickermood".into(),
        site: Some("Soundcloud".into()),
        identity: identity.map(str::to_string),
        ..video_entry(preset)
    }
}

fn album_entry(preset: u8) -> Entry {
    Entry { url: ALBUM.into(), title: "Jazz Format Mixtape vol.1".into(), site: Some("BandcampAlbum".into()), ..list_entry(preset) }
}

fn album_key() -> String {
    parse_link(ALBUM).unwrap().cache_key().unwrap()
}

/// The album capture's entries, read as the probe reads them, after `edit`.
fn album_videos(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<Video> {
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("bandcamp-album.json")).unwrap()).unwrap();
    edit(&mut raw);
    ritornello_plugin_web_streaming::ytdlp::parse_probe(&raw.to_string()).unwrap().entries
}

#[tokio::test]
async fn a_web_video_plays_its_link_under_its_extractor_and_id() {
    let mut f = Fixture::new(vec![web_video_entry(1, Some("Soundcloud:293")), web_video_entry(2, None)]);
    let o = f.source.select(1).await;
    assert_eq!(o.action, SourceAction::play(format!("ytdl://{TRACK}")).finite().ytdl_path(f.bin()));
    assert_eq!(o.identity, playing("Soundcloud:293"));
    assert_eq!(o.location.as_deref(), Some(TRACK));
    assert_eq!(armed_identity(&f.source.stop().await), Some(json!({"kind": "video", "id": "Soundcloud:293"})));
    // Saved without an identity: the link itself stands for one.
    let o = f.source.select(2).await;
    assert_eq!(o.identity, playing(TRACK));
}

#[tokio::test]
async fn a_web_playlist_writes_each_entry_s_own_address_and_names_it_by_extractor_and_id() {
    let mut f = Fixture::new(vec![album_entry(3)]);
    f.cache(&album_key(), &album_videos(|_| {}));
    let o = f.source.select(3).await;
    let m3u = std::fs::read_to_string(f.dir.path().join("playlist.m3u")).unwrap();
    let lines: Vec<&str> = m3u.lines().collect();
    assert_eq!(lines.len(), 22);
    assert_eq!(lines[0], "ytdl://https://blazo.bandcamp.com/track/intro");
    assert_eq!(o.identity, playing("Bandcamp:1353101989"));
    assert_eq!(o.location.as_deref(), Some("https://blazo.bandcamp.com/track/intro"));
    let t = f.source.player_track(1).await;
    assert_eq!(t.identity, playing("Bandcamp:38097443"));
    assert_eq!(t.location.as_deref(), Some("https://blazo.bandcamp.com/track/kero-one-keep-it-alive-blazo-remix"));
    assert_eq!(t.status_text, Some(video_title("Kero One - Keep It Alive (Blazo remix)")));
    f.source.stop().await;
    assert_eq!(
        armed_identity(&f.source.stop().await),
        Some(json!({"kind": "video", "id": "Bandcamp:1353101989"}))
    );
}

#[tokio::test]
async fn a_trapped_entry_address_is_dropped_before_the_m3u_and_the_index_space_is_the_m3u_s() {
    let mut f = Fixture::new(vec![album_entry(3)]);
    f.cache(
        &album_key(),
        &album_videos(|raw| {
            raw["entries"][0]["url"] = "https://blazo.bandcamp.com/track/intro\n#EXTINF:1,x\nfile:///etc/passwd".into();
            raw["entries"][2]["url"] = "file:///etc/passwd".into();
        }),
    );
    let o = f.source.select(3).await;
    let m3u = std::fs::read_to_string(f.dir.path().join("playlist.m3u")).unwrap();
    assert_eq!(m3u.lines().count(), 20, "{m3u}");
    assert!(!m3u.contains("EXTINF") && !m3u.contains("file:"), "{m3u}");
    assert!(m3u.lines().all(|l| l.starts_with("ytdl://https://blazo.bandcamp.com/track/")), "{m3u}");
    // The first line is the album's second entry, and so is mpv's index 0.
    assert_eq!(o.identity, playing("Bandcamp:38097443"));
    assert_eq!(f.source.player_track(1).await.identity, playing("Bandcamp:2929630012"));
    assert!(f.source.player_track(20).await.identity.is_none());
}

#[tokio::test]
async fn a_web_playlist_is_refreshed_from_its_own_link_under_its_own_key() {
    let mut f = Fixture::new(vec![album_entry(3)]);
    let args = f.dir.path().join("args");
    install_tool(
        f.dir.path(),
        &format!("printf '%s\\n' \"$@\" > '{}'\ncat '{}'", args.display(), fixtures().join("bandcamp-album.json").display()),
    );
    let o = f.source.select(3).await; // not cached
    assert_eq!(catalogue_knows(&o), "list_not_cached");
    settle(&f.source).await;
    assert_eq!(std::fs::read_to_string(&args).unwrap(), format!("-J\n--flat-playlist\n--\n{ALBUM}\n"));
    assert_eq!(ListCache::new(f.dir.path()).load(&album_key()).unwrap().len(), 22);
    assert!(is_play(&f.source.select(3).await.action));
}


// ---- fix round 1: every address through the hook, untitled tracks ----

#[tokio::test]
async fn every_address_mpv_is_handed_goes_through_the_hook() {
    // A YouTube video, a web video, and a web list: ytdl:// each time, the
    // plain address as the location.
    let mut f = Fixture::new(vec![video_entry(1), web_video_entry(2, Some("Soundcloud:293")), album_entry(3)]);
    f.cache(&album_key(), &album_videos(|_| {}));
    let o = f.source.select(1).await;
    assert_eq!(o.action, SourceAction::play(format!("ytdl://{WATCH}")).finite().ytdl_path(f.bin()));
    assert_eq!(o.location.as_deref(), Some(WATCH));
    let o = f.source.select(2).await;
    assert_eq!(o.action, SourceAction::play(format!("ytdl://{TRACK}")).finite().ytdl_path(f.bin()));
    assert_eq!(o.location.as_deref(), Some(TRACK));
    f.source.select(3).await;
    let m3u = std::fs::read_to_string(f.dir.path().join("playlist.m3u")).unwrap();
    assert_eq!(m3u.lines().count(), 22);
    assert!(m3u.lines().all(|l| l.starts_with("ytdl://https://blazo.bandcamp.com/track/")), "{m3u}");
}

fn set_entry(preset: u8) -> Entry {
    Entry {
        url: "https://soundcloud.com/the-concept-band/sets/the-royal-concept-ep".into(),
        title: "The Royal Concept EP".into(),
        ..list_entry(preset)
    }
}

fn set_videos() -> Vec<Video> {
    let raw = std::fs::read_to_string(fixtures().join("soundcloud-set.json")).unwrap();
    ritornello_plugin_web_streaming::ytdlp::parse_probe(&raw).unwrap().entries
}

fn untitled(n: usize) -> Option<Text> {
    Some(Text::Keyed { key: "untitled_track".into(), params: HashMap::from([("n".to_string(), n.to_string())]) })
}

#[tokio::test]
async fn a_set_s_untitled_tracks_are_named_on_the_status_line() {
    let mut f = Fixture::new(vec![set_entry(4)]);
    let key = parse_link("https://soundcloud.com/the-concept-band/sets/the-royal-concept-ep").unwrap().cache_key().unwrap();
    f.cache(&key, &set_videos());
    let o = f.source.select(4).await;
    assert_eq!(o.status_text, Some(video_title("world-on-fire-1")));
    assert_eq!(f.source.player_track(3).await.status_text, Some(video_title("the-concept-d-d-dance")));
    // The last one's address is `/tracks/47127631`: its position instead.
    let t = f.source.player_track(5).await;
    assert_eq!(t.status_text, untitled(6));
    assert_eq!(catalogue_knows(&t), "untitled_track");
}

/// A source wired to the admin half's save notification, with the table it
/// shares.
fn saved_source(
    entries: Vec<Entry>,
) -> (tempfile::TempDir, WebStreamingSource, Arc<AsyncRwLock<Entries>>, tokio::sync::watch::Sender<()>) {
    let dir = tempfile::tempdir().unwrap();
    install_tool(dir.path(), "exit 1");
    let (tx, rx) = tokio::sync::watch::channel(());
    let table = Arc::new(AsyncRwLock::new(Entries { entry: entries }));
    let source = WebStreamingSource::new(dir.path().to_path_buf(), table.clone(), Some(rx));
    (dir, source, table, tx)
}

/// The radio's `a_table_reshuffle_corrects_the_number_of_what_is_playing`,
/// for the same defect: the preset is a position, so a reorder on the page
/// made the card name another preset for the video that kept playing, Stop
/// arm that other one, and a reboot play it.
#[tokio::test]
async fn a_table_reshuffle_corrects_the_number_of_what_is_playing() {
    let (dir, mut source, table, tx) = saved_source(vec![video_entry(1), list_entry(2)]);
    assert!(is_play(&source.select(1).await.action));
    // The page swaps the two presets.
    table.write().await.entry = vec![list_entry(1), video_entry(2)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!(n.preset, Some(2), "the number follows the video");
    assert_eq!(n.preset_name.as_deref(), Some("Never gonna"));
    assert_eq!(source.preset, 2, "next/prev start from there");
    assert_eq!(state::load(dir.path()), Some(2), "and a reboot resumes it");
    // Nothing that plays is touched.
    assert!(n.identity.is_none() && n.status_text.is_none() && n.play_request.is_none());
    // Stop arms the video that played, not what took its old number.
    let o = source.stop().await;
    assert_eq!((o.preset, o.preset_name.as_deref()), (Some(2), Some("Never gonna")));
}

/// The video followed is the one selected, not the one armed at start.
#[tokio::test]
async fn a_selected_video_is_followed_across_a_reshuffle() {
    let other = Entry { url: watch("aaaaaaaaaaa"), title: "Other".into(), ..video_entry(2) };
    let (_dir, mut source, table, tx) = saved_source(vec![video_entry(1), other.clone()]);
    assert!(is_play(&source.select(2).await.action));
    table.write().await.entry = vec![Entry { preset: 1, ..other }, video_entry(2)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!((n.preset, n.preset_name.as_deref()), (Some(1), Some("Other")));
}

/// A playing playlist is followed the same way.
#[tokio::test]
async fn a_playing_playlist_is_followed_across_a_reshuffle() {
    let (dir, mut source, table, tx) = saved_source(vec![video_entry(1), list_entry(2)]);
    ListCache::new(dir.path()).store("PLx", &[vid("aaaaaaaaaaa", "A", true)]).unwrap();
    assert!(is_play(&source.select(2).await.action));
    table.write().await.entry = vec![list_entry(1), video_entry(2)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!((n.preset, n.preset_name.as_deref()), (Some(1), Some("A list")));
}

/// A rename shows at once, not at the next play.
#[tokio::test]
async fn a_renamed_preset_is_named_again_while_it_plays() {
    let (_dir, mut source, table, tx) = saved_source(vec![video_entry(1)]);
    source.select(1).await;
    table.write().await.entry[0].title = "Renamed".into();
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!((n.preset, n.preset_name.as_deref()), (Some(1), Some("Renamed")));
}

/// The radio's `a_removed_station_does_not_make_up_a_number`: deleted while
/// it plays, it keeps playing, and no number is declared rather than one
/// that names another preset.
#[tokio::test]
async fn a_removed_preset_does_not_make_up_a_number() {
    let (dir, mut source, table, tx) = saved_source(vec![video_entry(1), list_entry(2)]);
    source.select(1).await;
    table.write().await.entry = vec![list_entry(1)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!(n.preset, None, "no number made up");
    assert_eq!(n.preset_name, None);
    assert!(n.identity.is_none() && n.play_request.is_none(), "it keeps playing");
    assert_eq!(source.preset, 1, "nothing moved");
    assert_eq!(state::load(dir.path()), None, "nothing persisted");
}

/// Stopped, the armed preset is followed the same way, and Play (after a
/// reboot too) starts it under its new number.
#[tokio::test]
async fn a_reshuffle_moves_the_armed_preset_with_its_entry() {
    let (dir, mut source, table, tx) = saved_source(vec![video_entry(1), list_entry(2)]);
    source.select(1).await;
    source.stop().await;
    table.write().await.entry = vec![list_entry(1), video_entry(3)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!((n.preset, n.preset_name.as_deref()), (Some(3), Some("Never gonna")));
    assert_eq!(state::load(dir.path()), Some(3));
    let o = source.activate().await;
    assert_eq!(o.preset, Some(3));
    assert_eq!(o.identity, playing(ID));
}

/// A preset armed at start (from `state.json`) is followed too, before
/// anything was played.
#[tokio::test]
async fn the_preset_armed_at_start_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    install_tool(dir.path(), "exit 1");
    state::save(dir.path(), 2).unwrap();
    let (tx, rx) = tokio::sync::watch::channel(());
    let table = Arc::new(AsyncRwLock::new(Entries { entry: vec![list_entry(1), video_entry(2)] }));
    let mut source = WebStreamingSource::new(dir.path().to_path_buf(), table.clone(), Some(rx));
    table.write().await.entry = vec![video_entry(1), list_entry(2)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!((n.preset, n.preset_name.as_deref()), (Some(1), Some("Never gonna")));
    assert_eq!(state::load(dir.path()), Some(1));
}

/// Next while stopped arms another preset without playing it: that one is
/// what a save then follows, not the one played before.
#[tokio::test]
async fn a_preset_armed_by_next_is_the_one_followed() {
    let other = Entry { url: watch("aaaaaaaaaaa"), title: "Other".into(), ..video_entry(2) };
    let (_dir, mut source, table, tx) = saved_source(vec![video_entry(1), other.clone()]);
    source.select(1).await;
    source.stop().await;
    source.next().await;
    assert_eq!(source.preset, 2);
    table.write().await.entry = vec![Entry { preset: 1, ..other }, video_entry(2)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!((n.preset, n.preset_name.as_deref()), (Some(1), Some("Other")));
}

/// Stopped, and the armed entry is gone: re-armed from the number, as Play
/// would start it — what now sits there, or nothing.
#[tokio::test]
async fn a_removed_armed_preset_is_re_armed_from_its_number() {
    let (_dir, mut source, table, tx) = saved_source(vec![video_entry(1), video_entry(2)]);
    table.write().await.entry[1].url = watch("bbbbbbbbbbb");
    source.select(2).await;
    source.stop().await;
    let other = Entry { url: watch("aaaaaaaaaaa"), title: "Other".into(), ..video_entry(2) };
    table.write().await.entry = vec![video_entry(1), other];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!((n.preset, n.preset_name.as_deref()), (Some(2), Some("Other")));
    assert_eq!(n.armed.map(|a| a.identity), Some(json!({"kind": "video", "id": "aaaaaaaaaaa"})));
    // And nothing at the number: nothing armed.
    table.write().await.entry = vec![video_entry(1)];
    tx.send(()).unwrap();
    let n = source.poll_notification().await.unwrap();
    assert_eq!(n.identity, Some(IdentityUpdate::Nothing));
    assert!(n.armed.is_none() && n.preset.is_none());
}

/// What the wrapper writes when a run it served for mpv failed.
fn record_failure(dir: &Path, time: f64, code: &str) {
    let record = json!({"time": time, "host": "www.youtube.com", "code": code, "detail": "ERROR: gone"});
    std::fs::write(dir.join(ritornello_plugin_web_streaming::playfail::FILE), record.to_string()).unwrap();
}

fn epoch_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

/// The Stop the core sends when a load went idle without a sound says why,
/// once: a failure recorded since this playback started, by its code.
#[tokio::test]
async fn a_failed_playback_is_said_on_the_stop_that_follows_once() {
    for (code, key) in [
        ("guard_refused", "play_failed_guard_refused"),
        ("live_refused", "play_failed_live_refused"),
        ("no_formats", "play_failed_no_formats"),
        ("ytdlp_failed", "play_failed_ytdlp_failed"),
    ] {
        let mut f = Fixture::new(vec![video_entry(1)]);
        f.source.select(1).await;
        record_failure(f.dir.path(), epoch_now() + 0.5, code);
        let o = f.source.stop().await;
        assert_eq!(o.status_text, Some(keyed(key)), "{code}");
        assert_eq!(catalogue_knows(&o), key);
        assert_eq!(o.preset, Some(1), "still armed");
        // Said once: a second Stop has no playback to blame it on.
        assert_eq!(f.source.stop().await.status_text, None, "{code}");
    }
}

/// A failure recorded before this playback is another one's: not said.
#[tokio::test]
async fn an_older_failure_is_not_blamed_on_this_playback() {
    let mut f = Fixture::new(vec![video_entry(1)]);
    record_failure(f.dir.path(), epoch_now() - 5.0, "ytdlp_failed");
    f.source.select(1).await;
    assert_eq!(f.source.stop().await.status_text, None);
}

/// In a list, mpv skips an entry that failed and moves on: once it has, the
/// failure is not this playback's end.
#[tokio::test]
async fn a_failed_list_entry_that_mpv_moved_past_is_not_said() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    f.cache("PLx", &[vid("aaaaaaaaaaa", "A", true), vid("bbbbbbbbbbb", "B", true)]);
    f.source.select(2).await;
    record_failure(f.dir.path(), epoch_now(), "ytdlp_failed");
    std::thread::sleep(std::time::Duration::from_millis(20));
    f.source.player_track(1).await;
    assert_eq!(f.source.stop().await.status_text, None);
}

// ---- the cover of what plays ------------------------------------------------

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 7];

/// A fetch that answers `JPEG` for every address and records them; an address
/// containing `held` waits for `gate` first.
fn fetch_held(calls: Arc<Mutex<Vec<String>>>, held: &'static str, gate: Arc<tokio::sync::Semaphore>) -> CoverFetch {
    Arc::new(move |url: String| {
        calls.lock().unwrap().push(url.clone());
        let gate = gate.clone();
        Box::pin(async move {
            if url.contains(held) {
                gate.acquire().await.unwrap().forget();
            }
            Some(JPEG.to_vec())
        })
    })
}

fn with_thumb(mut v: Video, url: &str) -> Video {
    v.thumbnail_jpg = Some(url.into());
    v
}

/// The next notification, or `None` when none comes within a while.
async fn next_notice(f: &mut Fixture) -> Option<Notification> {
    tokio::time::timeout(Duration::from_millis(500), f.source.poll_notification()).await.ok().flatten()
}

fn announced_path(n: &Notification) -> PathBuf {
    match &n.cover {
        Some(CoverRef::Path { path }) => PathBuf::from(path),
        other => panic!("expected a cover by path, got {other:?}"),
    }
}

/// The regression: a played video never had a cover, only a downloaded one.
/// A single video's page is probed (its preset keeps no thumbnail), and the
/// picture, fetched by the plugin, is handed over as a file: never as an
/// address for the core to fetch outside the plugin's guard.
#[tokio::test]
async fn a_single_video_shows_its_sites_thumbnail_fetched_by_the_plugin() {
    let mut f = Fixture::new(vec![video_entry(1)]);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    install_tool(f.dir.path(), &format!("cat '{}'", fixtures.join("video.json").display()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    f.source.fetch = fetch_held(calls.clone(), "never", Arc::new(tokio::sync::Semaphore::new(0)));
    f.source.select(1).await;
    let n = next_notice(&mut f).await.expect("a cover is announced");
    let path = announced_path(&n);
    assert_eq!(std::fs::read(&path).unwrap(), JPEG);
    assert!(path.starts_with(f.dir.path().join("covers")), "{}", path.display());
    let probe = ritornello_plugin_web_streaming::ytdlp::parse_probe(&std::fs::read_to_string(fixtures.join("video.json")).unwrap()).unwrap();
    assert_eq!(*calls.lock().unwrap(), [probe.thumbnail_jpg.unwrap()], "the very thumbnail a download takes");
    assert!(n.identity.is_none() && n.status_text.is_none(), "nothing else moves");

    // Played again: from disk, neither probed nor fetched again.
    install_tool(f.dir.path(), "exit 1");
    f.source.stop().await;
    f.source.select(1).await;
    assert_eq!(announced_path(&next_notice(&mut f).await.unwrap()), path);
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn each_video_of_a_list_shows_its_own_thumbnail_and_a_late_one_is_dropped() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    f.cache(
        "PLx",
        &[
            with_thumb(vid("aaaaaaaaaaa", "A", true), "https://i.ytimg.com/vi/aaaaaaaaaaa/a.jpg"),
            with_thumb(vid("bbbbbbbbbbb", "B", true), "https://i.ytimg.com/vi/bbbbbbbbbbb/b.jpg"),
        ],
    );
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let calls = Arc::new(Mutex::new(Vec::new()));
    f.source.fetch = fetch_held(calls.clone(), "/a.jpg", gate.clone());
    f.source.select(2).await;
    // mpv moves on before A's picture arrives: B's is announced, A's never.
    f.source.player_track(1).await;
    let n = next_notice(&mut f).await.expect("B's cover");
    assert_eq!(announced_path(&n), f.source.covers.path_for(&vid("bbbbbbbbbbb", "B", true).identity()));
    gate.add_permits(1);
    assert!(next_notice(&mut f).await.is_none(), "A no longer plays");
    assert_eq!(
        *calls.lock().unwrap(),
        ["https://i.ytimg.com/vi/aaaaaaaaaaa/a.jpg", "https://i.ytimg.com/vi/bbbbbbbbbbb/b.jpg"],
        "each from its own entry, no probe",
    );
}

#[tokio::test]
async fn a_cover_that_arrives_after_stop_is_not_announced() {
    let mut f = Fixture::new(vec![list_entry(2)]);
    f.cache("PLx", &[with_thumb(vid("aaaaaaaaaaa", "A", true), "https://i.ytimg.com/vi/aaaaaaaaaaa/a.jpg")]);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    f.source.fetch = fetch_held(Arc::new(Mutex::new(Vec::new())), "/a.jpg", gate.clone());
    f.source.select(2).await;
    f.source.stop().await;
    gate.add_permits(1);
    assert!(next_notice(&mut f).await.is_none());
}
