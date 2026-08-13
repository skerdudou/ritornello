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
use serde::Serialize;

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
}

/// Process-lifetime facts the metrics endpoint needs: when this process
/// started, and what logind allows it to do.
#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub started: std::time::Instant,
    pub can_power_off: bool,
    pub can_reboot: bool,
}

impl Default for SystemInfo {
    /// Capabilities default to `false`: not knowing means offering nothing.
    /// `main` replaces them with what logind answers.
    fn default() -> Self {
        Self { started: std::time::Instant::now(), can_power_off: false, can_reboot: false }
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
    }
}

use axum::extract::State;
use axum::Json;

/// Metrics for the System tab. Read on demand, nothing cached: the page
/// polls, and everything here costs a handful of pseudo-file reads.
pub async fn system_json(State(state): State<crate::status::AppState>) -> Json<Metrics> {
    Json(collect(&state.system))
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
            "can_power_off", "can_reboot",
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
}
