# System tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a System tab to the web UI showing OS metrics with a local
CPU/RAM history graph, and offering three power actions — shut down the OS,
reboot the OS, restart the Ritornello service.

**Architecture:** A new core module `system.rs` follows the shape of
`audio_output.rs`: pure parsers with unit tests, thin I/O wrappers that are
not unit-tested, handlers registered in `status.rs`. `GET /api/system`
returns every metric as an optional field (`null` when the machine does not
expose it), `POST /api/system/power` performs one of three actions. The SPA
gets a `/system` route that polls the endpoint every 5 s while visible and
keeps its own in-memory history.

**Tech Stack:** Rust (axum, tokio, serde, libc), Vue 3 + TypeScript
(`@ritornello/ui`, vue-router), vitest, Playwright, systemd + logind +
polkit.

**Design document:** `docs/superpowers/specs/2026-08-13-onglet-systeme-design.md`
— read it if a requirement here seems underspecified; it carries the
rationale, this plan carries the code.

## Global Constraints

- **Working directory:** `C:\projets\perso\ritornello\.claude\worktrees\systeme`
  (a git worktree on branch `worktree-systeme`). Never `cd` to the shared
  checkout at `C:\projets\perso\ritornello`.
- **Rust commands run under WSL only**, from PowerShell:
  `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test --workspace"`.
  Same form for `cargo clippy --workspace --all-targets -- -D warnings`.
  A bare `cargo` on the Windows side does not exist in this workshop.
- **`-D warnings` is a hard gate.** In particular, never write `x as u64`
  on a libc field whose width differs between architectures: on the target
  where it is already `u64`, `clippy::unnecessary_cast` fires and the build
  fails. Use `u64::from(x)`, which compiles for both widths.
- **npm/node/git/Playwright run natively on Windows**, from the worktree
  root. This worktree is fresh: run `npm install` once before the first npm
  command.
- Build order matters and is not negotiable: `npm run build` (root — it
  walks the workspaces in dependency order, `web/kit` before `web/app`)
  **then** the cargo build, because `crates/ritornello-core/build.rs`
  embeds `web/app/dist`. After an npm rebuild, `touch
  crates/ritornello-core/build.rs` so cargo notices.
- Web test commands: `npm test` (all workspaces) or `npm test -w app`;
  `npm run typecheck`; e2e with `npx playwright test` from `web/app`.
- **Language policy:** documentation and new Rust comments in **English**;
  Rust test names in **French**; SPA comments and SPA test names in
  **French**; user-facing strings through i18n only.
- **Every i18n key must exist in BOTH** `crates/ritornello-core/src/locales/en.toml`
  (English) **and** `deploy/locales/core/fr.toml` (French). The SPA guard
  `web/app/src/i18nKeysUsed.test.ts` reads the former and fails on a key
  used but absent.
- HTTP conventions to match exactly: validate before mutating; refusal is
  `422` with a JSON body `{"error": "<message in French>"}`; success with no
  content is `204`; an accepted action whose effect outlives the response is
  `202`. The SPA's `api.put`/`api.post` return that `error` string, or
  `null` when the response is ok.
- Commit messages in French, conventional-commits prefix, e.g.
  `feat(core): métriques système exposées par GET /api/system`. One commit
  per task unless a task says otherwise.
- Do not fix unrelated pre-existing problems. Report them instead.

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/ritornello-core/Cargo.toml` | `libc` dependency (already in the lock via tokio) | 1 |
| `crates/ritornello-core/src/system.rs` | metrics (parsers + `collect`), then power actions and the logind probe | 1, 3 |
| `crates/ritornello-core/src/main.rs` | `mod system;`, `SystemInfo` construction, startup probe | 2, 3 |
| `crates/ritornello-core/src/status.rs` | `AppState.system`, route registration, test constructors | 2, 3 |
| `deploy/50-ritornello-power.rules` | polkit rule (six logind actions) | 4 |
| `deploy/deploy.sh` | installs the rule | 4 |
| `web/app/src/views/sparkline.ts` | pure SVG path builder | 5 |
| `web/app/src/types.ts` | `SystemPayload`, `SystemUsage` | 6 |
| `web/app/src/router.ts`, `web/app/src/App.vue` | `/system` route and nav link | 6 |
| `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml` | all `system_*` keys | 6 |
| `web/app/src/views/SystemView.vue` | metrics, polling, history; then the power card | 6, 7 |
| `docs/installation.md`, `docs/interface.md` | polkit prerequisite, route contract | 8 |
| `web/app/e2e/parcours.spec.ts` | the tab renders (never confirms an action) | 8 |

---

### Task 1: Metrics module (`system.rs`)

**Files:**
- Create: `crates/ritornello-core/src/system.rs`
- Modify: `crates/ritornello-core/Cargo.toml`

**Interfaces:**
- Produces: `pub struct Usage { total_kb: u64, available_kb: u64 }`,
  `pub struct Metrics { … }`, `pub struct SystemInfo { started: Instant,
  can_power_off: bool, can_reboot: bool }` with `impl Default`, and
  `pub fn collect(info: &SystemInfo) -> Metrics`. Task 2 serves `Metrics`
  over HTTP; Task 3 adds three fields to `SystemInfo`.
- Consumes: nothing.

TDD: write the parser tests first, then the parsers.

- [ ] **Step 1: Add the `libc` dependency**

In `crates/ritornello-core/Cargo.toml`, under `[dependencies]`, after
`rust-embed`:

```toml
# statvfs, for the root filesystem usage shown on the System tab. Already
# in Cargo.lock (tokio depends on it), so this adds no compilation.
libc = "0.2"
```

- [ ] **Step 2: Write the failing parser tests**

Create `crates/ritornello-core/src/system.rs` containing **only** this test
module for now (the file will not compile until Step 3 — that is the point):

```rust
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
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test -p ritornello-core system"`
Expected: compilation errors — `parse_temperature` and friends do not exist.
(`system.rs` is not declared as a module yet either; add `mod system;` to
`crates/ritornello-core/src/main.rs` alongside the other `mod` lines in the
next step, otherwise the file is never compiled.)

- [ ] **Step 4: Write the module**

Add `mod system;` to `crates/ritornello-core/src/main.rs`, in the existing
list of `mod` declarations (alphabetical order: after `mod status;`).

Then prepend to `crates/ritornello-core/src/system.rs`, above the test
module:

```rust
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
fn disk_usage(chemin: &str) -> Option<Usage> {
    let c = std::ffi::CString::new(chemin).ok()?;
    // SAFETY: `statvfs` only writes into the struct we hand it, and the
    // path stays a valid NUL-terminated C string for the whole call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    // `u64::from` and not `as u64`: these field widths differ between
    // architectures, and on the one where they are already `u64`,
    // `clippy::unnecessary_cast` would fail the `-D warnings` build.
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
```

Note on `parse_temperature`: `millis as f32` is intentional and passes
`-D warnings` — `clippy::cast_precision_loss` belongs to the pedantic group,
which is not enabled here. If clippy nonetheless objects, follow clippy; do
not add an `allow`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test -p ritornello-core system"`
Expected: the 8 tests pass.

- [ ] **Step 6: Run the whole suite and clippy**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings"`
Expected: everything green, clippy silent. Report the exact totals
(`N passed; 0 failed` summed over the binaries) in your report.

- [ ] **Step 7: Commit**

```
git add crates/ritornello-core/Cargo.toml crates/ritornello-core/src/system.rs crates/ritornello-core/src/main.rs
git commit -m "feat(core): lecture des métriques de l'OS (procfs, sysfs, statvfs)"
```

---

### Task 2: `GET /api/system`

**Files:**
- Modify: `crates/ritornello-core/src/status.rs` (AppState field, route, 4
  test constructors), `crates/ritornello-core/src/system.rs` (handler +
  HTTP tests), `crates/ritornello-core/src/main.rs` (build the field)

**Interfaces:**
- Consumes: `system::{collect, Metrics, SystemInfo}` from Task 1.
- Produces: `AppState.system: Arc<crate::system::SystemInfo>` and
  `system::system_json`. Task 3 adds `POST /api/system/power` next to it.

- [ ] **Step 1: Write the failing HTTP tests**

Append to the `mod tests` of `crates/ritornello-core/src/system.rs`:

```rust
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
```

- [ ] **Step 2: Run them to verify they fail**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test -p ritornello-core system"`
Expected: compilation failure — `AppState` has no field `system`, and
`/api/system` has no route.

- [ ] **Step 3: Add the field, the route and the handler**

In `crates/ritornello-core/src/status.rs`, add to `AppState` after
`player`:

```rust
    /// Process-lifetime system facts (start instant, what logind allows),
    /// read by the System tab's endpoints. One `Arc` field rather than
    /// three loose ones: every test constructor below would otherwise grow
    /// by three lines.
    pub system: Arc<crate::system::SystemInfo>,
```

In `pub fn router`, after the `/api/settings` line:

```rust
        .route("/api/system", get(crate::system::system_json))
```

In each of the four full `AppState { … }` literals of
`mod tests_support` (`app_state`, `app_state_with_audio`,
`app_state_with_cmd`, `app_state_fr`), add as the last field:

```rust
            system: Default::default(),
```

In `crates/ritornello-core/src/system.rs`, add above the test module:

```rust
use axum::extract::State;
use axum::Json;

/// Metrics for the System tab. Read on demand, nothing cached: the page
/// polls, and everything here costs a handful of pseudo-file reads.
pub async fn system_json(State(state): State<crate::status::AppState>) -> Json<Metrics> {
    Json(collect(&state.system))
}
```

In `crates/ritornello-core/src/main.rs`, in the `AppState { … }` literal
(around line 311), add as the last field:

```rust
            system: Arc::new(system::SystemInfo::default()),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test -p ritornello-core"`
Expected: the two new tests pass, the existing `status.rs` tests still pass.

- [ ] **Step 5: Full suite, clippy, commit**

Run the workspace suite and clippy as in Task 1 Step 6, then:

```
git add crates/ritornello-core/src
git commit -m "feat(core): GET /api/system sert les métriques de l'OS"
```

---

### Task 3: Power actions and the logind capability probe

**Files:**
- Modify: `crates/ritornello-core/src/system.rs` (three `SystemInfo`
  fields, `PowerAction`, `parse_action`, `parse_can`, `probe_capabilities`,
  `power_post`, tests), `crates/ritornello-core/src/status.rs` (route),
  `crates/ritornello-core/src/main.rs` (probe at startup)

**Interfaces:**
- Consumes: `SystemInfo` from Task 1, `AppState.system` from Task 2.
- Produces: `POST /api/system/power` accepting
  `{"action": "poweroff" | "reboot" | "restart-service"}`; the SPA (Task 7)
  posts exactly those three strings.

**Why the two injection points below exist:** without them these routes
cannot be tested at all — a test that really ran `systemctl poweroff` would
shut down the machine running the suite, and one that really called
`std::process::exit` would kill the test binary. They are the only
abstraction in the module, and the code says why.

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` of `system.rs`:

```rust
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
```

- [ ] **Step 2: Run them to verify they fail**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test -p ritornello-core system"`
Expected: compilation failure — `PowerAction`, `parse_action`, `parse_can`
and the three new `SystemInfo` fields do not exist.

- [ ] **Step 3: Extend `SystemInfo`**

Replace the `SystemInfo` struct and its `Default` impl in `system.rs` with:

```rust
/// Called to restart the service. A field rather than a direct
/// `std::process::exit(0)` **so the route can be tested**: a test that
/// really exited would kill the test binary.
pub type RestartHook = Arc<dyn Fn() + Send + Sync>;

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
```

`#[derive(Debug)]` is dropped: `Arc<dyn Fn()>` is not `Debug`. Add
`use std::sync::Arc;` to the module's imports if it is not there yet.

- [ ] **Step 4: Write the actions and the probe**

Add to `system.rs`:

```rust
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

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
pub async fn probe_capabilities() -> (bool, bool) {
    (interroge_logind("CanPowerOff").await, interroge_logind("CanReboot").await)
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
            tracing::info!("busctl indisponible ({e}): arrêt et redémarrage désactivés dans l'IHM");
            false
        }
        Err(_) => {
            tracing::info!("logind {methode}: pas de réponse en 3 s");
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
            tracing::warn!("redémarrage du service demandé depuis l'IHM");
            let info = state.system.clone();
            tokio::spawn(async move {
                tokio::time::sleep(info.restart_delay).await;
                (info.restart)();
            });
            return StatusCode::ACCEPTED.into_response();
        }
    };
    tracing::warn!("{verbe} de l'OS demandé depuis l'IHM");
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
                stderr
            };
            tracing::warn!("{verbe} refusé: {msg}");
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": msg }))).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("systemctl injoignable: {e}") })),
        )
            .into_response(),
    }
}
```

- [ ] **Step 5: Register the route and probe at startup**

In `status.rs`'s `router`, right after the `/api/system` line:

```rust
        .route("/api/system/power", axum::routing::post(crate::system::power_post))
```

In `main.rs`, replace the `system:` field added in Task 2 with a probed
value. Immediately **before** the `let app = status::router(AppState {`
line, insert:

```rust
        // Asked once, before serving: the answer gates the System tab's two
        // OS buttons, and asking per request would mean spawning `busctl`
        // twice every five seconds.
        let (can_power_off, can_reboot) = system::probe_capabilities().await;
```

and make the field:

```rust
            system: Arc::new(system::SystemInfo {
                can_power_off,
                can_reboot,
                ..Default::default()
            }),
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo test -p ritornello-core system"`
Expected: the seven new tests pass. **If any test hangs or the runner
disappears, stop and report** — that would mean the injection points are
not being used and a real power action ran.

- [ ] **Step 7: Full suite, clippy, commit**

```
git add crates/ritornello-core/src
git commit -m "feat(core): POST /api/system/power (arrêt, redémarrage, relance du service)"
```

---

### Task 4: polkit rule and its installation

**Files:**
- Create: `deploy/50-ritornello-power.rules`
- Modify: `deploy/deploy.sh`

**Interfaces:**
- Consumes: nothing. Produces the authorisation that makes
  `POST /api/system/power` succeed on a real device.

No automated test covers this task (it is a shell script and a JS policy
file installed on a remote machine); verification is a syntax check plus a
read-back of the two edits.

- [ ] **Step 1: Write the rule**

Create `deploy/50-ritornello-power.rules`:

```javascript
// Ritornello's System tab offers "shut down" and "restart" for the whole
// machine. The service runs as the unprivileged `ritornello` user with
// NoNewPrivileges=true, so `sudo` and any setuid path are structurally
// unavailable: logind through polkit is the mechanism, and this file is the
// authorisation.
//
// Three actions per verb, not one. logind checks the plain action only when
// nothing else is going on: it switches to `*-multiple-sessions` as soon as
// another session exists — an open SSH connection is enough, which is
// exactly the situation while testing this — and to `*-ignore-inhibit` when
// an inhibitor is held. Granting only the plain action produces "it works,
// except when I am logged in over SSH", a symptom that costs an hour to
// attribute.
//
// Nothing else is granted: not `manage-units`, because restarting
// Ritornello itself needs no privilege (the process exits and systemd
// restarts it, the unit saying Restart=always).
polkit.addRule(function (action, subject) {
  var autorisees = [
    "org.freedesktop.login1.power-off",
    "org.freedesktop.login1.power-off-multiple-sessions",
    "org.freedesktop.login1.power-off-ignore-inhibit",
    "org.freedesktop.login1.reboot",
    "org.freedesktop.login1.reboot-multiple-sessions",
    "org.freedesktop.login1.reboot-ignore-inhibit",
  ];
  if (subject.user === "ritornello" && autorisees.indexOf(action.id) !== -1) {
    return polkit.Result.YES;
  }
});
```

- [ ] **Step 2: Ship it in `deploy.sh`**

In `deploy/deploy.sh`, extend the line that copies the unit:

```bash
scp "${SSHOPTS[@]}" deploy/ritornello.service deploy/50-ritornello-power.rules "$PI:/tmp/"
```

and, in the final `ssh … "sudo mv /tmp/ritornello-core …"` chain, insert
right after the `sudo mv /tmp/ritornello.service /etc/systemd/system/ \`
line:

```bash
  && sudo mkdir -p /etc/polkit-1/rules.d \
  && sudo mv /tmp/50-ritornello-power.rules /etc/polkit-1/rules.d/ \
  && sudo chown root: /etc/polkit-1/rules.d/50-ritornello-power.rules \
  && sudo chmod 644 /etc/polkit-1/rules.d/50-ritornello-power.rules \
```

`mkdir -p` first: the rule must land even on a machine where polkit is not
installed yet, so that installing polkit later is enough. The file is
always overwritten — it is ours, like the unit.

- [ ] **Step 3: Check the script still parses**

Run: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && bash -n deploy/deploy.sh && grep -n 'polkit' deploy/deploy.sh"`
Expected: no output from `bash -n`, and the five new lines listed by grep.

- [ ] **Step 4: Commit**

```
git add deploy/50-ritornello-power.rules deploy/deploy.sh
git commit -m "feat(deploy): règle polkit d'arrêt et de redémarrage installée par deploy.sh"
```

---

### Task 5: Sparkline path builder

**Files:**
- Create: `web/app/src/views/sparkline.ts`, `web/app/src/views/sparkline.test.ts`

**Interfaces:**
- Produces: `cheminSparkline(valeurs: number[], largeur: number, hauteur: number): string`,
  consumed by `SystemView.vue` in Task 6 with `largeur = 100`,
  `hauteur = 30` (the SVG `viewBox` units).

TDD. Run `npm install` at the worktree root first if `node_modules` is
absent.

- [ ] **Step 1: Write the failing test**

Create `web/app/src/views/sparkline.test.ts`:

```ts
import { describe, expect, it } from 'vitest'
import { cheminSparkline } from './sparkline'

describe('cheminSparkline', () => {
  it('ne dessine rien avec moins de deux points', () => {
    expect(cheminSparkline([], 100, 30)).toBe('')
    expect(cheminSparkline([42], 100, 30)).toBe('')
  })

  it('inverse l axe y : 0 % en bas, 100 % en haut', () => {
    expect(cheminSparkline([0, 100], 100, 30)).toBe('M0.00,30.00 L100.00,0.00')
  })

  it('borne les valeurs hors de 0-100', () => {
    // Une charge supérieure au nombre de cœurs dépasse 100 % et ne doit pas
    // sortir du cadre.
    expect(cheminSparkline([-10, 200], 100, 30)).toBe(cheminSparkline([0, 100], 100, 30))
  })

  it('repartit les points sur toute la largeur', () => {
    expect(cheminSparkline([0, 50, 100], 100, 30)).toBe('M0.00,30.00 L50.00,15.00 L100.00,0.00')
  })
})
```

- [ ] **Step 2: Run it to verify it fails**

Run: `npm test -w app -- sparkline`
Expected: failure — the module does not exist.

- [ ] **Step 3: Write the module**

Create `web/app/src/views/sparkline.ts`:

```ts
/**
 * Construit l'attribut `d` d'un `<path>` SVG pour une série de pourcentages.
 *
 * Toute la géométrie du graphe tient ici, en fonction pure et testée : la
 * vue n'a plus qu'à passer ses deux séries.
 *
 * Les valeurs sont bornées à 0-100 — une charge supérieure au nombre de
 * cœurs dépasse 100 % et ne doit pas sortir du cadre — et l'axe y est
 * inversé : 0 % en bas, comme on lit un graphe, alors que le repère SVG a
 * son origine en haut.
 *
 * Moins de deux points : chaîne vide. Un échantillon seul ne dessine pas de
 * ligne, et un `d` vide est un `<path>` invisible, pas une erreur.
 */
export function cheminSparkline(valeurs: number[], largeur: number, hauteur: number): string {
  if (valeurs.length < 2) return ''
  const pas = largeur / (valeurs.length - 1)
  return valeurs
    .map((v, i) => {
      const borne = Math.min(100, Math.max(0, v))
      const x = i * pas
      const y = hauteur - (borne / 100) * hauteur
      return `${i === 0 ? 'M' : 'L'}${x.toFixed(2)},${y.toFixed(2)}`
    })
    .join(' ')
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `npm test -w app -- sparkline`
Expected: 4 tests pass.

- [ ] **Step 5: Typecheck and commit**

Run: `npm run typecheck -w app`

```
git add web/app/src/views/sparkline.ts web/app/src/views/sparkline.test.ts
git commit -m "feat(web): générateur de tracé sparkline, borné et testé"
```

---

### Task 6: System tab — metrics, polling, history

**Files:**
- Create: `web/app/src/views/SystemView.vue`, `web/app/src/views/SystemView.test.ts`
- Modify: `web/app/src/types.ts`, `web/app/src/router.ts`, `web/app/src/App.vue`,
  `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`

**Interfaces:**
- Consumes: `GET /api/system` (Task 2), `cheminSparkline` (Task 5).
- Produces: the view file that Task 7 extends with the power card, and
  **all** `system_*` i18n keys — including the ones only Task 7 uses.
  Adding them in one pass is deliberate: a second pass over the same two
  catalogue files for the same feature would be pure churn.

- [ ] **Step 1: Add the payload types**

Append to `web/app/src/types.ts`:

```ts
export interface SystemUsage { total_kb: number; available_kb: number }
/**
 * Metriques de l'OS, telles que les sert `GET /api/system`.
 *
 * Tout champ que la machine n'expose pas vaut `null` — pas de capteur
 * thermique, pas de cpufreq, pas de sonde de sous-tension — et la vue
 * affiche « — » sans traiter cela comme une panne. Le jeu de cles, lui, est
 * stable.
 */
export interface SystemPayload {
  temperature_c: number | null
  cpu_mhz: number | null
  load: [number, number, number] | null
  cpus: number | null
  memory: SystemUsage | null
  disk: SystemUsage | null
  under_voltage: boolean | null
  uptime_s: number | null
  service_uptime_s: number
  hostname: string | null
  ip: string | null
  os: string | null
  kernel: string | null
  version: string
  can_power_off: boolean
  can_reboot: boolean
}
```

- [ ] **Step 2: Add the i18n keys**

Append to `crates/ritornello-core/src/locales/en.toml`:

```toml
system_title = "System"
system_load = "Load"
system_temperature = "Temperature"
system_frequency = "CPU frequency"
system_loadavg = "Load average (1/5/15 min)"
system_cores = "Cores"
system_history = "History"
system_history_empty = "The graph fills up as the page refreshes."
system_memory = "Memory"
system_storage = "Storage"
system_device = "Device"
system_hostname = "Host name"
system_ip = "IP address"
system_os = "Operating system"
system_kernel = "Kernel"
system_version = "Ritornello version"
system_uptime = "OS uptime"
system_service_uptime = "Service uptime"
system_power = "Power"
system_poweroff = "Shut down the system"
system_reboot = "Restart the system"
system_restart_service = "Restart Ritornello"
system_confirm_poweroff = "Playback stops and the device switches off. Turning it back on needs physical access."
system_confirm_reboot = "Playback stops; the device restarts on its own."
system_confirm_restart_service = "Playback stops for a few seconds. The device stays on."
system_confirm = "Confirm"
system_cancel = "Cancel"
system_power_unavailable = "Shutdown and restart are unavailable: the polkit rule is not installed (see docs/installation.md)."
system_under_voltage = "Undervoltage detected: check the power supply."
system_powering_off = "The device is switching off."
system_rebooting = "The device is restarting."
system_restarting = "Ritornello is restarting…"
system_restarted = "Ritornello has restarted."
system_restart_timeout = "Ritornello is not answering — check journalctl -u ritornello."
system_unavailable = "System metrics unavailable."
system_unit_mb = "MB"
system_unit_gb = "GB"
system_unit_day = "d"
system_unit_hour = "h"
system_unit_minute = "min"
```

Append to `deploy/locales/core/fr.toml`:

```toml
system_title = "Système"
system_load = "Charge"
system_temperature = "Température"
system_frequency = "Fréquence CPU"
system_loadavg = "Charge moyenne (1/5/15 min)"
system_cores = "Cœurs"
system_history = "Historique"
system_history_empty = "Le graphe se remplit à mesure que la page se rafraîchit."
system_memory = "Mémoire"
system_storage = "Stockage"
system_device = "Appareil"
system_hostname = "Nom d'hôte"
system_ip = "Adresse IP"
system_os = "Système d'exploitation"
system_kernel = "Noyau"
system_version = "Version de Ritornello"
system_uptime = "Uptime de l'OS"
system_service_uptime = "Uptime du service"
system_power = "Alimentation"
system_poweroff = "Éteindre le système"
system_reboot = "Redémarrer le système"
system_restart_service = "Redémarrer Ritornello"
system_confirm_poweroff = "La lecture s'arrête et l'appareil s'éteint. Le rallumer demande un accès physique."
system_confirm_reboot = "La lecture s'arrête ; l'appareil redémarre tout seul."
system_confirm_restart_service = "La lecture s'arrête quelques secondes. L'appareil reste allumé."
system_confirm = "Confirmer"
system_cancel = "Annuler"
system_power_unavailable = "Arrêt et redémarrage indisponibles : la règle polkit n'est pas installée (voir docs/installation.md)."
system_under_voltage = "Sous-tension détectée : vérifier l'alimentation."
system_powering_off = "L'appareil s'éteint."
system_rebooting = "L'appareil redémarre."
system_restarting = "Ritornello redémarre…"
system_restarted = "Ritornello a redémarré."
system_restart_timeout = "Ritornello ne répond pas — voir journalctl -u ritornello."
system_unavailable = "Métriques système indisponibles."
system_unit_mb = "Mo"
system_unit_gb = "Go"
system_unit_day = "j"
system_unit_hour = "h"
system_unit_minute = "min"
```

- [ ] **Step 3: Route and nav link**

In `web/app/src/router.ts`, after the `/config` route and before the
`/status` redirect:

```ts
    { path: '/system', name: 'system', component: () => import('./views/SystemView.vue') },
```

In `web/app/src/App.vue`, after the `/config` `RouterLink`:

```html
        <RouterLink to="/system" class="text-sm text-muted-foreground">{{ t('system_title') }}</RouterLink>
```

- [ ] **Step 4: Write the failing view test**

Create `web/app/src/views/SystemView.test.ts`:

```ts
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import SystemView from './SystemView.vue'

// Charge utile complète, réutilisée en la modifiant par cas.
function payload(surcharge: Record<string, unknown> = {}) {
  return {
    temperature_c: 47.8,
    cpu_mhz: 900,
    load: [0.5, 0.4, 0.3],
    cpus: 4,
    memory: { total_kb: 1_000_000, available_kb: 400_000 },
    disk: { total_kb: 30_000_000, available_kb: 24_000_000 },
    under_voltage: false,
    uptime_s: 90_061,
    service_uptime_s: 3_600,
    hostname: 'ritornello',
    ip: '192.168.1.20',
    os: 'Debian GNU/Linux 12 (bookworm)',
    kernel: '6.6.51+rpt-rpi-v7',
    version: '0.1.0',
    can_power_off: true,
    can_reboot: true,
    ...surcharge,
  }
}

/** Catalogue minimal : seules les unités sont assertées à l'affichage. */
const CATALOGUE = {
  system_unit_mb: 'Mo',
  system_unit_gb: 'Go',
  system_unit_day: 'j',
  system_unit_hour: 'h',
  system_unit_minute: 'min',
}

/**
 * Stub de `fetch` qui répond selon l'URL : le catalogue i18n d'un côté,
 * `/api/system` de l'autre, `{}` pour les POST. `corps` accepte une fonction,
 * appelée à chaque sondage, pour faire varier les réponses successives.
 *
 * Le catalogue est bel et bien servi : sans lui, `createT` renvoie la clé
 * elle-même et les unités s'afficheraient « system_unit_day ». Le test
 * vérifierait alors le repli, pas la vue.
 */
function stub(corps: unknown | (() => unknown), catalogue: Record<string, string> = CATALOGUE) {
  const f = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
    if (init?.method === 'POST') {
      return Promise.resolve({ ok: true, json: async () => ({}) } as Response)
    }
    const j = String(url).includes('/api/i18n')
      ? catalogue
      : typeof corps === 'function'
        ? (corps as () => unknown)()
        : corps
    return Promise.resolve({ ok: true, json: async () => j } as Response)
  })
  vi.stubGlobal('fetch', f)
  return f
}

describe('SystemView', () => {
  beforeEach(() => vi.useFakeTimers({ shouldAdvanceTime: true }))
  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    // Les dialogues sont montés dans un portail : sans ce nettoyage, le DOM
    // d'un test fuiterait dans les `document.body.querySelector` du suivant.
    document.body.innerHTML = ''
  })

  /**
   * Charge le catalogue puis monte la vue — c'est l'ordre de l'application,
   * `App.vue` rechargeant le catalogue au montage. `attachTo` est nécessaire
   * aux tests du dialogue et inoffensif pour les autres.
   */
  async function monter() {
    await useCatalog().reload()
    const w = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    return w
  }

  it('affiche les métriques du premier sondage', async () => {
    stub(payload())
    const w = await monter()
    expect(w.get('[data-system-temperature]').text()).toContain('47.8')
    expect(w.get('[data-system-frequency]').text()).toContain('900')
    expect(w.get('[data-system-cores]').text()).toBe('4')
    expect(w.get('[data-system-hostname]').text()).toBe('ritornello')
    expect(w.get('[data-system-kernel]').text()).toBe('6.6.51+rpt-rpi-v7')
    // 90 061 s = 1 jour 1 heure, au plus deux unités.
    expect(w.get('[data-system-uptime]').text()).toBe('1 j 1 h')
    // 600 000 kio utilisés sur 1 000 000, arrondis en Mo.
    expect(w.get('[data-system-memory]').text()).toBe('586 / 977 Mo')
    w.unmount()
  })

  it('affiche un tiret pour ce que la machine n expose pas', async () => {
    stub(payload({ temperature_c: null, cpu_mhz: null, ip: null }))
    const w = await monter()
    expect(w.get('[data-system-temperature]').text()).toBe('—')
    expect(w.get('[data-system-frequency]').text()).toBe('—')
    expect(w.get('[data-system-ip]').text()).toBe('—')
    w.unmount()
  })

  it('signale un cœur injoignable sans vider la page', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('réseau')))
    const w = await monter()
    expect(w.find('[data-system-unavailable]').exists()).toBe(true)
    w.unmount()
  })

  it('arrive avec un historique vide et le remplit au fil des sondages', async () => {
    stub(payload())
    const w = await monter()
    // Un seul échantillon : pas de ligne, le message d'attente à la place.
    expect(w.find('[data-system-history-empty]').exists()).toBe(true)
    expect(w.find('[data-system-history]').exists()).toBe(false)
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(w.find('[data-system-history]').exists()).toBe(true)
    expect(w.get('[data-system-history]').html()).toContain('M0.00,')
    w.unmount()
  })

  it('arrête de sonder au démontage', async () => {
    const f = stub(payload())
    const w = await monter()
    const appels = f.mock.calls.length
    w.unmount()
    await vi.advanceTimersByTimeAsync(15000)
    expect(f.mock.calls.length).toBe(appels)
  })
})
```

Add the import the helper needs, next to the others:

```ts
import { useCatalog } from '../composables/useCatalog'
```

- [ ] **Step 5: Run it to verify it fails**

Run: `npm test -w app -- SystemView`
Expected: failure — the view does not exist.

- [ ] **Step 6: Write the view**

Create `web/app/src/views/SystemView.vue`:

```vue
<script setup lang="ts">
import { api, Card, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { SystemPayload, SystemUsage } from '../types'
import { cheminSparkline } from './sparkline'

const { t } = useCatalog()
const etat = ref<SystemPayload | null>(null)
const indisponible = ref(false)

/** Période de sondage. Voir `sonder` pour le choix du sondage. */
const PERIODE_MS = 5000
/** Cinq minutes d'historique à cette période. */
const CAPACITE = 60
/** Repère du graphe, en unités de `viewBox`. */
const LARGEUR = 100
const HAUTEUR = 30
/** Valeur que la machine n'expose pas : un tiret cadratin plutôt qu'un 0,
 *  qui se lirait comme une mesure. */
const RIEN = '—'

const historique = ref<{ cpu: number; ram: number }[]>([])
let minuteur: ReturnType<typeof setInterval> | null = null

/**
 * Pourcentages retenus dans l'historique. `null` si l'un des deux manque :
 * une machine sans loadavg garde un graphe vide plutôt qu'à moitié tracé.
 */
function pourcentages(s: SystemPayload): { cpu: number; ram: number } | null {
  if (!s.load || !s.cpus || !s.memory || s.memory.total_kb === 0) return null
  return {
    cpu: Math.min(100, (s.load[0] / s.cpus) * 100),
    ram: ((s.memory.total_kb - s.memory.available_kb) / s.memory.total_kb) * 100,
  }
}

/**
 * Sondage, là où le reste de la SPA reçoit du SSE, et c'est délibéré : le
 * flux `/api/player` publie un état que le cœur produit de toute façon,
 * alors que ces métriques n'existent que parce qu'on les demande. Les
 * pousser ferait travailler en permanence un appareil le plus souvent
 * inactif, pour personne. Le sondage s'arrête donc au démontage de la vue
 * et quand l'onglet passe en arrière-plan.
 *
 * Un échec n'affiche pas de toast : répété toutes les 5 secondes, un cœur
 * injoignable en produirait un flot. Une ligne de diagnostic suffit, comme
 * le drapeau `audioIndisponible` de la page de configuration.
 */
async function sonder() {
  try {
    const s = await api.get<SystemPayload>('/api/system')
    etat.value = s
    indisponible.value = false
    const p = pourcentages(s)
    if (p) {
      historique.value.push(p)
      if (historique.value.length > CAPACITE) historique.value.shift()
    }
  } catch (e) {
    indisponible.value = true
    console.warn('GET /api/system indisponible', e)
  }
}

function demarrer() {
  if (minuteur !== null) return
  void sonder()
  minuteur = setInterval(sonder, PERIODE_MS)
}

function arreter() {
  if (minuteur !== null) {
    clearInterval(minuteur)
    minuteur = null
  }
}

function visibilite() {
  if (document.hidden) arreter()
  else demarrer()
}

onMounted(() => {
  demarrer()
  document.addEventListener('visibilitychange', visibilite)
})
onUnmounted(() => {
  arreter()
  document.removeEventListener('visibilitychange', visibilite)
})

// « °C » et « MHz » ne sont pas traduits : ce sont des symboles SI,
// identiques dans les deux langues — contrairement à Mo/MB et j/d.
const temperature = computed(() =>
  etat.value?.temperature_c == null ? RIEN : `${etat.value.temperature_c.toFixed(1)} °C`,
)
const frequence = computed(() =>
  etat.value?.cpu_mhz == null ? RIEN : `${etat.value.cpu_mhz} MHz`,
)
const charge = computed(() =>
  etat.value?.load ? etat.value.load.map((v) => v.toFixed(2)).join(' · ') : RIEN,
)
const dernier = computed(() => historique.value.at(-1) ?? null)
const cheminCpu = computed(() =>
  cheminSparkline(historique.value.map((h) => h.cpu), LARGEUR, HAUTEUR),
)
const cheminRam = computed(() =>
  cheminSparkline(historique.value.map((h) => h.ram), LARGEUR, HAUTEUR),
)

function texte(v: string | null | undefined): string {
  return v || RIEN
}

function nombre(v: number | null | undefined): string {
  return v == null ? RIEN : String(v)
}

/** « 512 / 976 Mo » : utilisé et total dans la même unité, traduite. */
function occupation(u: SystemUsage | null | undefined, unite: 'mb' | 'gb'): string {
  if (!u) return RIEN
  const diviseur = unite === 'mb' ? 1024 : 1024 * 1024
  const chiffre = (kb: number) =>
    unite === 'mb' ? String(Math.round(kb / diviseur)) : (kb / diviseur).toFixed(1)
  const suffixe = t.value(unite === 'mb' ? 'system_unit_mb' : 'system_unit_gb')
  return `${chiffre(u.total_kb - u.available_kb)} / ${chiffre(u.total_kb)} ${suffixe}`
}

function pourcentOccupe(u: SystemUsage | null | undefined): number {
  if (!u || u.total_kb === 0) return 0
  return Math.round(((u.total_kb - u.available_kb) / u.total_kb) * 100)
}

/** Au plus deux unités : « 3 j 4 h », « 4 h 12 min », « 12 min ». */
function duree(secondes: number | null | undefined): string {
  if (secondes == null) return RIEN
  const j = Math.floor(secondes / 86400)
  const h = Math.floor((secondes % 86400) / 3600)
  const m = Math.floor((secondes % 3600) / 60)
  const jour = t.value('system_unit_day')
  const heure = t.value('system_unit_hour')
  const minute = t.value('system_unit_minute')
  if (j > 0) return `${j} ${jour} ${h} ${heure}`
  if (h > 0) return `${h} ${heure} ${m} ${minute}`
  return `${m} ${minute}`
}
</script>

<template>
  <div class="space-y-4">
    <p v-if="indisponible" data-system-unavailable class="text-sm text-destructive">
      {{ t('system_unavailable') }}
    </p>

    <Card>
      <CardHeader><CardTitle>{{ t('system_load') }}</CardTitle></CardHeader>
      <CardContent class="grid gap-2 text-sm sm:grid-cols-2">
        <div>{{ t('system_temperature') }} : <span data-system-temperature>{{ temperature }}</span></div>
        <div>{{ t('system_frequency') }} : <span data-system-frequency>{{ frequence }}</span></div>
        <div>{{ t('system_loadavg') }} : <span data-system-load>{{ charge }}</span></div>
        <div>{{ t('system_cores') }} : <span data-system-cores>{{ nombre(etat?.cpus) }}</span></div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_history') }}</CardTitle></CardHeader>
      <CardContent>
        <p
          v-if="historique.length < 2"
          data-system-history-empty
          class="text-sm text-muted-foreground"
        >
          {{ t('system_history_empty') }}
        </p>
        <template v-else>
          <!-- `preserveAspectRatio="none"` étire le repère à la largeur
               disponible ; `vector-effect` empêche l'épaisseur du trait
               d'être étirée avec lui. -->
          <svg
            data-system-history
            :viewBox="`0 0 ${LARGEUR} ${HAUTEUR}`"
            preserveAspectRatio="none"
            class="h-24 w-full"
            role="img"
            :aria-label="t('system_history')"
          >
            <path
              :d="cheminCpu"
              class="text-primary"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
            <path
              :d="cheminRam"
              class="text-muted-foreground"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
          </svg>
          <p class="mt-2 flex gap-4 text-xs">
            <span class="text-primary">
              {{ t('system_load') }} {{ dernier ? Math.round(dernier.cpu) : 0 }} %
            </span>
            <span class="text-muted-foreground">
              {{ t('system_memory') }} {{ dernier ? Math.round(dernier.ram) : 0 }} %
            </span>
          </p>
        </template>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_memory') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-memory>{{ occupation(etat?.memory, 'mb') }}</div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${pourcentOccupe(etat?.memory)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_storage') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2 text-sm">
        <div data-system-disk>{{ occupation(etat?.disk, 'gb') }}</div>
        <div class="h-2 w-full rounded bg-muted">
          <div class="h-2 rounded bg-primary" :style="{ width: `${pourcentOccupe(etat?.disk)}%` }" />
        </div>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('system_device') }}</CardTitle></CardHeader>
      <CardContent class="grid gap-2 text-sm sm:grid-cols-2">
        <div>{{ t('system_hostname') }} : <span data-system-hostname>{{ texte(etat?.hostname) }}</span></div>
        <div>{{ t('system_ip') }} : <span data-system-ip>{{ texte(etat?.ip) }}</span></div>
        <div>{{ t('system_os') }} : <span data-system-os>{{ texte(etat?.os) }}</span></div>
        <div>{{ t('system_kernel') }} : <span data-system-kernel>{{ texte(etat?.kernel) }}</span></div>
        <div>{{ t('system_version') }} : <span data-system-version>{{ texte(etat?.version) }}</span></div>
        <div>{{ t('system_uptime') }} : <span data-system-uptime>{{ duree(etat?.uptime_s) }}</span></div>
        <div>
          {{ t('system_service_uptime') }} :
          <span data-system-service-uptime>{{ duree(etat?.service_uptime_s) }}</span>
        </div>
      </CardContent>
    </Card>
  </div>
</template>
```

- [ ] **Step 7: Run the web tests**

Run: `npm test -w app` then `npm run typecheck -w app`
Expected: the 5 new tests pass, the i18n guard passes (it now sees the
`system_*` keys), nothing else regresses.

- [ ] **Step 8: Commit**

```
git add web/app/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web): onglet Système — métriques, sondage et historique local"
```

---

### Task 7: Power card

**Files:**
- Modify: `web/app/src/views/SystemView.vue`, `web/app/src/views/SystemView.test.ts`

**Interfaces:**
- Consumes: `POST /api/system/power` (Task 3), the i18n keys added in Task 6,
  and the view's `etat` / `arreter` / `demarrer` / `sonder`.
- Produces: nothing for later tasks; Task 8 only asserts that the buttons
  are present.

**Hard rule:** no test may confirm an action against a real core. These
tests stub `fetch`; the e2e task never clicks the confirm button.

- [ ] **Step 1: Write the failing tests**

Append inside the `describe('SystemView', …)` of `SystemView.test.ts`:

```ts
  it('désactive les boutons système quand polkit n est pas configuré', async () => {
    stub(payload({ can_power_off: false, can_reboot: false }))
    const w = await monter()
    expect(w.get('[data-power-poweroff]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-power-reboot]').attributes('disabled')).toBeDefined()
    // Le redémarrage du service ne dépend d'aucune autorisation.
    expect(w.get('[data-power-restart]').attributes('disabled')).toBeUndefined()
    expect(w.find('[data-power-unavailable]').exists()).toBe(true)
    w.unmount()
  })

  it('n envoie rien avant confirmation', async () => {
    const f = stub(payload())
    const w = await monter()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    // Le dialogue est monté dans un portail : il vit dans document.body.
    expect(document.body.querySelector('[data-power-confirm]')).not.toBeNull()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    document.body.querySelector<HTMLElement>('[data-power-cancel]')!.click()
    await flushPromises()
    expect(f.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(false)
    w.unmount()
  })

  it('poste l action confirmée puis annonce l arrêt et cesse de sonder', async () => {
    const f = stub(payload())
    const w = await monter()
    await w.get('[data-power-poweroff]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    const poste = f.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'POST')
    expect(poste).toBeDefined()
    expect(poste?.[0]).toBe('/api/system/power')
    expect(JSON.parse(String((poste?.[1] as RequestInit).body))).toEqual({ action: 'poweroff' })
    expect(w.find('[data-power-progress]').exists()).toBe(true)
    // Le cœur s'en va : plus aucun sondage, sans quoi la page afficherait
    // une erreur réseau alors que tout se passe comme demandé.
    const appels = f.mock.calls.length
    await vi.advanceTimersByTimeAsync(15000)
    expect(f.mock.calls.length).toBe(appels)
    w.unmount()
  })

  it('reprend la main quand le redémarrage du service aboutit', async () => {
    // Uptime décroissant : le service est bien revenu, ce qu'une simple
    // réponse ne prouverait pas (le premier sondage peut encore atteindre
    // l'ancien process).
    const reponses = [payload(), payload(), payload({ service_uptime_s: 2 })]
    let i = 0
    stub(() => reponses[Math.min(i++, reponses.length - 1)])
    const w = await monter()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    expect(w.get('[data-power-progress]').text()).toBeTruthy()
    await vi.advanceTimersByTimeAsync(6000)
    await flushPromises()
    expect(w.find('[data-power-progress]').exists()).toBe(false)
    w.unmount()
  })
```

- [ ] **Step 2: Run them to verify they fail**

Run: `npm test -w app -- SystemView`
Expected: failures — no `[data-power-*]` element exists.

- [ ] **Step 3: Extend the view's script**

Add to the imports of `SystemView.vue`:

```ts
import {
  api, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent,
  DialogDescription, DialogHeader, DialogTitle, toast,
} from '@ritornello/ui'
```

and after the metric helpers:

```ts
type ActionPower = 'poweroff' | 'reboot' | 'restart-service'

/** Sondage rapproché pendant le redémarrage du service, et son plafond. */
const REPRISE_MS = 2000
const REPRISE_MAX_MS = 30000

/** Action dont on attend la confirmation, et action en cours. */
const dialogue = ref<ActionPower | null>(null)
const enCours = ref<ActionPower | null>(null)

function libelle(a: ActionPower): string {
  if (a === 'poweroff') return t.value('system_poweroff')
  if (a === 'reboot') return t.value('system_reboot')
  return t.value('system_restart_service')
}

function consequence(a: ActionPower): string {
  if (a === 'poweroff') return t.value('system_confirm_poweroff')
  if (a === 'reboot') return t.value('system_confirm_reboot')
  return t.value('system_confirm_restart_service')
}

const messageEnCours = computed(() => {
  if (enCours.value === 'poweroff') return t.value('system_powering_off')
  if (enCours.value === 'reboot') return t.value('system_rebooting')
  if (enCours.value === 'restart-service') return t.value('system_restarting')
  return ''
})

/**
 * Le cœur va disparaître : le sondage normal s'arrête avant l'envoi. Sans
 * cela, le sondage suivant échouerait et afficherait une erreur réseau
 * alarmante alors que l'arrêt se passe exactement comme demandé.
 */
async function confirmer() {
  const action = dialogue.value
  if (!action) return
  dialogue.value = null
  enCours.value = action
  arreter()
  const uptimeAvant = etat.value?.service_uptime_s ?? null
  const err = await api.post('/api/system/power', { action })
  if (err) {
    // Refus de logind (règle polkit absente) ou cœur injoignable : rien ne
    // s'arrête, on rend la main.
    toast.error(err)
    enCours.value = null
    demarrer()
    return
  }
  if (action === 'restart-service') await attendreRetour(uptimeAvant)
}

/**
 * Le service redémarre : on sonde plus vite en ignorant les erreurs (il est
 * arrêté, c'est attendu), et on ne le considère revenu que lorsque son
 * uptime a *diminué* — une réponse suffirait à se tromper, le premier
 * sondage pouvant encore atteindre l'ancien process.
 */
async function attendreRetour(avant: number | null) {
  const limite = Date.now() + REPRISE_MAX_MS
  while (Date.now() < limite) {
    await new Promise((r) => setTimeout(r, REPRISE_MS))
    try {
      const s = await api.get<SystemPayload>('/api/system')
      if (avant === null || s.service_uptime_s < avant) {
        etat.value = s
        enCours.value = null
        toast.success(t.value('system_restarted'))
        demarrer()
        return
      }
    } catch {
      // Service arrêté : on réessaie jusqu'au plafond.
    }
  }
  toast.error(t.value('system_restart_timeout'))
  enCours.value = null
  demarrer()
}
```

- [ ] **Step 4: Extend the template**

Add, as the last card inside the root `<div class="space-y-4">`:

```html
    <Card>
      <CardHeader><CardTitle>{{ t('system_power') }}</CardTitle></CardHeader>
      <CardContent class="space-y-3">
        <p v-if="etat?.under_voltage" data-system-under-voltage class="text-sm text-destructive">
          {{ t('system_under_voltage') }}
        </p>
        <p v-if="enCours" data-power-progress class="text-sm text-muted-foreground">
          {{ messageEnCours }}
        </p>
        <p
          v-else-if="etat && (!etat.can_power_off || !etat.can_reboot)"
          data-power-unavailable
          class="text-sm text-muted-foreground"
        >
          {{ t('system_power_unavailable') }}
        </p>
        <div class="flex flex-wrap gap-2">
          <Button
            variant="destructive"
            data-power-poweroff
            :disabled="!!enCours || !etat?.can_power_off"
            @click="dialogue = 'poweroff'"
          >
            {{ t('system_poweroff') }}
          </Button>
          <Button
            variant="destructive"
            data-power-reboot
            :disabled="!!enCours || !etat?.can_reboot"
            @click="dialogue = 'reboot'"
          >
            {{ t('system_reboot') }}
          </Button>
          <Button
            variant="outline"
            data-power-restart
            :disabled="!!enCours"
            @click="dialogue = 'restart-service'"
          >
            {{ t('system_restart_service') }}
          </Button>
        </div>
      </CardContent>
    </Card>

    <!-- Un seul dialogue pour les trois actions : le titre et la phrase de
         conséquence viennent de l'action en attente. -->
    <Dialog
      :open="dialogue !== null"
      @update:open="(ouvert: boolean) => { if (!ouvert) dialogue = null }"
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ dialogue ? libelle(dialogue) : '' }}</DialogTitle>
          <DialogDescription>{{ dialogue ? consequence(dialogue) : '' }}</DialogDescription>
        </DialogHeader>
        <div class="flex justify-end gap-2">
          <Button variant="outline" data-power-cancel @click="dialogue = null">
            {{ t('system_cancel') }}
          </Button>
          <Button variant="destructive" data-power-confirm @click="confirmer">
            {{ t('system_confirm') }}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
```

Note: `DialogFooter` is **not** exported by `@ritornello/ui` — hence the
plain `<div class="flex justify-end gap-2">`. Do not add an export to the
kit for this.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `npm test -w app -- SystemView` then `npm run typecheck -w app`
Expected: all 9 tests of the file pass.

If the portal-rendered dialog proves undrivable under jsdom (reka-ui
mounting outside the wrapper in a way `document.body` does not see), stop
and report it as a concern with what you observed — do not replace the
assertions with a check on internal state, and do not drop the dialog.

- [ ] **Step 6: Commit**

```
git add web/app/src/views
git commit -m "feat(web): carte Alimentation — arrêt, redémarrage et relance du service"
```

---

### Task 8: Documentation and e2e journey

**Files:**
- Modify: `docs/installation.md`, `docs/interface.md`,
  `web/app/e2e/parcours.spec.ts`

**Interfaces:**
- Consumes everything built above. Produces no code interface.

- [ ] **Step 1: Document the prerequisite in `installation.md`**

In the "Unprivileged service" access table, add a row after the
HDMI console one:

```markdown
| OS shutdown / reboot | polkit rule + logind (see the next section) |
```

Then insert this section **between** "Unprivileged service" and "Audio
dropouts":

```markdown
## Shutdown and reboot from the web UI

The System tab offers three power actions. Two of them act on the machine
and need an authorisation; the third needs none.

| Action | Mechanism | Prerequisite |
|---|---|---|
| Shut down / restart the **system** | `systemctl poweroff` / `reboot` → logind → polkit | the polkit rule below |
| Restart **Ritornello** | the process exits, systemd starts it again (`Restart=always` in the unit) | none |

`deploy.sh` installs `deploy/50-ritornello-power.rules` into
`/etc/polkit-1/rules.d/`. It grants the `ritornello` user the six logind
actions involved — power-off and reboot, each in its plain,
`-multiple-sessions` and `-ignore-inhibit` form. All six, because logind
checks the plain action only when nothing else is going on: it switches to
`-multiple-sessions` as soon as another session exists (an open SSH
connection is enough, which is the usual situation while testing) and to
`-ignore-inhibit` when an inhibitor is held.

polkit itself is not installed by `deploy.sh` — the script installs no
package — and it is not present everywhere:

- **DietPi**: absent by default, `sudo apt install polkitd`;
- **Raspberry Pi OS Lite**: normally already there; if not, same command;
- **other Debian-based distributions**: `polkitd`, or `policykit-1` before
  Debian 12;
- **Arch, Fedora, openSUSE**: `polkit`, generally already installed.

To check, on the device:

    sudo -u ritornello busctl --system call org.freedesktop.login1 \
      /org/freedesktop/login1 org.freedesktop.login1.Manager CanPowerOff

`s "yes"` means the rule is in effect. `s "challenge"` or `s "no"` means it
is not: polkit is missing, or the rule did not land.

Nothing breaks without it: the core asks logind the same question at
startup, and the two system buttons stay **disabled**, with the reason shown
on the page. That answer is cached for the lifetime of the process, so
installing polkit takes effect at the next service start —
`sudo systemctl restart ritornello`, or simply the next `deploy.sh`.

"Restart Ritornello" depends on none of this: the process exits and systemd
starts it again two seconds later. Run **outside** systemd (development),
the same action merely stops the process — there is no supervisor to bring
it back. And systemd's start rate limit applies: five restarts within ten
seconds leave the unit failed, cleared with
`sudo systemctl reset-failed ritornello`.
```

- [ ] **Step 2: Document the routes in `interface.md`**

Add a `## System page` section after the "Config page" section (before
"Internationalization (i18n)"):

```markdown
## System page

`GET /api/system` reports OS metrics. **Every metric is optional and is
`null` when the machine does not expose it** — no thermal zone under WSL, no
cpufreq in most VMs, no `rpi_volt` sensor outside a Raspberry Pi — while the
set of keys stays stable:

```json
{
  "temperature_c": 47.8, "cpu_mhz": 900, "load": [0.12, 0.15, 0.09], "cpus": 4,
  "memory": { "total_kb": 948000, "available_kb": 512000 },
  "disk": { "total_kb": 30000000, "available_kb": 24000000 },
  "under_voltage": false, "uptime_s": 84213, "service_uptime_s": 3600,
  "hostname": "ritornello", "ip": "192.168.1.20",
  "os": "Debian GNU/Linux 12 (bookworm)", "kernel": "6.6.51+rpt-rpi-v7",
  "version": "0.1.0", "can_power_off": true, "can_reboot": true
}
```

Sources: `/sys/class/thermal/thermal_zone0/temp`,
`cpu0/cpufreq/scaling_cur_freq`, `/proc/loadavg`, `/proc/meminfo`
(`MemAvailable`, not `MemFree`), `statvfs("/")` (`f_bavail`, so the blocks
reserved for root are not counted as free), the `rpi_volt` hwmon
`in0_lcrit_alarm`, `/proc/uptime`, `/proc/sys/kernel/{hostname,osrelease}`,
`/etc/os-release`. The IP address is the local end of a UDP socket
*connected* to a routable address: no packet is sent and no internet access
is needed — the kernel is merely asked which interface faces the default
route.

`can_power_off` and `can_reboot` answer logind's `CanPowerOff`/`CanReboot`,
asked **once at startup** and cached: the page polls, and spawning `busctl`
twice per poll would be absurd. Installing the polkit rule therefore takes
effect at the next service start (see
[installation.md](installation.md#shutdown-and-reboot-from-the-web-ui)).

`POST /api/system/power` takes `{"action": "poweroff" | "reboot" |
"restart-service"}`. An unknown action is refused with `422` and an `error`
message. `poweroff` and `reboot` run `systemctl` and wait up to 5 s:
`202` when it succeeds or is still running (the machine is going away),
`502` carrying **logind's own message** when it refuses — that message names
the missing polkit rule, which a silent `202` would hide.
`restart-service` answers `202` and exits the process 300 ms later; systemd
restarts it because the unit says `Restart=always`. It needs no privilege,
which is why there is no `can_restart_service` field. Outside systemd, that
action stops the process for good.

The page polls `GET /api/system` every 5 s while it is open and visible,
rather than receiving a stream: unlike the player state, which the core
produces anyway, these metrics exist only because someone asked for them.
The CPU/RAM history graph lives in the page only — 60 samples, five
minutes, lost on navigation and never stored.
```

- [ ] **Step 3: Add the e2e journey**

In `web/app/e2e/parcours.spec.ts`, add this test at the end of the file
(adapting only the import/`test` helper names to what the file already
uses):

```ts
// L'onglet Système : rendu et navigation seulement. AUCUNE action
// d'alimentation n'est confirmée ici — le harnais lance un vrai cœur sur la
// machine de développement, où confirmer « Éteindre » l'arrêterait et
// « Redémarrer Ritornello » tuerait le harnais en cours de route. Le
// dialogue et l'envoi sont couverts par les tests vitest, qui n'ont pas de
// machine à perdre.
test('onglet Système : métriques et boutons présents', async ({ page }) => {
  await page.goto('/system')
  // Toujours lisible sous Linux, donc une valeur réelle et non « — ».
  await expect(page.locator('[data-system-kernel]')).not.toHaveText('—')
  await expect(page.locator('[data-system-memory]')).toBeVisible()
  await expect(page.locator('[data-system-disk]')).toBeVisible()
  await expect(page.locator('[data-power-poweroff]')).toBeVisible()
  await expect(page.locator('[data-power-restart]')).toBeVisible()
  // Le lien de navigation existe depuis la page d'accueil.
  await page.goto('/')
  await expect(page.locator('a[href="/system"]')).toBeVisible()
})
```

- [ ] **Step 4: Rebuild the SPA and run the e2e suite**

The e2e harness serves the SPA embedded in the core binary, so the order
matters:

```
npm run build
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && touch crates/ritornello-core/build.rs && cargo build --workspace"
```

then, from `web/app`: `npx playwright test`
Expected: every journey passes, including the new one.

- [ ] **Step 5: Commit**

```
git add docs web/app/e2e
git commit -m "docs+e2e: prérequis polkit, contrat de /api/system et parcours de l'onglet"
```

---

## Self-review notes (for the controller)

- Spec coverage: metrics (Task 1-2), history graph (5-6), poweroff/reboot
  (3-4), service restart (3, 7), tab and polling (6), i18n (6), docs and e2e
  (8). Every section of the design document maps to a task.
- The three `SystemInfo` injection fields appear only in Task 3, when the
  code using them appears: introducing them in Task 1 would have left dead
  fields, which `-D warnings` rejects.
- Task 6 adds i18n keys that only Task 7 consumes. That is deliberate and
  stated in Task 6's Interfaces block — a second pass over two catalogue
  files for the same feature would be churn, and the guard test only fails
  on keys used but missing, never the reverse.
