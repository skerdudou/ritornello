use super::*;
use crate::download::{CoverFetch, Limits};
use crate::entries::{Entry, ListCache};
use crate::ytdlp::{Kind, Ytdlp};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

const VIDEO_URL: &str = "https://www.youtube.com/watch?v=jNQXAC9IVRw";

struct World {
    data: tempfile::TempDir,
    out: PathBuf,
    _out_guard: tempfile::TempDir,
    jobs: Jobs,
    log: PathBuf,
}

fn target() -> Target {
    Target { root: "nas".into(), path: String::new() }
}

fn no_cover() -> CoverFetch {
    Arc::new(|_| Box::pin(async { None }))
}

/// A world with a fake yt-dlp whose "download" runs `hook` first, and a
/// resolver that always lands in a temporary folder.
fn world_with(hook: &str, resolver: Option<Resolver>) -> World {
    world_full(hook, resolver, "video.json", false)
}

/// `probe_file` is a capture of tests/fixtures, or `MISSING` for a probe that
/// fails; `slow_probe` makes the probe of the video preset take a second.
fn world_full(hook: &str, resolver: Option<Resolver>, probe_file: &str, slow_probe: bool) -> World {
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir(data.path().join("bin")).unwrap();
    let ytdlp = Ytdlp::new(data.path());
    let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(probe_file);
    let log = data.path().join("calls.log");
    let script = format!(
        r#"#!/bin/sh
case "$1" in -J) echo probe >> '{log}.probes'; {slow}; cat '{probe}' || exit 1; exit 0;; esac
out=; url=
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift;; --) url="$2"; break;; esac; shift; done
echo "$url" >> '{log}'
{hook}
file=$(printf '%s' "$out" | sed 's/%(ext)s/m4a/')
printf 'audio' > "$file"
"#,
        probe = probe.display(),
        log = log.display(),
        slow = if slow_probe { r#"case "$4" in *jNQXAC9IVRw) sleep 1;; esac"# } else { ":" },
    );
    std::fs::write(ytdlp.bin(), script).unwrap();
    std::fs::set_permissions(ytdlp.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();

    let out_guard = tempfile::tempdir().unwrap();
    let out = std::fs::canonicalize(out_guard.path()).unwrap();
    let resolved = Resolved { base: out.clone(), dir: out.clone() };
    let resolver = resolver.unwrap_or_else(|| {
        Arc::new(move |_| {
            let r = resolved.clone();
            Box::pin(async move { Ok(r) })
        })
    });
    let entries = Entries {
        entry: vec![
            Entry { preset: 1, url: VIDEO_URL.into(), kind: Kind::Video, title: "Me at the zoo".into(), channel: None, count: None, site: None, identity: None },
            Entry { preset: 2, url: "https://www.youtube.com/playlist?list=PLfail".into(), kind: Kind::Playlist, title: "A list".into(), channel: None, count: None, site: None, identity: None },
        ],
    };
    let ctx = Ctx {
        ytdlp: Arc::new(ytdlp),
        lists: ListCache::new(data.path()),
        limits: Limits { fetch_cover: no_cover(), ..Limits::default() },
    };
    let jobs = Jobs::start(data.path(), Arc::new(AsyncRwLock::new(entries)), ctx, resolver);
    World { data, out, _out_guard: out_guard, jobs, log }
}

fn world(hook: &str) -> World {
    world_with(hook, None)
}

fn job(w: &World, id: JobId) -> Job {
    w.jobs.view().lock().unwrap().jobs.iter().find(|j| j.id == id).cloned().unwrap()
}

async fn until(w: &World, id: JobId, what: &str, ok: impl Fn(&Job) -> bool) -> Job {
    let started = Instant::now();
    loop {
        let j = job(w, id);
        if ok(&j) {
            return j;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "job {id} never {what}: {j:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn finished(w: &World, id: JobId) -> Job {
    until(w, id, "finished", |j| j.state.is_over()).await
}

#[tokio::test]
async fn a_job_downloads_its_presets_and_records_each_video() {
    let w = world("");
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    assert_eq!(j.state, JobState::Done);
    assert_eq!(j.rows.len(), 1);
    assert_eq!(j.rows[0].preset, 1);
    assert_eq!(j.rows[0].id, "jNQXAC9IVRw");
    assert_eq!(j.rows[0].state, VideoState::Done);
    assert_eq!(j.rows[0].percent, Some(100.0));
    assert!(w.out.join("Me at the zoo.m4a").is_file());
}

#[tokio::test]
async fn a_preset_that_does_not_exist_is_a_failed_row_and_the_job_goes_on() {
    let w = world("");
    let id = w.jobs.enqueue(vec![7, 1], target());
    let j = finished(&w, id).await;
    assert_eq!(j.state, JobState::Done);
    assert_eq!(j.rows[0].state, VideoState::Failed("no such preset".into()));
    assert_eq!(j.rows[1].state, VideoState::Done);
}

#[tokio::test]
async fn a_refused_target_fails_the_job_without_running_anything() {
    let refuse: Resolver = Arc::new(|_| Box::pin(async { Err(TargetError::NotWritable) }));
    let w = world_with("", Some(refuse));
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    assert_eq!(j.state, JobState::Failed("the root is not writable".into()));
    assert!(!w.log.exists(), "the tool was never run");
}

#[tokio::test]
async fn jobs_run_one_at_a_time_in_order() {
    let w = world("sleep 0.3");
    let a = w.jobs.enqueue(vec![1], target());
    let b = w.jobs.enqueue(vec![1], target());
    let c = w.jobs.enqueue(vec![1], target());
    let started = Instant::now();
    let mut max_running = 0;
    loop {
        let running = w.jobs.view().lock().unwrap().jobs.iter().filter(|j| j.state == JobState::Running).count();
        max_running = max_running.max(running);
        let all_over = [a, b, c].iter().all(|id| job(&w, *id).state.is_over());
        if all_over {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(max_running, 1, "never two at once, and at least one at a time");
    // 3 x 0.3 s of work cannot have overlapped.
    assert!(started.elapsed() >= Duration::from_millis(850), "{:?}", started.elapsed());
}

#[tokio::test]
async fn cancelling_the_running_job_kills_its_group_and_the_next_one_still_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let hook = format!("if [ ! -e '{p}' ]; then sleep 30 &\necho $! > '{p}'\nwait\nfi", p = pidfile.display());
    let w = world(&hook);
    let a = w.jobs.enqueue(vec![1], target());
    let b = w.jobs.enqueue(vec![1], target());
    let pid = {
        let started = Instant::now();
        loop {
            if let Ok(t) = std::fs::read_to_string(&pidfile)
                && !t.trim().is_empty()
            {
                break t.trim().to_string();
            }
            assert!(started.elapsed() < Duration::from_secs(20));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert!(w.jobs.cancel(a));
    let ja = finished(&w, a).await;
    assert_eq!(ja.state, JobState::Cancelled);
    assert_eq!(ja.rows[0].state, VideoState::Cancelled);
    let started = Instant::now();
    while std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.rsplit_once(')').is_some_and(|(_, r)| r.trim_start().starts_with('Z'))) {
        assert!(started.elapsed() < Duration::from_secs(10), "the grandchild {pid} lives on");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The worker was not lost with the job.
    assert_eq!(finished(&w, b).await.state, JobState::Done);
}

#[tokio::test]
async fn a_queued_job_that_is_cancelled_never_starts() {
    let w = world("sleep 0.5");
    let a = w.jobs.enqueue(vec![1], target());
    let b = w.jobs.enqueue(vec![1], target());
    assert!(w.jobs.cancel(b));
    assert_eq!(finished(&w, b).await.state, JobState::Cancelled);
    assert_eq!(finished(&w, a).await.state, JobState::Done);
    let log = std::fs::read_to_string(&w.log).unwrap();
    assert_eq!(log.lines().count(), 1, "only a ran: {log}");
    assert!(job(&w, b).rows.is_empty());
    let probes = std::fs::read_to_string(format!("{}.probes", w.log.display())).unwrap();
    assert_eq!(probes.lines().count(), 1, "the cancelled job did not even look anything up: {probes}");
}

#[tokio::test]
async fn cancelling_an_unknown_or_finished_job_says_so() {
    let w = world("");
    assert!(!w.jobs.cancel(99));
    let a = w.jobs.enqueue(vec![1], target());
    finished(&w, a).await;
    // The worker forgets a job's token only after writing its end state.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!w.jobs.cancel(a));
}

#[tokio::test]
async fn jobs_json_keeps_the_last_twenty() {
    let refuse: Resolver = Arc::new(|_| Box::pin(async { Err(TargetError::NotMounted) }));
    let w = world_with("", Some(refuse));
    let ids: Vec<JobId> = (0..25).map(|_| w.jobs.enqueue(vec![1], target())).collect();
    finished(&w, *ids.last().unwrap()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let on_disk: JobsView = serde_json::from_str(&std::fs::read_to_string(w.data.path().join("jobs.json")).unwrap()).unwrap();
    assert_eq!(on_disk.jobs.len(), KEPT_JOBS);
    assert_eq!(on_disk.jobs.first().unwrap().id, ids[5]);
    assert_eq!(on_disk.jobs.last().unwrap().id, ids[24]);
    assert_eq!(*w.jobs.view().lock().unwrap(), on_disk, "what is read back is what the view says");
    assert_eq!(files(w.data.path()).iter().filter(|f| f.contains("jobs.json.tmp")).count(), 0);
}

fn files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

#[tokio::test]
async fn the_view_round_trips_through_json_with_rows_of_every_state() {
    let view = JobsView {
        jobs: vec![Job {
            id: 3,
            presets: vec![1, 2],
            target: Target { root: "nas".into(), path: "a/b".into() },
            state: JobState::Failed("why".into()),
            rows: vec![
                Row { preset: 1, id: "a".into(), title: "A".into(), state: VideoState::Pending, percent: None, notes: Vec::new() },
                Row { preset: 1, id: "b".into(), title: "B".into(), state: VideoState::Running, percent: Some(12.5), notes: Vec::new() },
                Row { preset: 1, id: "c".into(), title: "C".into(), state: VideoState::Done, percent: Some(100.0), notes: Vec::new() },
                Row { preset: 1, id: "d".into(), title: "D".into(), state: VideoState::Failed("ERROR: x".into()), percent: None, notes: Vec::new() },
                Row { preset: 2, id: "e".into(), title: "E".into(), state: VideoState::Skipped("unavailable".into()), percent: None, notes: Vec::new() },
                Row { preset: 2, id: "f".into(), title: "F".into(), state: VideoState::Cancelled, percent: None, notes: Vec::new() },
                Row { preset: 2, id: "g".into(), title: "G".into(), state: VideoState::Converting, percent: None, notes: Vec::new() },
                Row {
                    preset: 2,
                    id: "h".into(),
                    title: "H".into(),
                    state: VideoState::Done,
                    percent: Some(100.0),
                    notes: vec![Note::ConversionFailed("boom".into()), Note::CoverBeside],
                },
            ],
            options: JobOptions {
                format: Format::Mp3,
                cover: CoverMode::Embedded,
                names: [(1, "Mine".to_string())].into(),
                trims: [(1, crate::download::Trim { start: Some(83), end: None })].into(),
            },
        }],
    };
    let text = serde_json::to_string(&view).unwrap();
    assert_eq!(serde_json::from_str::<JobsView>(&text).unwrap(), view);
}

#[tokio::test]
async fn a_job_left_running_by_a_previous_run_is_failed_and_ids_go_on() {
    let data = tempfile::tempdir().unwrap();
    let old = JobsView {
        jobs: vec![Job {
            id: 41,
            presets: vec![1],
            target: target(),
            state: JobState::Running,
            rows: vec![
                Row { preset: 1, id: "a".into(), title: "A".into(), state: VideoState::Running, percent: Some(3.0), notes: Vec::new() },
                Row { preset: 1, id: "b".into(), title: "B".into(), state: VideoState::Converting, percent: None, notes: Vec::new() },
            ],
            options: JobOptions::default(),
        }],
    };
    std::fs::write(data.path().join("jobs.json"), serde_json::to_string(&old).unwrap()).unwrap();
    let ctx = Ctx { ytdlp: Arc::new(Ytdlp::new(data.path())), lists: ListCache::new(data.path()), limits: Limits::default() };
    let refuse: Resolver = Arc::new(|_| Box::pin(async { Err(TargetError::NotMounted) }));
    let jobs = Jobs::start(data.path(), Arc::new(AsyncRwLock::new(Entries::default())), ctx, refuse);
    {
        let view = jobs.view();
        let view = view.lock().unwrap();
        assert_eq!(view.jobs[0].state, JobState::Failed("interrupted by a restart".into()));
        assert_eq!(view.jobs[0].rows[0].state, VideoState::Failed("interrupted by a restart".into()));
    }
    assert_eq!(jobs.enqueue(vec![1], target()), 42);
}

#[tokio::test]
async fn the_real_resolver_reads_the_files_plugins_roots_and_the_mount_table() {
    let dir = tempfile::tempdir().unwrap();
    let roots = dir.path().join("media-roots.toml");
    let resolver = resolver_for(roots.clone(), Duration::from_secs(2));
    // No roots file: an error, not a hang and not a panic.
    assert!(matches!(resolver(target()).await, Err(TargetError::Io(_))));
    std::fs::write(&roots, "[[root]]\nname = \"nas-not-mounted-here\"\nkind = \"smb\"\nhost = \"h\"\nshare = \"s\"\nwritable = true\n").unwrap();
    let t = Target { root: "nas-not-mounted-here".into(), path: String::new() };
    assert_eq!(resolver(t).await, Err(TargetError::NotMounted));
    assert_eq!(resolver(target()).await, Err(TargetError::UnknownRoot));
}

fn on_disk(w: &World) -> Option<JobsView> {
    serde_json::from_str(&std::fs::read_to_string(w.data.path().join("jobs.json")).ok()?).ok()
}

#[tokio::test]
async fn clearing_forgets_the_jobs_that_are_over_and_never_one_queued_or_running() {
    // Each job waits at its resolver for a permit: two are let through, the
    // third is running (held there) and the fourth queued behind it.
    let gate = Arc::new(tokio::sync::Semaphore::new(2));
    let g = gate.clone();
    let resolver: Resolver = Arc::new(move |_| {
        let g = g.clone();
        Box::pin(async move {
            g.acquire().await.unwrap().forget();
            Err(TargetError::NotMounted)
        })
    });
    let w = world_with("", Some(resolver));
    let ids: Vec<JobId> = (0..4).map(|_| w.jobs.enqueue(vec![1], target())).collect();
    finished(&w, ids[1]).await;
    until(&w, ids[2], "started", |j| j.state == JobState::Running).await;

    w.jobs.clear_finished();
    let left: Vec<JobId> = w.jobs.view().lock().unwrap().jobs.iter().map(|j| j.id).collect();
    assert_eq!(left, [ids[2], ids[3]]);
    let started = Instant::now();
    while on_disk(&w).is_none_or(|v| v.jobs.len() != 2) {
        assert!(started.elapsed() < Duration::from_secs(10), "jobs.json still holds the jobs cleared: {:?}", on_disk(&w));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The ones kept still run to their end.
    gate.add_permits(2);
    assert!(finished(&w, ids[3]).await.state.is_over());
}

#[tokio::test]
async fn an_unfinished_job_is_never_dropped_however_long_the_queue() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let g = gate.clone();
    let resolver: Resolver = Arc::new(move |_| {
        let g = g.clone();
        Box::pin(async move {
            g.acquire().await.unwrap().forget();
            Err(TargetError::NotMounted)
        })
    });
    let w = world_with("", Some(resolver));
    let ids: Vec<JobId> = (0..25).map(|_| w.jobs.enqueue(vec![1], target())).collect();
    let started = Instant::now();
    loop {
        if on_disk(&w).is_some_and(|v| v.jobs.len() == 25) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "jobs.json never held the 25 queued jobs");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(w.jobs.view().lock().unwrap().jobs.len(), 25);
    // Once they are over, only the last twenty remain.
    gate.add_permits(25);
    finished(&w, *ids.last().unwrap()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(on_disk(&w).unwrap().jobs.len(), KEPT_JOBS);
}

#[tokio::test]
async fn cancelling_leaves_no_row_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild");
    let hook = format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display());
    let w = world_full(&hook, None, "playlist.json", false);
    let id = w.jobs.enqueue(vec![1], target());
    let started = Instant::now();
    while !std::fs::read_to_string(&pidfile).is_ok_and(|t| !t.trim().is_empty()) {
        assert!(started.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(w.jobs.cancel(id));
    let j = finished(&w, id).await;
    assert_eq!(j.state, JobState::Cancelled);
    assert_eq!(j.rows.len(), 12);
    assert_settled(&j);
    assert!(j.rows.iter().all(|r| r.state == VideoState::Cancelled), "{:?}", j.rows.iter().map(|r| &r.state).collect::<Vec<_>>());
}

/// The invariant: a job that is over leaves no row Pending or Running.
fn assert_settled(j: &Job) {
    assert!(j.state.is_over());
    let open: Vec<&VideoState> = j.rows.iter().map(|r| &r.state).filter(|s| matches!(s, VideoState::Pending | VideoState::Running)).collect();
    assert!(open.is_empty(), "{:?} left {} rows open", j.state, open.len());
}

#[tokio::test]
async fn a_preset_that_fails_after_planning_leaves_no_row_pending_and_no_duplicate() {
    // The playlist's folder is a symlink to a folder with a `$`: refused after
    // the 12 videos were planned.
    let w = world_full("", None, "playlist.json", false);
    let real = w.out.join("real$dir");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, w.out.join("Popular Music Videos")).unwrap();
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    assert_settled(&j);
    assert_eq!(j.rows.len(), 12, "one row per video, no summary row beside them");
    assert!(j.rows.iter().all(|r| matches!(r.state, VideoState::Failed(_))), "{:?}", j.rows.iter().map(|r| &r.state).collect::<Vec<_>>());
    // And what is on disk says the same.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_settled(on_disk(&w).unwrap().jobs.iter().find(|x| x.id == id).unwrap());
}

/// A refusal of the share itself is said to be the share's, with the system's
/// own words: not yt-dlp's (a flattened string once read "yt-dlp: Permission
/// denied" on the page).
/// yt-dlp failing with nothing on stderr must not read "yt-dlp: " and then
/// nothing on the page.
#[tokio::test]
async fn a_tool_that_fails_without_a_word_has_its_own_reason() {
    let w = world_with("exit 1", None);
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    let VideoState::Failed(message) = &j.rows[0].state else { panic!("{:?}", j.rows[0].state) };
    let c = crate::reason::coded(message);
    assert_eq!((c.code, c.detail.as_deref()), ("exited", Some("exit status: 1")), "{message}");
}

#[tokio::test]
async fn a_share_that_refuses_a_write_says_so_with_the_share_error_code() {
    let w = world_full("", None, "playlist.json", false);
    // The playlist's folder cannot be made: a file already has its name.
    std::fs::write(w.out.join("Popular Music Videos"), "in the way").unwrap();
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    assert!(!j.rows.is_empty());
    for row in &j.rows {
        let VideoState::Failed(message) = &row.state else { panic!("{:?}", row.state) };
        let c = crate::reason::coded(message);
        assert_eq!(c.code, "io", "{message}");
        assert!(c.detail.is_some_and(|d| !d.is_empty()));
    }
}

#[tokio::test]
async fn a_target_that_cannot_be_resolved_for_an_io_reason_fails_the_job_as_the_shares() {
    let refuse: Resolver =
        Arc::new(|_| Box::pin(async { Err(TargetError::Io("Permission denied (os error 13)".into())) }));
    let w = world_with("", Some(refuse));
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    let JobState::Failed(message) = &j.state else { panic!("{:?}", j.state) };
    assert_eq!(crate::reason::coded(message).code, "io");
}

#[tokio::test]
async fn a_failed_preset_is_on_disk_when_it_happens_not_only_at_the_end() {
    // Preset 2 (a playlist with nothing cached) fails at once; preset 1's
    // probe then takes a second, during which nothing else is persisted.
    let w = world_full("sleep 1", None, "MISSING", true);
    let id = w.jobs.enqueue(vec![2, 1], target());
    let started = Instant::now();
    let seen = loop {
        if let Some(v) = on_disk(&w)
            && let Some(j) = v.jobs.iter().find(|j| j.id == id)
            && j.rows.iter().any(|r| matches!(r.state, VideoState::Failed(_)))
        {
            break j.clone();
        }
        assert!(started.elapsed() < Duration::from_secs(10), "never persisted");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(seen.state, JobState::Running, "persisted while the job still runs");
    assert_eq!(seen.rows.len(), 1);
    finished(&w, id).await;
}

#[tokio::test]
async fn a_running_job_holds_the_tool_gate_and_a_waiting_update_holds_the_next_job() {
    let w = world("sleep 0.5");
    let gate = w.jobs.tool_gate();
    let a = w.jobs.enqueue(vec![1], target());
    until(&w, a, "started", |j| j.state == JobState::Running).await;
    assert!(gate.try_write().is_err(), "an update could swap yt-dlp under the running job");
    finished(&w, a).await;
    // An update holding the gate: the next job waits for it.
    let update = gate.clone().write_owned().await;
    let b = w.jobs.enqueue(vec![1], target());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(job(&w, b).state, JobState::Queued, "a job started under an update");
    drop(update);
    assert_eq!(finished(&w, b).await.state, JobState::Done);
}

/// A resolver that holds for its first `ok` calls, then says the share is
/// no longer mounted; and the folder it hands out while it holds.
fn share_goes_after(ok: usize) -> (tempfile::TempDir, Resolver) {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().to_path_buf();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let resolver: Resolver = Arc::new(move |_| {
        let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let r = Resolved { base: out.clone(), dir: out.clone() };
        Box::pin(async move { if n < ok { Ok(r) } else { Err(TargetError::NotMounted) } })
    });
    (dir, resolver)
}

/// The share goes away mid-job: the video whose write failed says why in
/// the share's words, the rest of its playlist is skipped with that reason,
/// the presets after it are not tried, and the job is failed with it — not
/// "done" after resolving every remaining video to fail its write.
#[tokio::test]
async fn a_share_that_goes_away_fails_the_job_at_the_first_failed_video() {
    let (_out, resolver) = share_goes_after(1);
    let w = world_full("exit 1", Some(resolver), "playlist.json", false);
    let id = w.jobs.enqueue(vec![1, 2], target());
    let j = finished(&w, id).await;
    let gone = TargetError::NotMounted.to_string();
    assert_eq!(j.state, JobState::Failed(gone.clone()));
    assert!(j.rows.len() > 2, "{j:?}");
    assert_eq!(j.rows[0].state, VideoState::Failed(gone.clone()));
    assert!(j.rows[1..].iter().all(|r| r.preset == 1 && r.state == VideoState::Skipped(gone.clone())), "{j:?}");
    assert_eq!(std::fs::read_to_string(&w.log).unwrap().lines().count(), 1, "one write tried, not one per video");
}

/// The share is only asked about after a failure: written videos are done,
/// whatever the share would answer by then.
#[tokio::test]
async fn the_share_is_not_asked_about_after_a_video_that_went_through() {
    let (_out, resolver) = share_goes_after(1);
    let w = world_full("", Some(resolver), "playlist.json", false);
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    assert_eq!(j.state, JobState::Done);
    assert!(j.rows.len() > 2 && j.rows.iter().all(|r| r.state == VideoState::Done), "{j:?}");
}

/// A video that fails while the share holds is that video's failure: the
/// job goes on.
#[tokio::test]
async fn a_failed_video_on_a_share_that_holds_does_not_stop_the_job() {
    let (_out, resolver) = share_goes_after(usize::MAX);
    let w = world_full("exit 1", Some(resolver), "playlist.json", false);
    let id = w.jobs.enqueue(vec![1], target());
    let j = finished(&w, id).await;
    assert_eq!(j.state, JobState::Done);
    assert!(j.rows.len() > 2 && j.rows.iter().all(|r| matches!(r.state, VideoState::Failed(_))), "{j:?}");
    assert_eq!(std::fs::read_to_string(&w.log).unwrap().lines().count(), j.rows.len());
}
