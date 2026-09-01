//! OS metrics for the System tab, and the power actions behind it.
//!
//! Same shape as `audio_output.rs`: pure parsers carrying the unit tests,
//! thin I/O wrappers that are not unit-tested.
//!
//! The reads below are synchronous `std::fs` calls even though the caller
//! is an async handler, and that is deliberate: procfs and sysfs files are
//! produced by the kernel on read — they do not wait on a device the way a
//! disk read does — and `statvfs` reads a cached superblock. Neither
//! warrants `spawn_blocking`.
//!
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ritornello_i18n::Catalog;
use serde::Serialize;
use std::sync::Arc;

/// Available/total pair in kilobytes — the unit `/proc/meminfo` already
/// uses, kept for the filesystem too so the SPA formats both the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub total_kb: u64,
    pub available_kb: u64,
}

/// Everything `GET /api/system` reports.
///
/// Every metric is an `Option` and serializes as `null` when the machine
/// does not expose it: an x86 box has no `rpi_volt` sensor, WSL has no
/// thermal zone, a VM often has no cpufreq. All three must render a page,
/// not an error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metrics {
    pub temperature_c: Option<f32>,
    pub cpu_mhz: Option<u32>,
    pub load: Option<[f32; 3]>,
    pub cpus: Option<usize>,
    pub memory: Option<Usage>,
    pub disk: Option<Usage>,
    pub under_voltage: Option<bool>,
    /// Whether an under-voltage episode has happened since boot, even if the
    /// supply is fine right now. See `under_voltage_since_boot()` below for
    /// why this needs a source distinct from `under_voltage`.
    pub under_voltage_since_boot: Option<bool>,
    pub uptime_s: Option<u64>,
    pub service_uptime_s: u64,
    pub hostname: Option<String>,
    pub ip: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub version: String,
    pub can_power_off: bool,
    pub can_reboot: bool,
    /// Did logind answer the capability probe at all?
    ///
    /// Distinct from the two booleans above, and the page needs both: logind
    /// answering "no" means the polkit rule is missing, whereas logind not
    /// answering means logind itself is unavailable — a masked or unloadable
    /// `systemd-logind`, seen on a DietPi image as `Call failed: Unit
    /// dbus-org.freedesktop.login1.service failed to load properly`. Both
    /// leave the two buttons disabled, but they are not fixed by the same
    /// thing, and a single sentence naming polkit sends the second case
    /// looking for a rule that is already there.
    pub logind_reachable: bool,
    /// Cumulative CPU jiffies since boot: `total` includes every field of
    /// `/proc/stat`'s aggregate line, `idle` is `idle + iowait`. A
    /// percentage cannot be read, only computed from two readings taken
    /// apart in time — the core exposes the raw counters rather than
    /// differencing them itself, because that would need shared state to
    /// remember the previous reading, and two browser tabs polling out of
    /// phase would corrupt each other's delta. The page differences its own
    /// successive polls instead.
    pub cpu_total_jiffies: Option<u64>,
    pub cpu_idle_jiffies: Option<u64>,
}

/// Called to restart the service. A field rather than a direct
/// `std::process::exit(0)` **so the route can be tested**: a test that
/// really exited would kill the test binary.
pub type RestartHook = Arc<dyn Fn() + Send + Sync>;

/// Sends `SIGTERM` to `pid`, if there is one. No-op on `None`.
///
/// Exists as a named function so the service restart's cleanup can be tested
/// on a real process. The bug it answers was a guarantee nobody had checked:
/// mpv is spawned with `kill_on_drop(true)`, but the restart hook ends in
/// `std::process::exit`, which does not unwind and therefore runs no `Drop` —
/// so mpv outlived the core and kept playing, holding the audio device the
/// restarted core wanted back. Under systemd nothing showed, the unit's
/// remaining cgroup processes being killed before the restart; in a
/// development run, with no supervisor, the orphan stayed.
///
/// `SIGTERM` rather than `SIGKILL`: mpv closes its stream and its audio
/// device on the way out.
pub fn terminate_process(pid: Option<u32>) {
    let Some(pid) = pid.and_then(|p| libc::pid_t::try_from(p).ok()) else {
        return;
    };
    // SAFETY: `kill` only reads the pid we hand it and shares no memory with
    // us. A pid that is already gone yields `ESRCH`, which changes nothing.
    unsafe { libc::kill(pid, libc::SIGTERM) };
}

/// Process-lifetime facts the System tab's endpoints need.
///
/// No `#[derive(Clone)]`: nothing clones a `SystemInfo` directly, only the
/// `Arc<SystemInfo>` that wraps the single shared instance (`status::AppState`),
/// and `Arc::clone` needs no bound on `T`. Deriving it would also be wrong once
/// `under_voltage_latched` exists below — an `AtomicBool` cannot implement
/// `Clone` without silently deciding whether a clone shares the flag or starts
/// its own, and nothing here needs that decision made.
pub struct SystemInfo {
    pub started: std::time::Instant,
    pub can_power_off: bool,
    pub can_reboot: bool,
    /// See `Metrics::logind_reachable`: whether the startup probe got an
    /// answer, whatever the answer was.
    pub logind_reachable: bool,
    /// Command used for the OS power actions. A field rather than a
    /// constant **so the destructive routes can be tested**: a test that
    /// really ran `systemctl poweroff` would shut down the machine running
    /// the suite. Tests point it at `/bin/true` and `/bin/false` and still
    /// exercise the real spawn/await/exit-code path.
    pub systemctl: String,
    /// Command used to read the firmware's sticky under-voltage flag
    /// (`vcgencmd get_throttled`). A field for the same reason `systemctl`
    /// is: tests point it at a stub script instead of a real `vcgencmd`.
    pub vcgencmd: String,
    /// Latches to `true` the first time the sticky flag is seen set, and
    /// never resets: the firmware itself only clears it at reboot, so once
    /// this process has observed it, spawning `vcgencmd` again can only ever
    /// confirm the same answer. See `under_voltage_since_boot()`.
    ///
    /// `AtomicBool` rather than a plain `bool` behind a `Mutex`: `SystemInfo`
    /// lives behind an `Arc` shared by every poll, so recording the answer
    /// needs interior mutability (`&self`, not `&mut self`), and a single
    /// flag with no invariant linking it to another field needs nothing
    /// heavier than atomic load/store — no lock, no poisoning to handle.
    // `pub(crate)`, not private: struct-update syntax (`..Default::default()`,
    // used both in `main.rs` and throughout this file's tests) requires every
    // field to be visible at the construction site, even the ones filled from
    // the base — a documented quirk of the syntax, not a relaxation of intent.
    // Nothing outside the crate constructs a `SystemInfo` at all.
    pub(crate) under_voltage_latched: std::sync::atomic::AtomicBool,
    /// Delay between the `202` and the process exit, so the response
    /// reaches the browser before the socket dies.
    pub restart_delay: std::time::Duration,
    pub restart: RestartHook,
}

impl Default for SystemInfo {
    /// Capabilities default to `false`: not knowing means offering nothing.
    /// `main` replaces them with what logind answers.
    fn default() -> Self {
        Self {
            started: std::time::Instant::now(),
            can_power_off: false,
            can_reboot: false,
            // Unreachable by default, like the capabilities: a development
            // machine has no logind, and that is what the page should say.
            logind_reachable: false,
            systemctl: "systemctl".to_string(),
            vcgencmd: "vcgencmd".to_string(),
            under_voltage_latched: std::sync::atomic::AtomicBool::new(false),
            restart_delay: std::time::Duration::from_millis(300),
            restart: Arc::new(|| std::process::exit(0)),
        }
    }
}

/// `/sys/class/thermal/thermal_zone0/temp` holds millidegrees Celsius:
/// "47800" is 47.8 °C. One decimal is already more precision than the
/// sensor deserves.
pub fn parse_temperature(raw: &str) -> Option<f32> {
    let millis: i32 = raw.trim().parse().ok()?;
    Some((millis as f32 / 100.0).round() / 10.0)
}

/// `scaling_cur_freq` is in kHz: "900000" is 900 MHz.
pub fn parse_khz_to_mhz(raw: &str) -> Option<u32> {
    let khz: u64 = raw.trim().parse().ok()?;
    u32::try_from(khz / 1000).ok()
}

/// `/proc/loadavg`: "0.12 0.15 0.09 1/234 5678" — the three averages, then
/// task counts and the last pid, which are of no interest here.
pub fn parse_loadavg(raw: &str) -> Option<[f32; 3]> {
    let mut fields = raw.split_whitespace();
    let one = fields.next()?.parse().ok()?;
    let five = fields.next()?.parse().ok()?;
    let fifteen = fields.next()?.parse().ok()?;
    Some([one, five, fifteen])
}

/// `/proc/meminfo`: "MemTotal:         948000 kB".
///
/// `MemAvailable` and not `MemFree`: free memory alone counts caches as
/// used and reads as alarmingly low on a healthy Linux, while
/// `MemAvailable` is the kernel's own estimate of what a new workload could
/// take. A kernel too old to publish it reports no measurement rather than
/// a misleading one.
pub fn parse_meminfo(raw: &str) -> Option<Usage> {
    let field = |name: &str| -> Option<u64> {
        raw.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    };
    Some(Usage { total_kb: field("MemTotal")?, available_kb: field("MemAvailable")? })
}

/// `/proc/uptime`: "84213.42 512345.10" — seconds since boot, then idle
/// time summed over all cores.
pub fn parse_uptime(raw: &str) -> Option<u64> {
    let seconds: f64 = raw.split_whitespace().next()?.parse().ok()?;
    Some(seconds as u64)
}

/// `/etc/os-release`: `PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"`.
pub fn parse_os_release(raw: &str) -> Option<String> {
    let value = raw.lines().find_map(|l| l.strip_prefix("PRETTY_NAME="))?;
    Some(value.trim().trim_matches('"').to_string())
}

/// hwmon alarm files hold "1" while the alarm is raised, "0" otherwise.
pub fn parse_alarm(raw: &str) -> Option<bool> {
    match raw.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// `vcgencmd get_throttled`: "throttled=0x50000\n" — a bitmask whose low
/// four bits (0-3) are the *current* state, already covered by
/// `under_voltage()`'s hwmon alarm, and whose bits 16-19 are sticky: they
/// latch when the matching low bit has fired even once since boot, and the
/// firmware only clears them at the next reboot. Only bit 16, "under-voltage
/// has occurred", is read here. Bit 18 ("throttling has occurred") is not
/// exposed as its own field: on a Raspberry Pi it is, in practice, always
/// the consequence of bit 16, and a second field for the same underlying
/// event would read as two separate problems instead of one.
pub fn parse_throttled(raw: &str) -> Option<bool> {
    let value = raw.trim().strip_prefix("throttled=0x")?;
    let mask = u32::from_str_radix(value, 16).ok()?;
    Some(mask & (1 << 16) != 0)
}

/// `/proc/stat`'s first line: "cpu  123456 789 34567 9876543 1234 0 567 0 0
/// 0" (user, nice, system, idle, iowait, irq, softirq, steal, guest,
/// guest_nice). Returns `(total, idle)` jiffy counters, cumulative since
/// boot.
///
/// `total` sums **every** field on the line, not just the ten named above —
/// a kernel that adds a column keeps being counted correctly rather than
/// silently undercounted — then subtracts `guest` and `guest_nice` (fields 8
/// and 9, absent on kernels old enough not to report them, treated as 0
/// there): the kernel already folds guest time into `user` and guest_nice
/// into `nice`, so summing every column without correcting for that would
/// double-count guest time and under-report utilisation on a virtualised
/// host.
///
/// `idle` is `idle + iowait`, not `idle` alone: `iowait` is time spent
/// waiting on a disk, not doing work, and `top` treats it the same way.
/// Counting it as busy would show a disk-bound appliance as CPU-saturated.
///
/// Only the aggregate line is read, matched on `"cpu "` with the trailing
/// space the kernel writes (two spaces before the first number): `"cpu0"`,
/// `"cpu1"`, etc. do not match that prefix and are skipped. `None` when
/// that line is missing, holds a non-numeric field, or has fewer than four
/// fields — not enough to know `idle` and `iowait`.
pub fn parse_cpu_jiffies(raw: &str) -> Option<(u64, u64)> {
    let line = raw.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|v| v.parse::<u64>())
        .collect::<Result<_, _>>()
        .ok()?;
    if fields.len() < 4 {
        return None;
    }
    let total: u64 = fields.iter().sum();
    let guest = fields.get(8).copied().unwrap_or(0) + fields.get(9).copied().unwrap_or(0);
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some((total - guest, idle))
}

/// Reads a pseudo-file, `None` on any error. Absence is the normal case for
/// most of these paths, not an incident worth a log line.
fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The Raspberry Pi undervoltage flag, published by the `rpi_volt` driver
/// as `in0_lcrit_alarm`. The hwmon number varies with probe order, hence
/// the scan by driver `name` rather than a hardcoded `hwmon0`. `vcgencmd`
/// was rejected: it needs the `video` group and a spawned process per poll.
fn under_voltage() -> Option<bool> {
    for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        // `unwrap_or_default` and not `?`: one unreadable entry must not
        // abandon the scan of the others.
        let name = std::fs::read_to_string(entry.path().join("name")).unwrap_or_default();
        if name.trim() == "rpi_volt" {
            let alarm = std::fs::read_to_string(entry.path().join("in0_lcrit_alarm")).ok()?;
            return parse_alarm(&alarm);
        }
    }
    None
}

/// Whether an under-voltage episode has occurred since boot, from the
/// firmware's own sticky flag (see `parse_throttled`). The kernel does not
/// publish this anywhere in `/sys` or `/proc` — `find /sys -name
/// "*throttled*"` comes back empty on a real Pi, only `soc:firmware:vcio`
/// shows up — so `vcgencmd` is the only source, unlike every other reading
/// in this module.
///
/// Latched through `info.under_voltage_latched`: once this has answered
/// `true` once, it answers `true` forever without spawning `vcgencmd` again
/// — the flag cannot un-set itself before a reboot, so a second process
/// could only ever learn what this one already knows. Before that, every
/// call spawns the command again, because the answer can still change (the
/// whole point of an appliance that keeps polling while it plays).
///
/// `None` — not `Some(false)` — on anything short of a successful, parsable
/// reply: `vcgencmd` absent (not a Pi), a permission refusal (the service not
/// in the `video` group, granted by `SupplementaryGroups=` in
/// `deploy/ritornello.service` alongside `audio`, `input`, `cdrom` and `tty`),
/// or output this parser does not recognise. None of these are worth a log
/// line; a machine that is not a Raspberry Pi is the ordinary case, not an
/// incident, the same reasoning `read()` already applies to missing
/// pseudo-files.
fn under_voltage_since_boot(info: &SystemInfo) -> Option<bool> {
    use std::sync::atomic::Ordering;
    if info.under_voltage_latched.load(Ordering::Relaxed) {
        return Some(true);
    }
    let output = std::process::Command::new(&info.vcgencmd).arg("get_throttled").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let seen = parse_throttled(&String::from_utf8_lossy(&output.stdout))?;
    if seen {
        info.under_voltage_latched.store(true, Ordering::Relaxed);
    }
    Some(seen)
}

/// Root filesystem usage through `statvfs`, in kilobytes.
///
/// `libc` rather than parsing `df`: `df` means one process per poll and can
/// hang on an unrelated stale mount, while `statvfs` on `/` reads a cached
/// superblock.
///
/// `f_bavail` (blocks available to an unprivileged user) and not `f_bfree`:
/// the blocks a filesystem reserves for root are not space this device can
/// use.
///
/// `u64::from` and not `as u64`: these field widths differ between
/// architectures (32-bit on armv7, already `u64` on x86_64), and `as u64`
/// would hit `clippy::unnecessary_cast` on the architecture where it is a
/// no-op. On x86_64 — the only architecture `cargo clippy` runs on in this
/// workshop — `u64::from` is itself that no-op, which is the *other* half of
/// the same tension: `clippy::useless_conversion` fires here instead. The
/// conversion stays because it is required on armv7; the allow documents
/// that this specific "useless" call is platform-dependent, not sloppy.
#[allow(clippy::useless_conversion)]
fn disk_usage(path: &str) -> Option<Usage> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: `statvfs` only writes into the struct we hand it, and the
    // path stays a valid NUL-terminated C string for the whole call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let block = u64::from(st.f_frsize);
    Some(Usage {
        total_kb: u64::from(st.f_blocks) * block / 1024,
        available_kb: u64::from(st.f_bavail) * block / 1024,
    })
}

/// Local address of the interface facing the default route.
///
/// The UDP socket **sends nothing**: `connect` on a datagram socket only
/// asks the kernel which local address would be used to reach that
/// destination. No internet access is needed or attempted — `8.8.8.8:53` is
/// a routable address, not a server we talk to. `None` when there is no
/// route at all.
fn ip_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:53").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// Reads everything, once, for one HTTP response.
pub fn collect(info: &SystemInfo) -> Metrics {
    // A single read of /proc/stat feeds both counters below: two separate
    // reads could straddle a tick and skew the delta the page computes
    // between polls.
    let cpu_jiffies = read("/proc/stat").as_deref().and_then(parse_cpu_jiffies);
    Metrics {
        temperature_c: read("/sys/class/thermal/thermal_zone0/temp")
            .as_deref()
            .and_then(parse_temperature),
        cpu_mhz: read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
            .as_deref()
            .and_then(parse_khz_to_mhz),
        load: read("/proc/loadavg").as_deref().and_then(parse_loadavg),
        cpus: std::thread::available_parallelism().ok().map(|n| n.get()),
        memory: read("/proc/meminfo").as_deref().and_then(parse_meminfo),
        disk: disk_usage("/"),
        under_voltage: under_voltage(),
        under_voltage_since_boot: under_voltage_since_boot(info),
        uptime_s: read("/proc/uptime").as_deref().and_then(parse_uptime),
        service_uptime_s: info.started.elapsed().as_secs(),
        hostname: read("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()),
        ip: ip_address(),
        os: read("/etc/os-release").as_deref().and_then(parse_os_release),
        kernel: read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()),
        version: env!("CARGO_PKG_VERSION").to_string(),
        can_power_off: info.can_power_off,
        can_reboot: info.can_reboot,
        logind_reachable: info.logind_reachable,
        cpu_total_jiffies: cpu_jiffies.map(|(total, _)| total),
        cpu_idle_jiffies: cpu_jiffies.map(|(_, idle)| idle),
    }
}

/// Metrics for the System tab. Read on demand, nothing cached: the page
/// polls, and everything here costs a handful of pseudo-file reads.
pub async fn system_json(State(state): State<crate::status::AppState>) -> Json<Metrics> {
    Json(collect(&state.system))
}

/// What `POST /api/system/power` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    PowerOff,
    Reboot,
    RestartService,
}

/// Unknown power action. Follows the `ValidationError` model
/// (`ritornello-plugin-radio/src/config.rs`): the user-facing text is
/// produced at the boundary via `message(&Catalog)`, `Display` provides an
/// English version for the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPowerAction;

impl UnknownPowerAction {
    pub fn message(&self, catalog: &Catalog) -> String {
        catalog.get("power_action_unknown").to_string()
    }
}

impl std::fmt::Display for UnknownPowerAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown power action")
    }
}

impl std::error::Error for UnknownPowerAction {}

/// The action arrives as a string and is validated here rather than through
/// serde's enum deserialization: an unknown value must answer with this
/// project's `422 {"error": …}` shape, whereas a serde rejection would
/// answer with axum's own plain-text 422. Same reasoning as
/// `validate_audio_device`.
pub fn parse_action(action: &str) -> Result<PowerAction, UnknownPowerAction> {
    match action {
        "poweroff" => Ok(PowerAction::PowerOff),
        "reboot" => Ok(PowerAction::Reboot),
        "restart-service" => Ok(PowerAction::RestartService),
        _ => Err(UnknownPowerAction),
    }
}

/// `systemctl` returned with a failure code and nothing on stderr. The only
/// case that goes through the catalog: when logind wrote a message
/// (neighbouring branch, untouched), that message is relayed word for
/// word — it names the missing polkit rule, which no generic sentence
/// could do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemctlFailed {
    pub code: i32,
}

impl SystemctlFailed {
    pub fn message(&self, catalog: &Catalog) -> String {
        catalog.get("systemctl_failed").replace("{code}", &self.code.to_string())
    }
}

impl std::fmt::Display for SystemctlFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "systemctl failed (exit code {})", self.code)
    }
}

impl std::error::Error for SystemctlFailed {}

/// `systemctl` could not be launched at all (missing path, permissions…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemctlUnreachable {
    pub detail: String,
}

impl SystemctlUnreachable {
    pub fn message(&self, catalog: &Catalog) -> String {
        catalog.get("systemctl_unreachable").replace("{detail}", &self.detail)
    }
}

impl std::fmt::Display for SystemctlUnreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "systemctl unreachable: {}", self.detail)
    }
}

impl std::error::Error for SystemctlUnreachable {}

/// `busctl` prints `s "yes"` for a granted action. Anything else — `"no"`,
/// `"challenge"` (interactive authentication, which a system service can
/// never satisfy), or unparseable output — means no.
pub fn parse_can(raw: &str) -> bool {
    raw.contains("\"yes\"")
}

/// Asks logind, once at startup, whether this process may power off and
/// reboot the machine.
///
/// Cached for the process lifetime on purpose: two spawned processes per
/// 5-second poll of the System tab would be absurd. Installing the polkit
/// rule therefore takes effect at the next service start — which is how it
/// gets installed in the first place, `deploy.sh` restarting the service.
///
/// The cache does not replace reporting the real failure when an action is
/// attempted: logind can still refuse at call time (another session, an
/// inhibitor), which is why the shipped rule grants all six actions.
///
/// The two calls are joined rather than awaited one after the other: this
/// probe runs before the HTTP listener binds and before the core resumes, so
/// a machine where `busctl` exists but D-Bus hangs would otherwise delay both
/// the web interface and the audio wake by up to twice the 3 s timeout.
pub async fn probe_capabilities() -> PowerProbe {
    let (off, reboot) =
        tokio::join!(query_logind("CanPowerOff"), query_logind("CanReboot"));
    probe_summary(off, reboot)
}

/// The rule for reading the two answers, **separated from obtaining them**: it
/// can then be tested without logind, which no development machine has.
pub fn probe_summary(off: Option<bool>, reboot: Option<bool>) -> PowerProbe {
    PowerProbe {
        can_power_off: off == Some(true),
        can_reboot: reboot == Some(true),
        // A single answer is enough to establish that logind is reachable: the
        // two calls can only diverge on the answer, never on the existence of
        // the other party.
        logind_reachable: off.is_some() || reboot.is_some(),
    }
}

/// What the startup probe learned.
///
/// Three facts rather than two: see `Metrics::logind_reachable` for what the
/// third one avoids saying wrongly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerProbe {
    pub can_power_off: bool,
    pub can_reboot: bool,
    pub logind_reachable: bool,
}

/// `Some(true)` allowed, `Some(false)` refused, `None` **no answer** — the
/// three cases are distinct, the last one not being fixable by a polkit
/// rule.
async fn query_logind(method: &str) -> Option<bool> {
    let call = tokio::process::Command::new("busctl")
        .args([
            "--system",
            "call",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            method,
        ])
        .output();
    // INFO and not WARN throughout: a development machine without logind is
    // a normal situation, and WARN lines are surfaced on the config page.
    match tokio::time::timeout(std::time::Duration::from_secs(3), call).await {
        Ok(Ok(out)) if out.status.success() => {
            Some(parse_can(&String::from_utf8_lossy(&out.stdout)))
        }
        Ok(Ok(out)) => {
            tracing::info!("logind {method}: {}", String::from_utf8_lossy(&out.stderr).trim());
            None
        }
        Ok(Err(e)) => {
            tracing::info!("busctl unavailable ({e}): power off and reboot disabled in the UI");
            None
        }
        Err(_) => {
            tracing::info!("logind {method}: no response within 3s");
            None
        }
    }
}

#[derive(serde::Deserialize)]
pub struct PowerRequest {
    action: String,
}

/// The three power actions of the System tab.
///
/// `poweroff` and `reboot` go through `systemctl`, hence logind and polkit:
/// the service is unprivileged and `NoNewPrivileges` rules out `sudo`. The
/// child is awaited up to 5 s so a refusal can be reported with logind's own
/// message — `Interactive authentication required` is exactly the sentence
/// that names the missing polkit rule. A fire-and-forget `202` would show a
/// page that looks fine while nothing happens.
///
/// `restart-service` needs no privilege at all: the process exits and
/// systemd starts it again, because the unit says `Restart=always` /
/// `RestartSec=2`. Exiting abruptly loses nothing — `Core::persist()` runs
/// on every change, never at shutdown.
pub async fn power_post(
    State(state): State<crate::status::AppState>,
    Json(req): Json<PowerRequest>,
) -> Response {
    let action = match parse_action(&req.action) {
        Ok(a) => a,
        Err(e) => {
            let msg = e.message(&*state.catalog.read().await);
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
                .into_response();
        }
    };
    let verb = match action {
        PowerAction::PowerOff => "poweroff",
        PowerAction::Reboot => "reboot",
        PowerAction::RestartService => {
            tracing::warn!("service restart requested from the UI");
            let info = state.system.clone();
            tokio::spawn(async move {
                tokio::time::sleep(info.restart_delay).await;
                (info.restart)();
            });
            return StatusCode::ACCEPTED.into_response();
        }
    };
    tracing::warn!("{verb} of the OS requested from the UI");
    let call = tokio::process::Command::new(&state.system.systemctl).arg(verb).output();
    match tokio::time::timeout(std::time::Duration::from_secs(5), call).await {
        // Still running after 5 s: the machine is on its way out, which is
        // the successful case. The child is not killed — dropping the
        // future leaves it alone, `kill_on_drop` being off by default.
        Err(_) => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(out)) if out.status.success() => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // Two texts for two audiences: the response body keeps the
            // sentence meant for the user (resolved through the catalog), the
            // log stays technical and entirely in English — `Display` provides
            // that version in the fallback case.
            let msg = if stderr.is_empty() {
                let err = SystemctlFailed { code: out.status.code().unwrap_or(-1) };
                tracing::warn!("{verb} refused: {err}");
                err.message(&*state.catalog.read().await)
            } else {
                tracing::warn!("{verb} refused: {stderr}");
                stderr
            };
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": msg }))).into_response()
        }
        Ok(Err(e)) => {
            let msg = SystemctlUnreachable { detail: e.to_string() }.message(&*state.catalog.read().await);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": msg }))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_in_millidegrees() {
        assert_eq!(parse_temperature("47800\n"), Some(47.8));
        assert_eq!(parse_temperature("0"), Some(0.0));
        assert_eq!(parse_temperature("chatty"), None);
        assert_eq!(parse_temperature(""), None);
    }

    #[test]
    fn frequency_in_kilohertz() {
        assert_eq!(parse_khz_to_mhz("900000\n"), Some(900));
        assert_eq!(parse_khz_to_mhz("1500000"), Some(1500));
        assert_eq!(parse_khz_to_mhz("-"), None);
    }

    #[test]
    fn load_average_and_rest_ignored() {
        assert_eq!(parse_loadavg("0.12 0.15 0.09 1/234 5678\n"), Some([0.12, 0.15, 0.09]));
        // Only two values: the line is not the expected one.
        assert_eq!(parse_loadavg("0.12 0.15\n"), None);
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn meminfo_reads_total_and_available() {
        let raw = "MemTotal:         948000 kB\nMemFree:          120000 kB\nMemAvailable:     512000 kB\n";
        assert_eq!(parse_meminfo(raw), Some(Usage { total_kb: 948_000, available_kb: 512_000 }));
        // MemAvailable missing (very old kernel): no measurement rather than
        // a wrong one derived from MemFree.
        assert_eq!(parse_meminfo("MemTotal:  948000 kB\nMemFree: 120000 kB\n"), None);
    }

    #[test]
    fn uptime_keeps_whole_seconds() {
        assert_eq!(parse_uptime("84213.42 512345.10\n"), Some(84_213));
        assert_eq!(parse_uptime("nope"), None);
    }

    #[test]
    fn os_release_without_the_quotes() {
        let raw = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\n";
        assert_eq!(parse_os_release(raw), Some("Debian GNU/Linux 12 (bookworm)".to_string()));
        assert_eq!(parse_os_release("ID=debian\n"), None);
    }

    #[test]
    fn hwmon_alarm_is_binary() {
        assert_eq!(parse_alarm("0\n"), Some(false));
        assert_eq!(parse_alarm("1\n"), Some(true));
        assert_eq!(parse_alarm(""), None);
    }

    #[test]
    fn sticky_under_voltage_bit_read_from_vcgencmd_mask() {
        // Real value observed on the owner's Pi: bit 16 (under-voltage
        // occurred since boot) and bit 18 (throttling occurred, not exposed
        // separately — see the comment on `parse_throttled`) at the same time.
        assert_eq!(parse_throttled("throttled=0x50000\n"), Some(true));
        assert_eq!(parse_throttled("throttled=0x0\n"), Some(false));
        // Bit 0 alone: *current* under-voltage, but never seen since boot —
        // which this sticky bit must not claim.
        assert_eq!(parse_throttled("throttled=0x1\n"), Some(false));
        // Bit 16 alone, without 18: the bit we care about is enough.
        assert_eq!(parse_throttled("throttled=0x10000\n"), Some(true));
        // Aberrant inputs: no claim rather than a lie.
        assert_eq!(parse_throttled(""), None);
        assert_eq!(parse_throttled("0x0\n"), None);
        assert_eq!(parse_throttled("throttled=zz\n"), None);
    }

    /// An executable script which, on every call, appends a line to
    /// `counter` (to count the launches) and answers `reply` on stdout.
    /// Used to verify the latching without depending on a real Pi.
    fn stub_vcgencmd(dir: &std::path::Path, reply: &str) -> (String, std::path::PathBuf) {
        let script = dir.join("vcgencmd");
        let counter = dir.join("calls");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho x >> '{}'\necho '{reply}'\n", counter.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (script.to_string_lossy().to_string(), counter)
    }

    #[test]
    fn under_voltage_since_boot_stops_relaunching_vcgencmd_once_seen_true() {
        let dir = tempfile::tempdir().unwrap();
        let (vcgencmd, counter) = stub_vcgencmd(dir.path(), "throttled=0x50000");
        let info = SystemInfo { vcgencmd, ..Default::default() };
        assert_eq!(under_voltage_since_boot(&info), Some(true));
        assert_eq!(under_voltage_since_boot(&info), Some(true));
        // A single call despite the two reads: the second goes through the
        // latch, not through a new execution.
        assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1);
    }

    #[test]
    fn under_voltage_since_boot_relaunches_vcgencmd_while_the_bit_stays_false() {
        let dir = tempfile::tempdir().unwrap();
        let (vcgencmd, counter) = stub_vcgencmd(dir.path(), "throttled=0x0");
        let info = SystemInfo { vcgencmd, ..Default::default() };
        assert_eq!(under_voltage_since_boot(&info), Some(false));
        assert_eq!(under_voltage_since_boot(&info), Some(false));
        // Nothing is settled yet: every read relaunches the command.
        assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 2);
    }

    #[test]
    fn under_voltage_since_boot_without_vcgencmd_returns_nothing() {
        // Nonexistent path: a machine that simply is not a Pi.
        let info = SystemInfo { vcgencmd: "/nonexistent".to_string(), ..Default::default() };
        assert_eq!(under_voltage_since_boot(&info), None);
    }

    #[test]
    fn cpu_jiffies_aggregate_line_and_cores_ignored() {
        let raw = "cpu  123456 789 34567 9876543 1234 0 567 0 0 0\ncpu0 61728 394 17283 4938271 617 0 283 0 0 0\ncpu1 61728 395 17284 4938272 617 0 284 0 0 0\n";
        // idle + iowait: 9876543 + 1234.
        assert_eq!(
            parse_cpu_jiffies(raw),
            Some((123456 + 789 + 34567 + 9876543 + 1234 + 567, 9876543 + 1234))
        );
    }

    #[test]
    fn cpu_jiffies_extra_columns_included_in_total() {
        // A future kernel that adds a column: the sum must count it.
        let raw = "cpu  100 200 300 400 500 0 0 0 0 0 999\n";
        assert_eq!(parse_cpu_jiffies(raw), Some((100 + 200 + 300 + 400 + 500 + 999, 400 + 500)));
    }

    #[test]
    fn cpu_jiffies_subtracts_guest_time_already_counted_in_user_and_nice() {
        // The kernel already counts guest time in `user` (here 1000) and
        // "nice" guest time in `nice` (here 500): adding them again through
        // `guest`/`guest_nice` (200 and 100) would count that time twice.
        // Raw sum of the ten fields: 1000+500+300+400+500+0+0+0+200+
        // 100 = 3000, minus the 300 of guest+guest_nice = 2700. idle stays
        // idle + iowait: 400 + 500 = 900.
        let raw = "cpu  1000 500 300 400 500 0 0 0 200 100\n";
        assert_eq!(parse_cpu_jiffies(raw), Some((2700, 900)));
    }

    #[test]
    fn cpu_jiffies_line_missing_or_malformed() {
        assert_eq!(parse_cpu_jiffies(""), None);
        assert_eq!(parse_cpu_jiffies("cpu0 123 456 789 0\n"), None);
        assert_eq!(parse_cpu_jiffies("cpu  123 chatty 789 0\n"), None);
        // Fewer than four fields: not enough for idle + iowait.
        assert_eq!(parse_cpu_jiffies("cpu  123 456\n"), None);
    }

    #[test]
    fn collect_fills_what_the_machine_exposes() {
        // Smoke test: the suite runs on Linux, so /proc exists and these
        // three measurements are always readable. The Raspberry Pi-specific
        // fields (temperature, frequency, under-voltage) are deliberately
        // left out of the assertions: absent under WSL as on a PC.
        let info = SystemInfo::default();
        let m = collect(&info);
        assert!(m.load.is_some(), "loadavg readable on Linux");
        assert!(m.memory.is_some_and(|u| u.total_kb > 0));
        assert!(m.disk.is_some_and(|u| u.total_kb > 0));
        assert!(m.kernel.is_some());
        assert_eq!(m.version, env!("CARGO_PKG_VERSION"));
        assert!(!m.can_power_off, "capabilities default to false");
        assert!(m.cpu_total_jiffies.is_some(), "/proc/stat readable on Linux");
        assert!(m.cpu_idle_jiffies.is_some());
    }

    use crate::status::{router, AppState};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    /// HTTP rig with a chosen `SystemInfo`, on the shared setup of the
    /// `status` tests.
    fn app(info: SystemInfo) -> axum::Router {
        router(AppState { system: Arc::new(info), ..crate::status::tests_support::app_state() })
    }

    async fn json_body(app: axum::Router, uri: &str) -> serde_json::Value {
        let resp = app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn get_system_exposes_every_key() {
        let v = json_body(app(SystemInfo::default()), "/api/system").await;
        // Stable key set: an unreadable field is `null` and stays present,
        // so the view does not have two cases to tell apart.
        for key in [
            "temperature_c", "cpu_mhz", "load", "cpus", "memory", "disk", "under_voltage",
            "under_voltage_since_boot", "uptime_s", "service_uptime_s", "hostname", "ip", "os",
            "kernel", "version", "can_power_off", "can_reboot", "logind_reachable",
            "cpu_total_jiffies", "cpu_idle_jiffies",
        ] {
            assert!(v.get(key).is_some(), "key {key} missing");
        }
        assert!(v["version"].is_string());
        assert_eq!(v["can_power_off"], false);
        assert_eq!(v["can_reboot"], false);
    }

    #[tokio::test]
    async fn get_system_reflects_known_capabilities() {
        let info = SystemInfo {
            can_power_off: true,
            can_reboot: true,
            logind_reachable: true,
            ..Default::default()
        };
        let v = json_body(app(info), "/api/system").await;
        assert_eq!(v["can_power_off"], true);
        assert_eq!(v["can_reboot"], true);
        assert_eq!(v["logind_reachable"], true);
    }

    #[test]
    fn a_logind_refusal_is_not_confused_with_its_absence() {
        // Both leave the buttons greyed out, but are not fixed the same way:
        // the refusal wants the polkit rule, the absence wants a running
        // `systemd-logind`. The page picks its sentence on that basis.
        let refused = probe_summary(Some(false), Some(false));
        assert!(refused.logind_reachable, "logind did answer, even if with no");
        assert!(!refused.can_power_off);

        let absent = probe_summary(None, None);
        assert!(!absent.logind_reachable);
        assert!(!absent.can_power_off);

        let open = probe_summary(Some(true), Some(true));
        assert!(open.logind_reachable);
        assert!(open.can_power_off && open.can_reboot);
    }

    #[test]
    fn a_single_answer_is_enough_to_call_logind_reachable() {
        // Mixed case: one call succeeds, the other times out. The other party exists.
        let m = probe_summary(Some(true), None);
        assert!(m.logind_reachable);
        assert!(m.can_power_off);
        assert!(!m.can_reboot, "without an answer, nothing is offered");
    }

    #[test]
    fn action_known_or_refused() {
        assert_eq!(parse_action("poweroff"), Ok(PowerAction::PowerOff));
        assert_eq!(parse_action("reboot"), Ok(PowerAction::Reboot));
        assert_eq!(parse_action("restart-service"), Ok(PowerAction::RestartService));
        assert!(parse_action("").is_err());
        assert!(parse_action("halt").is_err());
        // No case tolerance and no aliases: the only client is the SPA,
        // which sends these three exact strings.
        assert!(parse_action("PowerOff").is_err());
    }

    /// Minimal catalog loaded from a temporary directory, to test the
    /// interpolation without depending on the shipped packs.
    fn test_catalog(keys: &str) -> ritornello_i18n::Catalog {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), keys).unwrap();
        // `Catalog::load` copies everything into memory, so the temporary
        // root can be thrown away at the end of the function.
        ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::i18n::EN)
    }

    #[test]
    fn unknown_action_message_uses_the_catalog() {
        let cat = test_catalog("power_action_unknown = \"action inconnue\"\n");
        assert_eq!(UnknownPowerAction.message(&cat), "action inconnue");
    }

    #[test]
    fn systemctl_failed_message_interpolates_the_code() {
        let cat = test_catalog("systemctl_failed = \"echec systemctl (code {code})\"\n");
        assert_eq!(SystemctlFailed { code: 1 }.message(&cat), "echec systemctl (code 1)");
    }

    #[test]
    fn systemctl_unreachable_message_interpolates_the_detail() {
        let cat = test_catalog("systemctl_unreachable = \"injoignable : {detail}\"\n");
        assert_eq!(
            SystemctlUnreachable { detail: "No such file or directory".to_string() }.message(&cat),
            "injoignable : No such file or directory"
        );
    }

    #[test]
    fn logind_answer() {
        assert!(parse_can("s \"yes\"\n"));
        assert!(!parse_can("s \"no\"\n"));
        // "challenge" = interactive authentication, which a system service
        // can never satisfy: it is a no.
        assert!(!parse_can("s \"challenge\"\n"));
        assert!(!parse_can(""));
    }

    async fn post_power(app: axum::Router, body: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::post("/api/system/power")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v = if body.is_empty() { serde_json::Value::Null } else { serde_json::from_slice(&body).unwrap() };
        (status, v)
    }

    #[tokio::test]
    async fn post_power_unknown_action_returns_usable_422() {
        let (status, v) = post_power(app(SystemInfo::default()), r#"{"action":"halt"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        // A message in the `error` field, like /api/theme and
        // /api/audio-output: that is what the SPA turns into a toast.
        assert!(v["error"].is_string());
    }

    #[tokio::test]
    async fn post_power_accepts_when_systemctl_succeeds() {
        // `/bin/true` plays the role of systemctl: the real code path is
        // exercised (spawn, await, exit code) without endangering the
        // machine running the tests.
        let info = SystemInfo { systemctl: "/bin/true".to_string(), ..Default::default() };
        let (status, _) = post_power(app(info), r#"{"action":"poweroff"}"#).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn post_power_relays_systemctl_failure() {
        let info = SystemInfo { systemctl: "/bin/false".to_string(), ..Default::default() };
        let (status, v) = post_power(app(info), r#"{"action":"reboot"}"#).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        // /bin/false writes nothing on stderr: the fallback names the exit
        // code rather than returning an empty string.
        assert!(v["error"].as_str().is_some_and(|m| !m.is_empty()));
    }

    #[tokio::test]
    async fn post_power_relays_systemctl_launch_failure() {
        // Nonexistent path: `Command::output` fails before even launching a
        // process (branch `Ok(Err(e))`, distinct from the failure exercised
        // by /bin/false, which does launch systemctl but has it return an
        // error code).
        let info = SystemInfo { systemctl: "/nonexistent".to_string(), ..Default::default() };
        let (status, v) = post_power(app(info), r#"{"action":"poweroff"}"#).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(v["error"].as_str().is_some_and(|m| !m.is_empty()));
    }

    #[tokio::test]
    async fn post_power_restarts_the_service_through_the_hook() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let triggered = Arc::new(AtomicBool::new(false));
        let witness = triggered.clone();
        let info = SystemInfo {
            restart_delay: std::time::Duration::from_millis(10),
            restart: Arc::new(move || witness.store(true, Ordering::SeqCst)),
            ..Default::default()
        };
        let (status, _) = post_power(app(info), r#"{"action":"restart-service"}"#).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        // The response leaves before the process exits: the hook is called
        // by a detached task, after the delay.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(triggered.load(Ordering::SeqCst), "the restart hook must be called");
    }

    #[tokio::test]
    async fn terminate_process_really_kills_the_targeted_process() {
        // Regression observed in use: restarting the service left mpv alive,
        // playing, because `std::process::exit` runs no `Drop` and therefore
        // never the `kill_on_drop(true)` of its spawn. The test targets a
        // real process: a cleanup guarantee that goes unchecked is exactly
        // what produced this defect.
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("/bin/sleep must exist");
        terminate_process(child.id());
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .expect("the process must die well before this timeout")
            .expect("waiting for the process");
        // Terminated by a signal, hence not a successful exit: that is the
        // proof the signal was sent, and not that `sleep 30` finished on its own.
        assert!(!status.success(), "the process must be killed by the signal");
    }

    #[tokio::test]
    async fn terminate_process_without_pid_does_nothing() {
        // The case of a child already reaped (`Child::id()` then returns `None`):
        // it must not panic, and above all not target a default pid — an
        // `unwrap_or(0)` would send the signal to the whole process group.
        terminate_process(None);
    }
}
