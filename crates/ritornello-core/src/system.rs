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
    pub uptime_s: Option<u64>,
    pub service_uptime_s: u64,
    pub hostname: Option<String>,
    pub ip: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub version: String,
    pub can_power_off: bool,
    pub can_reboot: bool,
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
#[derive(Clone)]
pub struct SystemInfo {
    pub started: std::time::Instant,
    pub can_power_off: bool,
    pub can_reboot: bool,
    /// Command used for the OS power actions. A field rather than a
    /// constant **so the destructive routes can be tested**: a test that
    /// really ran `systemctl poweroff` would shut down the machine running
    /// the suite. Tests point it at `/bin/true` and `/bin/false` and still
    /// exercise the real spawn/await/exit-code path.
    pub systemctl: String,
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
            systemctl: "systemctl".to_string(),
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
    let mut champs = raw.split_whitespace();
    let un = champs.next()?.parse().ok()?;
    let cinq = champs.next()?.parse().ok()?;
    let quinze = champs.next()?.parse().ok()?;
    Some([un, cinq, quinze])
}

/// `/proc/meminfo`: "MemTotal:         948000 kB".
///
/// `MemAvailable` and not `MemFree`: free memory alone counts caches as
/// used and reads as alarmingly low on a healthy Linux, while
/// `MemAvailable` is the kernel's own estimate of what a new workload could
/// take. A kernel too old to publish it reports no measurement rather than
/// a misleading one.
pub fn parse_meminfo(raw: &str) -> Option<Usage> {
    let champ = |nom: &str| -> Option<u64> {
        raw.lines()
            .find_map(|l| l.strip_prefix(nom)?.strip_prefix(':'))
            .and_then(|reste| reste.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    };
    Some(Usage { total_kb: champ("MemTotal")?, available_kb: champ("MemAvailable")? })
}

/// `/proc/uptime`: "84213.42 512345.10" — seconds since boot, then idle
/// time summed over all cores.
pub fn parse_uptime(raw: &str) -> Option<u64> {
    let secondes: f64 = raw.split_whitespace().next()?.parse().ok()?;
    Some(secondes as u64)
}

/// `/etc/os-release`: `PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"`.
pub fn parse_os_release(raw: &str) -> Option<String> {
    let valeur = raw.lines().find_map(|l| l.strip_prefix("PRETTY_NAME="))?;
    Some(valeur.trim().trim_matches('"').to_string())
}

/// hwmon alarm files hold "1" while the alarm is raised, "0" otherwise.
pub fn parse_alarm(raw: &str) -> Option<bool> {
    match raw.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
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
    let ligne = raw.lines().find(|l| l.starts_with("cpu "))?;
    let champs: Vec<u64> = ligne
        .split_whitespace()
        .skip(1)
        .map(|v| v.parse::<u64>())
        .collect::<Result<_, _>>()
        .ok()?;
    if champs.len() < 4 {
        return None;
    }
    let total: u64 = champs.iter().sum();
    let invite = champs.get(8).copied().unwrap_or(0) + champs.get(9).copied().unwrap_or(0);
    let idle = champs[3] + champs.get(4).copied().unwrap_or(0);
    Some((total - invite, idle))
}

/// Reads a pseudo-file, `None` on any error. Absence is the normal case for
/// most of these paths, not an incident worth a log line.
fn lire(chemin: &str) -> Option<String> {
    std::fs::read_to_string(chemin).ok()
}

/// The Raspberry Pi undervoltage flag, published by the `rpi_volt` driver
/// as `in0_lcrit_alarm`. The hwmon number varies with probe order, hence
/// the scan by driver `name` rather than a hardcoded `hwmon0`. `vcgencmd`
/// was rejected: it needs the `video` group and a spawned process per poll.
fn under_voltage() -> Option<bool> {
    for entree in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        // `unwrap_or_default` and not `?`: one unreadable entry must not
        // abandon the scan of the others.
        let nom = std::fs::read_to_string(entree.path().join("name")).unwrap_or_default();
        if nom.trim() == "rpi_volt" {
            let alarme = std::fs::read_to_string(entree.path().join("in0_lcrit_alarm")).ok()?;
            return parse_alarm(&alarme);
        }
    }
    None
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
fn disk_usage(chemin: &str) -> Option<Usage> {
    let c = std::ffi::CString::new(chemin).ok()?;
    // SAFETY: `statvfs` only writes into the struct we hand it, and the
    // path stays a valid NUL-terminated C string for the whole call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bloc = u64::from(st.f_frsize);
    Some(Usage {
        total_kb: u64::from(st.f_blocks) * bloc / 1024,
        available_kb: u64::from(st.f_bavail) * bloc / 1024,
    })
}

/// Local address of the interface facing the default route.
///
/// The UDP socket **sends nothing**: `connect` on a datagram socket only
/// asks the kernel which local address would be used to reach that
/// destination. No internet access is needed or attempted — `8.8.8.8:53` is
/// a routable address, not a server we talk to. `None` when there is no
/// route at all.
fn adresse_ip() -> Option<String> {
    let prise = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    prise.connect("8.8.8.8:53").ok()?;
    Some(prise.local_addr().ok()?.ip().to_string())
}

/// Reads everything, once, for one HTTP response.
pub fn collect(info: &SystemInfo) -> Metrics {
    // A single read of /proc/stat feeds both counters below: two separate
    // reads could straddle a tick and skew the delta the page computes
    // between polls.
    let cpu_jiffies = lire("/proc/stat").as_deref().and_then(parse_cpu_jiffies);
    Metrics {
        temperature_c: lire("/sys/class/thermal/thermal_zone0/temp")
            .as_deref()
            .and_then(parse_temperature),
        cpu_mhz: lire("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
            .as_deref()
            .and_then(parse_khz_to_mhz),
        load: lire("/proc/loadavg").as_deref().and_then(parse_loadavg),
        cpus: std::thread::available_parallelism().ok().map(|n| n.get()),
        memory: lire("/proc/meminfo").as_deref().and_then(parse_meminfo),
        disk: disk_usage("/"),
        under_voltage: under_voltage(),
        uptime_s: lire("/proc/uptime").as_deref().and_then(parse_uptime),
        service_uptime_s: info.started.elapsed().as_secs(),
        hostname: lire("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()),
        ip: adresse_ip(),
        os: lire("/etc/os-release").as_deref().and_then(parse_os_release),
        kernel: lire("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()),
        version: env!("CARGO_PKG_VERSION").to_string(),
        can_power_off: info.can_power_off,
        can_reboot: info.can_reboot,
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

/// The action arrives as a string and is validated here rather than through
/// serde's enum deserialization: an unknown value must answer with this
/// project's `422 {"error": …}` shape, whereas a serde rejection would
/// answer with axum's own plain-text 422. Same reasoning as
/// `validate_audio_device`.
pub fn parse_action(action: &str) -> Result<PowerAction, String> {
    match action {
        "poweroff" => Ok(PowerAction::PowerOff),
        "reboot" => Ok(PowerAction::Reboot),
        "restart-service" => Ok(PowerAction::RestartService),
        _ => Err("action d'alimentation inconnue".to_string()),
    }
}

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
pub async fn probe_capabilities() -> (bool, bool) {
    tokio::join!(interroge_logind("CanPowerOff"), interroge_logind("CanReboot"))
}

async fn interroge_logind(methode: &str) -> bool {
    let appel = tokio::process::Command::new("busctl")
        .args([
            "--system",
            "call",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            methode,
        ])
        .output();
    // INFO and not WARN throughout: a development machine without logind is
    // a normal situation, and WARN lines are surfaced on the config page.
    match tokio::time::timeout(std::time::Duration::from_secs(3), appel).await {
        Ok(Ok(out)) if out.status.success() => parse_can(&String::from_utf8_lossy(&out.stdout)),
        Ok(Ok(out)) => {
            tracing::info!("logind {methode}: {}", String::from_utf8_lossy(&out.stderr).trim());
            false
        }
        Ok(Err(e)) => {
            tracing::info!("busctl unavailable ({e}): power off and reboot disabled in the UI");
            false
        }
        Err(_) => {
            tracing::info!("logind {methode}: no response within 3s");
            false
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
        Err(msg) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
                .into_response()
        }
    };
    let verbe = match action {
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
    tracing::warn!("{verbe} of the OS requested from the UI");
    let appel = tokio::process::Command::new(&state.system.systemctl).arg(verbe).output();
    match tokio::time::timeout(std::time::Duration::from_secs(5), appel).await {
        // Still running after 5 s: the machine is on its way out, which is
        // the successful case. The child is not killed — dropping the
        // future leaves it alone, `kill_on_drop` being off by default.
        Err(_) => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(out)) if out.status.success() => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let msg = if stderr.is_empty() {
                format!("systemctl a échoué (code {})", out.status.code().unwrap_or(-1))
            } else {
                stderr.clone()
            };
            // Deux textes pour deux publics : le corps de la réponse garde la
            // phrase destinée au lecteur (française jusqu'à son passage par
            // catalogue), le log reste technique et entièrement en anglais.
            // Interpoler `msg` ici mêlerait les deux langues dans une ligne de
            // journal, et le code de sortie y est plus parlant qu'une phrase.
            let detail = if stderr.is_empty() {
                format!("exit code {}", out.status.code().unwrap_or(-1))
            } else {
                stderr
            };
            tracing::warn!("{verbe} refused: {detail}");
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": msg }))).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("systemctl injoignable: {e}") })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_en_millidegres() {
        assert_eq!(parse_temperature("47800\n"), Some(47.8));
        assert_eq!(parse_temperature("0"), Some(0.0));
        assert_eq!(parse_temperature("bavard"), None);
        assert_eq!(parse_temperature(""), None);
    }

    #[test]
    fn frequence_en_kilohertz() {
        assert_eq!(parse_khz_to_mhz("900000\n"), Some(900));
        assert_eq!(parse_khz_to_mhz("1500000"), Some(1500));
        assert_eq!(parse_khz_to_mhz("-"), None);
    }

    #[test]
    fn charge_moyenne_et_reste_ignore() {
        assert_eq!(parse_loadavg("0.12 0.15 0.09 1/234 5678\n"), Some([0.12, 0.15, 0.09]));
        // Deux valeurs seulement : la ligne n'est pas celle attendue.
        assert_eq!(parse_loadavg("0.12 0.15\n"), None);
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn meminfo_lit_total_et_disponible() {
        let raw = "MemTotal:         948000 kB\nMemFree:          120000 kB\nMemAvailable:     512000 kB\n";
        assert_eq!(parse_meminfo(raw), Some(Usage { total_kb: 948_000, available_kb: 512_000 }));
        // MemAvailable absent (noyau très ancien) : pas de mesure plutôt
        // qu'une mesure fausse tirée de MemFree.
        assert_eq!(parse_meminfo("MemTotal:  948000 kB\nMemFree: 120000 kB\n"), None);
    }

    #[test]
    fn uptime_garde_les_secondes_entieres() {
        assert_eq!(parse_uptime("84213.42 512345.10\n"), Some(84_213));
        assert_eq!(parse_uptime("nope"), None);
    }

    #[test]
    fn os_release_sans_les_guillemets() {
        let raw = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\n";
        assert_eq!(parse_os_release(raw), Some("Debian GNU/Linux 12 (bookworm)".to_string()));
        assert_eq!(parse_os_release("ID=debian\n"), None);
    }

    #[test]
    fn alarme_hwmon_binaire() {
        assert_eq!(parse_alarm("0\n"), Some(false));
        assert_eq!(parse_alarm("1\n"), Some(true));
        assert_eq!(parse_alarm(""), None);
    }

    #[test]
    fn jiffies_cpu_ligne_agregee_et_coeurs_ignores() {
        let raw = "cpu  123456 789 34567 9876543 1234 0 567 0 0 0\ncpu0 61728 394 17283 4938271 617 0 283 0 0 0\ncpu1 61728 395 17284 4938272 617 0 284 0 0 0\n";
        // idle + iowait : 9876543 + 1234.
        assert_eq!(
            parse_cpu_jiffies(raw),
            Some((123456 + 789 + 34567 + 9876543 + 1234 + 567, 9876543 + 1234))
        );
    }

    #[test]
    fn jiffies_cpu_colonnes_supplementaires_incluses_dans_le_total() {
        // Un noyau futur qui ajoute une colonne : la somme doit la compter.
        let raw = "cpu  100 200 300 400 500 0 0 0 0 0 999\n";
        assert_eq!(parse_cpu_jiffies(raw), Some((100 + 200 + 300 + 400 + 500 + 999, 400 + 500)));
    }

    #[test]
    fn jiffies_cpu_soustrait_le_temps_invite_deja_compte_dans_user_et_nice() {
        // Le noyau compte déjà le temps invité dans `user` (ici 1000) et le
        // temps invité "nice" dans `nice` (ici 500) : les additionner aussi
        // via `guest`/`guest_nice` (200 et 100) compterait ce temps deux
        // fois. Somme brute des dix champs : 1000+500+300+400+500+0+0+0+200+
        // 100 = 3000, moins les 300 de guest+guest_nice = 2700. idle reste
        // idle + iowait : 400 + 500 = 900.
        let raw = "cpu  1000 500 300 400 500 0 0 0 200 100\n";
        assert_eq!(parse_cpu_jiffies(raw), Some((2700, 900)));
    }

    #[test]
    fn jiffies_cpu_ligne_absente_ou_malformee() {
        assert_eq!(parse_cpu_jiffies(""), None);
        assert_eq!(parse_cpu_jiffies("cpu0 123 456 789 0\n"), None);
        assert_eq!(parse_cpu_jiffies("cpu  123 bavard 789 0\n"), None);
        // Moins de quatre champs : pas assez pour idle + iowait.
        assert_eq!(parse_cpu_jiffies("cpu  123 456\n"), None);
    }

    #[test]
    fn collect_remplit_ce_que_la_machine_expose() {
        // Test de fumée : la suite tourne sous Linux, donc /proc existe et
        // ces trois mesures sont toujours lisibles. Les champs propres au
        // Raspberry Pi (température, fréquence, sous-tension) restent
        // volontairement hors assertion : absents sous WSL comme sur un PC.
        let info = SystemInfo::default();
        let m = collect(&info);
        assert!(m.load.is_some(), "loadavg lisible sous Linux");
        assert!(m.memory.is_some_and(|u| u.total_kb > 0));
        assert!(m.disk.is_some_and(|u| u.total_kb > 0));
        assert!(m.kernel.is_some());
        assert_eq!(m.version, env!("CARGO_PKG_VERSION"));
        assert!(!m.can_power_off, "capacités à false par défaut");
        assert!(m.cpu_total_jiffies.is_some(), "/proc/stat lisible sous Linux");
        assert!(m.cpu_idle_jiffies.is_some());
    }

    use crate::status::{router, AppState};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    /// Montage HTTP avec un `SystemInfo` choisi, sur le montage partagé des
    /// tests de `status.rs`.
    fn app(info: SystemInfo) -> axum::Router {
        router(AppState { system: Arc::new(info), ..crate::status::tests_support::app_state() })
    }

    async fn corps_json(app: axum::Router, uri: &str) -> serde_json::Value {
        let resp = app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn get_system_expose_toutes_les_cles() {
        let v = corps_json(app(SystemInfo::default()), "/api/system").await;
        // Jeu de clés stable : un champ illisible vaut `null` et reste
        // présent, pour que la vue n'ait pas deux cas à distinguer.
        for cle in [
            "temperature_c", "cpu_mhz", "load", "cpus", "memory", "disk", "under_voltage",
            "uptime_s", "service_uptime_s", "hostname", "ip", "os", "kernel", "version",
            "can_power_off", "can_reboot", "cpu_total_jiffies", "cpu_idle_jiffies",
        ] {
            assert!(v.get(cle).is_some(), "clé {cle} absente");
        }
        assert!(v["version"].is_string());
        assert_eq!(v["can_power_off"], false);
        assert_eq!(v["can_reboot"], false);
    }

    #[tokio::test]
    async fn get_system_reflete_les_capacites_connues() {
        let info = SystemInfo { can_power_off: true, can_reboot: true, ..Default::default() };
        let v = corps_json(app(info), "/api/system").await;
        assert_eq!(v["can_power_off"], true);
        assert_eq!(v["can_reboot"], true);
    }

    #[test]
    fn action_connue_ou_refusee() {
        assert_eq!(parse_action("poweroff"), Ok(PowerAction::PowerOff));
        assert_eq!(parse_action("reboot"), Ok(PowerAction::Reboot));
        assert_eq!(parse_action("restart-service"), Ok(PowerAction::RestartService));
        assert!(parse_action("").is_err());
        assert!(parse_action("halt").is_err());
        // Pas de tolérance de casse ni d'alias : le seul client est la SPA,
        // qui envoie ces trois chaînes exactes.
        assert!(parse_action("PowerOff").is_err());
    }

    #[test]
    fn reponse_de_logind() {
        assert!(parse_can("s \"yes\"\n"));
        assert!(!parse_can("s \"no\"\n"));
        // « challenge » = authentification interactive, qu'un service
        // système ne peut jamais satisfaire : c'est un non.
        assert!(!parse_can("s \"challenge\"\n"));
        assert!(!parse_can(""));
    }

    async fn post_power(app: axum::Router, corps: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::post("/api/system/power")
                    .header("content-type", "application/json")
                    .body(Body::from(corps.to_string()))
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
    async fn post_power_action_inconnue_renvoie_422_exploitable() {
        let (status, v) = post_power(app(SystemInfo::default()), r#"{"action":"halt"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        // Un message dans le champ `error`, comme /api/theme et
        // /api/audio-output : c'est ce que la SPA transforme en toast.
        assert!(v["error"].is_string());
    }

    #[tokio::test]
    async fn post_power_accepte_quand_systemctl_reussit() {
        // `/bin/true` tient le rôle de systemctl : le chemin réel du code
        // est exercé (lancement, attente, code de sortie) sans risquer la
        // machine qui exécute les tests.
        let info = SystemInfo { systemctl: "/bin/true".to_string(), ..Default::default() };
        let (status, _) = post_power(app(info), r#"{"action":"poweroff"}"#).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn post_power_relaie_lechec_de_systemctl() {
        let info = SystemInfo { systemctl: "/bin/false".to_string(), ..Default::default() };
        let (status, v) = post_power(app(info), r#"{"action":"reboot"}"#).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        // /bin/false n'écrit rien sur stderr : le repli nomme le code de
        // sortie plutôt que de renvoyer une chaîne vide.
        assert!(v["error"].as_str().is_some_and(|m| !m.is_empty()));
    }

    #[tokio::test]
    async fn post_power_relaie_lechec_de_lancement_de_systemctl() {
        // Chemin inexistant : `Command::output` échoue avant même de lancer
        // un processus (branche `Ok(Err(e))`, distincte de l'échec exercé par
        // /bin/false, qui lance bien systemctl mais lui fait rendre un code
        // d'erreur).
        let info = SystemInfo { systemctl: "/nonexistent".to_string(), ..Default::default() };
        let (status, v) = post_power(app(info), r#"{"action":"poweroff"}"#).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(v["error"].as_str().is_some_and(|m| !m.is_empty()));
    }

    #[tokio::test]
    async fn post_power_redemarre_le_service_par_le_crochet() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let declenche = Arc::new(AtomicBool::new(false));
        let temoin = declenche.clone();
        let info = SystemInfo {
            restart_delay: std::time::Duration::from_millis(10),
            restart: Arc::new(move || temoin.store(true, Ordering::SeqCst)),
            ..Default::default()
        };
        let (status, _) = post_power(app(info), r#"{"action":"restart-service"}"#).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        // La réponse part avant la sortie du process : le crochet est
        // appelé par une tâche détachée, après le délai.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(declenche.load(Ordering::SeqCst), "le crochet de redémarrage doit être appelé");
    }

    #[tokio::test]
    async fn terminate_process_tue_vraiment_le_processus_vise() {
        // Régression constatée à l'usage : la relance du service laissait mpv
        // vivant, à jouer, parce que `std::process::exit` n'exécute aucun
        // `Drop` et donc jamais le `kill_on_drop(true)` de son lancement. Le
        // test porte sur un vrai processus : une garantie de nettoyage qu'on
        // ne vérifie pas est exactement ce qui a produit ce défaut.
        let mut enfant = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("/bin/sleep doit exister");
        terminate_process(enfant.id());
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), enfant.wait())
            .await
            .expect("le processus doit mourir bien avant ce délai")
            .expect("attente du processus");
        // Terminé par un signal, donc pas une sortie réussie : c'est la preuve
        // que le signal est bien parti, et pas que `sleep 30` a fini tout seul.
        assert!(!status.success(), "le processus doit être tué par le signal");
    }

    #[tokio::test]
    async fn terminate_process_sans_pid_ne_fait_rien() {
        // Le cas d'un enfant déjà moissonné (`Child::id()` rend alors `None`) :
        // il ne doit pas paniquer, et surtout pas viser un pid par défaut — un
        // `unwrap_or(0)` enverrait le signal à tout le groupe de processus.
        terminate_process(None);
    }
}
