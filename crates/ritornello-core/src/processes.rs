//! The Ritornello process tree and its memory footprint, behind the `(?)` of
//! the System tab's Memory card.
//!
//! Same shape as `system.rs`: pure parsers and a pure selection carry the
//! unit tests, and the one function that touches the filesystem takes the
//! `/proc` root as a parameter so the tests can hand it a fake one. The reads
//! are synchronous `std::fs` for the reason given at the top of `system.rs` —
//! procfs files are produced by the kernel on read.
//!
//! **Why a route of its own rather than a field of `/api/system`.** That
//! payload is polled every 1 to 30 s by every open tab; this list is looked
//! at when someone opens the popin. Scanning every `/proc` entry on each poll
//! to serve something nobody is reading would be pure waste.
//!
//! **Why a scan rather than a registry of the pids we spawned.** The core
//! loses track of a plugin restarted by hand: it is no longer its parent, and
//! will never see its exit code (see `HotPlugChildren::unreachable_tx` in
//! `main.rs`, which exists for that very reason). A registry would also need
//! purging on every death, and `kill_triggers` already taught this codebase
//! that three purge sites was one too many — "re-read the state rather than
//! keep a registry to purge on every transition". The scan also *shows* an
//! orphan or a duplicate, which is precisely what a diagnostic panel is for.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Basename prefix that marks one of our own binaries: `ritornello-core`,
/// `ritornello-plugin-radio`, …
///
/// Only used to catch what the tree walk misses — a plugin that is no longer
/// the core's child. Matched on the basename of `argv[0]` and **not** on
/// `/proc/<pid>/comm`, which the kernel truncates to 15 characters:
/// `ritornello-plugin-generic-input` shows up there as `ritornello-plug`,
/// enough to recognise but useless to name.
const OWN_PREFIX: &str = "ritornello";

/// One line of the list, as the page renders it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessEntry {
    pub pid: u32,
    /// Basename of `argv[0]`, falling back to the (truncated) `comm` when
    /// `cmdline` is empty — which is what a zombie looks like.
    pub name: String,
    pub rss_kb: u64,
    /// Share of `MemTotal`. `None` when `MemTotal` is unreadable, so the page
    /// shows a dash rather than a percentage of nothing.
    pub percent: Option<f32>,
    /// Seconds since this process started. `None` when `/proc/uptime` is
    /// unreadable. This is the column that answers "is this ephemeral child
    /// still around by mistake?" — without it, the line cannot say so.
    pub age_s: Option<u64>,
}

/// What `GET /api/system/processes` reports.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessList {
    /// Heaviest first. Never `null`: an empty list is a legitimate answer on
    /// a machine without `/proc`, and one shape is easier to render than two.
    pub processes: Vec<ProcessEntry>,
}

/// A process as the scan sees it, before selection.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub rss_pages: u64,
    pub starttime_ticks: u64,
}

/// The four fields of `/proc/<pid>/stat` this module needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub comm: String,
    pub ppid: u32,
    pub rss_pages: u64,
    pub starttime_ticks: u64,
}

/// `/proc/<pid>/stat`: "1234 (some name) S 1 1234 …".
///
/// Field 2 is the command in parentheses, and it is the whole difficulty of
/// this format: it is written raw, so it can contain spaces *and*
/// parentheses. Splitting on whitespace from the start therefore shifts every
/// following field for any process whose name has a space in it. Hence
/// `rfind(')')` — the kernel writes exactly one such field, so the last
/// closing parenthesis of the line ends it.
///
/// Past it, counting from zero: 1 is `ppid`, 19 `starttime`, 21 `rss` (fields
/// 4, 22 and 24 of the whole line). `rss` is in pages, `starttime` in clock
/// ticks since boot.
pub fn parse_stat(raw: &str) -> Option<Stat> {
    let open = raw.find('(')?;
    let close = raw.rfind(')')?;
    let comm = raw.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = raw.get(close + 1..)?.split_whitespace().collect();
    Some(Stat {
        comm,
        ppid: rest.get(1)?.parse().ok()?,
        starttime_ticks: rest.get(19)?.parse().ok()?,
        rss_pages: rest.get(21)?.parse().ok()?,
    })
}

/// `/proc/<pid>/cmdline`: NUL-separated arguments. Only `argv[0]` interests
/// us, and only its basename: the manifest holds absolute paths, which would
/// make every line of the table as wide as an installation prefix.
///
/// `None` on an empty file: that is a zombie or a kernel thread, and the
/// caller falls back to `comm`.
pub fn command_name(raw: &str) -> Option<String> {
    let argv0 = raw.split('\0').next().filter(|s| !s.is_empty())?;
    // `rsplit('/')` and not `Path::file_name`: `argv[0]` is whatever the
    // parent chose to write there, not necessarily a valid path, and a
    // trailing slash must not swallow the name.
    Some(argv0.rsplit('/').next().unwrap_or(argv0).to_string())
}

/// Which pids belong to Ritornello.
///
/// Two rules, unioned:
///
/// 1. `root` and everything descended from it, transitively — the plugins,
///    mpv, and the short-lived helpers (`smbclient`, `vcgencmd`) the core or
///    a plugin spawns. Showing those is the point: an "ephemeral" child still
///    alive is a defect, and it has nowhere else to become visible.
/// 2. Any of our own binaries that rule 1 misses, plus *their* descendants —
///    a plugin restarted by hand is re-parented away from the core but is
///    still ours, and it may itself have spawned a helper.
///
/// The `visited` set is not decoration: a pid graph read entry by entry is
/// not a snapshot, and a pid recycled between two reads can close a loop that
/// never existed at any single instant. Without it that loop would hang the
/// handler.
pub fn select(candidates: &[Candidate], root: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for c in candidates {
        children.entry(c.ppid).or_default().push(c.pid);
    }
    let mut queue: Vec<u32> = candidates
        .iter()
        .filter(|c| c.pid == root || c.name.starts_with(OWN_PREFIX))
        .map(|c| c.pid)
        .collect();
    let mut visited: HashSet<u32> = HashSet::new();
    while let Some(pid) = queue.pop() {
        if !visited.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids);
        }
    }
    visited
}

/// Share of the total, as a percentage. `None` rather than a division by zero
/// when `MemTotal` is unknown.
pub fn percent(rss_kb: u64, total_kb: Option<u64>) -> Option<f32> {
    let total = total_kb.filter(|t| *t > 0)?;
    Some(rss_kb as f32 * 100.0 / total as f32)
}

/// Seconds since a process started, from its `starttime` and the machine's
/// uptime.
///
/// Saturates at zero instead of going negative: `uptime` and `stat` are two
/// separate reads, and a process that started between them is younger than
/// the uptime we hold.
pub fn age_s(starttime_ticks: u64, uptime_s: f64, ticks_per_s: u64) -> Option<u64> {
    if ticks_per_s == 0 {
        return None;
    }
    let started_at = starttime_ticks as f64 / ticks_per_s as f64;
    Some((uptime_s - started_at).max(0.0) as u64)
}

/// `/proc/uptime`: "84213.42 512345.10". Only the first number matters here.
pub fn parse_uptime_secs(raw: &str) -> Option<f64> {
    raw.split_whitespace().next()?.parse().ok()
}

/// Reads every process of a `/proc` tree.
///
/// An entry that disappears mid-scan is skipped, not reported: a process
/// exiting while we walk the directory is ordinary, not an error worth
/// failing the whole list over.
pub fn scan(proc_root: &Path) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let dir = entry.path();
        let Some(stat) = std::fs::read_to_string(dir.join("stat")).ok().as_deref().and_then(parse_stat)
        else {
            continue;
        };
        let name = std::fs::read_to_string(dir.join("cmdline"))
            .ok()
            .as_deref()
            .and_then(command_name)
            .unwrap_or(stat.comm);
        out.push(Candidate {
            pid,
            ppid: stat.ppid,
            name,
            rss_pages: stat.rss_pages,
            starttime_ticks: stat.starttime_ticks,
        });
    }
    out
}

/// The whole list, ready to serve: scan, select, convert to kilobytes and
/// percentages, heaviest first.
///
/// Ties break on the pid so the order is total, and therefore stable between
/// two openings of the popin — two plugins idling at the same footprint were
/// otherwise free to swap places on every refresh.
pub fn collect(
    proc_root: &Path,
    root: u32,
    total_kb: Option<u64>,
    page_size_kb: u64,
    ticks_per_s: u64,
) -> Vec<ProcessEntry> {
    let candidates = scan(proc_root);
    let selected = select(&candidates, root);
    let uptime = std::fs::read_to_string(proc_root.join("uptime"))
        .ok()
        .as_deref()
        .and_then(parse_uptime_secs);
    let mut out: Vec<ProcessEntry> = candidates
        .into_iter()
        .filter(|c| selected.contains(&c.pid))
        .map(|c| {
            let rss_kb = c.rss_pages.saturating_mul(page_size_kb);
            ProcessEntry {
                pid: c.pid,
                name: c.name,
                rss_kb,
                percent: percent(rss_kb, total_kb),
                age_s: uptime.and_then(|u| age_s(c.starttime_ticks, u, ticks_per_s)),
            }
        })
        .collect();
    out.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb).then(a.pid.cmp(&b.pid)));
    out
}

/// Page size in kilobytes, as `libc` reports it.
fn page_size_kb() -> u64 {
    // SAFETY: `sysconf` reads no memory of ours and only returns a long.
    let bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(bytes).unwrap_or(4096) / 1024
}

/// Clock ticks per second, as `libc` reports it.
fn ticks_per_s() -> u64 {
    // SAFETY: same as `page_size_kb`.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).unwrap_or(100)
}

/// `GET /api/system/processes`.
///
/// Takes no state: the answer is read entirely from `/proc`, and the root of
/// the tree is this very process.
pub async fn processes_json() -> axum::Json<ProcessList> {
    let total_kb = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .as_deref()
        .and_then(crate::system::parse_meminfo)
        .map(|u| u.total_kb);
    axum::Json(ProcessList {
        processes: collect(
            Path::new("/proc"),
            std::process::id(),
            total_kb,
            page_size_kb(),
            ticks_per_s(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(pid: u32, ppid: u32, name: &str) -> Candidate {
        Candidate { pid, ppid, name: name.to_string(), rss_pages: 0, starttime_ticks: 0 }
    }

    #[test]
    fn stat_reads_ppid_starttime_and_rss() {
        // Real shape, trimmed to the fields that matter.
        let raw = "1234 (ritornello-plug) S 42 1234 1234 0 -1 4194560 100 0 0 0 \
                   5 6 0 0 20 0 12 0 3046214 5009408 871 18446744073709551615\n";
        assert_eq!(
            parse_stat(raw),
            Some(Stat {
                comm: "ritornello-plug".to_string(),
                ppid: 42,
                starttime_ticks: 3_046_214,
                rss_pages: 871,
            })
        );
    }

    #[test]
    fn stat_survives_a_command_containing_spaces_and_parentheses() {
        // The reason `rfind(')')` exists: splitting from the start would put
        // `Files)` where `ppid` belongs, and every later field would shift.
        let raw = "7 ((My Files) reader) S 42 7 7 0 -1 0 0 0 0 0 \
                   0 0 0 0 20 0 1 0 900 0 250 0\n";
        let stat = parse_stat(raw).expect("parsed");
        assert_eq!(stat.comm, "(My Files) reader");
        assert_eq!(stat.ppid, 42);
        assert_eq!(stat.starttime_ticks, 900);
        assert_eq!(stat.rss_pages, 250);
    }

    #[test]
    fn stat_truncated_or_malformed_is_no_measurement() {
        assert_eq!(parse_stat(""), None);
        assert_eq!(parse_stat("1234 no parens here S 42"), None);
        // Stops before field 24: better nothing than a wrong footprint.
        assert_eq!(parse_stat("1234 (x) S 42 1234\n"), None);
        assert_eq!(parse_stat("1234 (x) S notanumber 1234 1234 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 900 0 250 0"), None);
    }

    #[test]
    fn command_name_keeps_only_the_basename_of_argv0() {
        assert_eq!(
            command_name("/usr/local/bin/ritornello-plugin-radio\0--socket\0/run/x\0"),
            Some("ritornello-plugin-radio".to_string())
        );
        assert_eq!(command_name("mpv\0--idle\0"), Some("mpv".to_string()));
        // Empty file: a zombie or a kernel thread, the caller falls back to
        // `comm`.
        assert_eq!(command_name(""), None);
        assert_eq!(command_name("\0\0"), None);
    }

    #[test]
    fn select_takes_the_whole_subtree_of_the_core() {
        // 100 is the core; the plugin and mpv are its children; smbclient is
        // a grandchild through the plugin. `sshd` shares no ancestry and must
        // stay out — that is the whole request: no unrelated system process.
        let procs = [
            candidate(1, 0, "systemd"),
            candidate(100, 1, "ritornello-core"),
            candidate(101, 100, "ritornello-plugin-files"),
            candidate(102, 100, "mpv"),
            candidate(103, 101, "smbclient"),
            candidate(200, 1, "sshd"),
        ];
        let mut got: Vec<u32> = select(&procs, 100).into_iter().collect();
        got.sort_unstable();
        assert_eq!(got, vec![100, 101, 102, 103]);
    }

    #[test]
    fn select_catches_a_plugin_restarted_by_hand_and_its_own_children() {
        // 300 was re-parented to init when it was restarted outside the
        // core, so no tree walk from 100 can reach it. Its name is what
        // brings it back, and 301 comes along: a helper of an orphan is
        // still ours.
        let procs = [
            candidate(1, 0, "systemd"),
            candidate(100, 1, "ritornello-core"),
            candidate(300, 1, "ritornello-plugin-mpd"),
            candidate(301, 300, "smbclient"),
            candidate(200, 1, "sshd"),
        ];
        let mut got: Vec<u32> = select(&procs, 100).into_iter().collect();
        got.sort_unstable();
        assert_eq!(got, vec![100, 300, 301]);
    }

    #[test]
    fn select_does_not_hang_on_a_cycle() {
        // A pid graph read entry by entry is not a snapshot: a recycled pid
        // can close a loop that existed at no single instant. Without the
        // `visited` set this test never returns.
        let procs = [
            candidate(100, 1, "ritornello-core"),
            candidate(101, 102, "child-a"),
            candidate(102, 101, "child-b"),
            candidate(103, 100, "ritornello-plugin-cd"),
        ];
        let mut got: Vec<u32> = select(&procs, 100).into_iter().collect();
        got.sort_unstable();
        // 101/102 point at each other and hang off nothing rooted, so they
        // are not ours; the cycle merely must not trap the walk.
        assert_eq!(got, vec![100, 103]);
    }

    #[test]
    fn select_without_the_root_in_the_list_still_finds_our_binaries() {
        // Defensive: `/proc/self` vanishing from a scan is impossible in
        // production, but the selection must not depend on it.
        let procs = [candidate(1, 0, "systemd"), candidate(300, 1, "ritornello-plugin-radio")];
        let got: Vec<u32> = select(&procs, 999).into_iter().collect();
        assert_eq!(got, vec![300]);
    }

    #[test]
    fn percent_of_an_unknown_total_is_no_percentage() {
        assert_eq!(percent(1024, Some(2048)), Some(50.0));
        assert_eq!(percent(1024, None), None);
        // Not an infinity, not a NaN: `MemTotal: 0` is nonsense, and the
        // page must show a dash rather than serialise `null`-hostile JSON.
        assert_eq!(percent(1024, Some(0)), None);
    }

    #[test]
    fn age_never_goes_negative() {
        assert_eq!(age_s(6000, 100.0, 100), Some(40));
        // Read between two clocks: `stat` says the process started after the
        // uptime we hold. Zero, not an underflow.
        assert_eq!(age_s(20_000, 100.0, 100), Some(0));
        assert_eq!(age_s(6000, 100.0, 0), None);
    }

    #[test]
    fn uptime_reads_the_first_number() {
        assert_eq!(parse_uptime_secs("84213.42 512345.10\n"), Some(84_213.42));
        assert_eq!(parse_uptime_secs(""), None);
        assert_eq!(parse_uptime_secs("chatty\n"), None);
    }

    /// Writes a fake `/proc/<pid>` with the two files the scan reads.
    fn write_proc(root: &Path, pid: u32, ppid: u32, argv0: &str, rss_pages: u64, start: u64) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let comm = argv0.rsplit('/').next().unwrap();
        std::fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) S {ppid} {pid} {pid} 0 -1 0 0 0 0 0 \
                 0 0 0 0 20 0 1 0 {start} 0 {rss_pages} 0\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("cmdline"), format!("{argv0}\0--flag\0")).unwrap();
    }

    #[test]
    fn collect_serves_the_tree_heaviest_first_with_percentages_and_ages() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("uptime"), "1000.00 4000.00\n").unwrap();
        // Not a pid: `/proc` is full of these (`self`, `meminfo`, `net`…)
        // and they must be skipped without a word.
        std::fs::create_dir_all(root.join("net")).unwrap();
        std::fs::write(root.join("meminfo"), "MemTotal: 1 kB\n").unwrap();
        write_proc(root, 1, 0, "/sbin/init", 500, 0);
        write_proc(root, 100, 1, "/opt/ritornello/ritornello-core", 1000, 10_000);
        write_proc(root, 102, 100, "/usr/bin/mpv", 5000, 50_000);
        write_proc(root, 101, 100, "/opt/ritornello/ritornello-plugin-files", 1000, 20_000);
        write_proc(root, 200, 1, "/usr/sbin/sshd", 9999, 0);

        // 1 page = 4 kB, MemTotal = 100 000 kB.
        let got = collect(root, 100, Some(100_000), 4, 100);

        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        // mpv first (20 MB), then the two 4 MB processes ordered by pid, and
        // neither init nor sshd anywhere.
        assert_eq!(names, vec!["mpv", "ritornello-core", "ritornello-plugin-files"]);
        assert_eq!(got[0].rss_kb, 20_000);
        assert_eq!(got[0].percent, Some(20.0));
        // Started at 50 000 ticks = 500 s, uptime 1000 s.
        assert_eq!(got[0].age_s, Some(500));
        assert_eq!(got[1].age_s, Some(900));
    }

    #[test]
    fn collect_without_a_proc_tree_is_an_empty_list_not_a_failure() {
        // A container without `/proc` mounted, or a non-Linux host: the page
        // must render, empty. `system.rs` makes the same choice for every
        // metric it cannot read.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(collect(&dir.path().join("nope"), 1, Some(1000), 4, 100), Vec::new());
    }

    #[test]
    fn collect_falls_back_to_comm_when_cmdline_is_empty() {
        // What a zombie looks like. The name is then the kernel's truncated
        // one, which is still better than an empty cell.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("uptime"), "10.0 10.0\n").unwrap();
        let pid_dir = root.join("100");
        std::fs::create_dir_all(&pid_dir).unwrap();
        std::fs::write(
            pid_dir.join("stat"),
            "100 (ritornello-plug) Z 1 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0 0\n",
        )
        .unwrap();
        std::fs::write(pid_dir.join("cmdline"), "").unwrap();
        let got = collect(root, 100, Some(1000), 4, 100);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "ritornello-plug");
    }

    #[test]
    fn the_real_proc_holds_at_least_this_process() {
        // Smoke test, like `system.rs`'s: the suite runs on Linux, so the
        // scan must at minimum find the test binary itself and report a
        // non-zero footprint for it.
        let me = std::process::id();
        let got = collect(Path::new("/proc"), me, Some(1_000_000), page_size_kb(), ticks_per_s());
        let mine = got.iter().find(|e| e.pid == me).expect("this process is in its own tree");
        assert!(mine.rss_kb > 0, "a live process has a resident footprint");
    }
}
