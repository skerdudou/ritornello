//! The device's time zone: read it, report its offset, and change it.
//!
//! The automatic update fires at an hour counted in the device's local time
//! (`update::schedule::local_now`), so the page has to say which zone that
//! is, and let the owner correct it: a Raspberry Pi image ships set to UTC or
//! to whatever its author used, and nothing else on the device shows it.
//!
//! Reading needs no privilege and spawns nothing — `/etc/localtime` is a
//! symlink whose target names the zone. Changing it goes through
//! `systemd-timedated` over D-Bus, which asks polkit; the rule
//! `53-ritornello-time.rules` grants `org.freedesktop.timedate1.set-timezone`
//! to the service user, and the operator may decline it at install time.
//! The core never writes `/etc/localtime` itself: it runs unprivileged, and
//! timedated is the one component that knows how to change the zone safely.
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ritornello_i18n::Chain;
use std::path::Path;

/// The zone named by a `/etc/localtime` link target.
///
/// Everything up to and including the first `zoneinfo/` is the zone
/// database's location, which varies (`/usr/share/zoneinfo/` on Debian, the
/// relative `../usr/share/zoneinfo/` timedated writes); what follows is the
/// IANA name. A target that does not go through a `zoneinfo/` directory
/// names no zone: `None`, and the page says "unknown" rather than guessing.
pub fn zone_from_link(target: &str) -> Option<String> {
    let (_, zone) = target.split_once("zoneinfo/")?;
    (!zone.is_empty()).then(|| zone.to_string())
}

/// `current_zone` with its two paths as parameters, so the fallbacks can be
/// tested on a temporary directory rather than on the machine's own files.
///
/// The link first, `/etc/timezone` second: timedated replaces the link and
/// does not touch `/etc/timezone`, so after a change from the page the latter
/// can be stale. It is still worth reading when `/etc/localtime` is a copied
/// file rather than a link (some images do that), where the link says
/// nothing at all.
fn zone_at(localtime: &Path, timezone_file: &Path) -> Option<String> {
    if let Some(zone) = std::fs::read_link(localtime)
        .ok()
        .and_then(|target| zone_from_link(&target.to_string_lossy()))
    {
        return Some(zone);
    }
    let raw = std::fs::read_to_string(timezone_file).ok()?;
    raw.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// The device's zone name, `None` when nothing names it.
///
/// No spawn — `timedatectl show` would answer the same thing at the cost of a
/// process per poll of the System tab, and a `readlink` is a syscall. Read at
/// every poll rather than cached: a change from the page, or from a shell,
/// shows at the next one.
pub fn current_zone() -> Option<String> {
    zone_at(Path::new("/etc/localtime"), Path::new("/etc/timezone"))
}

// `libc` 0.2 declares `tzset` for Windows only, so it is declared here, once.
// glibc, musl and macOS all export it. Only glibc builds of the core are
// shipped (armv7 and aarch64 `-gnu`); musl's `tzset` does not re-read an
// unchanged `TZ`-less default, which does not matter for a binary never built
// for it.
unsafe extern "C" {
    fn tzset();
}

/// Makes this process follow the zone `/etc/localtime` names **now**.
///
/// glibc's `localtime_r` reads the zone once per process and never again, so
/// without this a zone changed from the page would only reach the update
/// schedule at the next service restart — the hour shown and the hour used
/// would disagree in the meantime. `tzset()` re-reads the file when `TZ` is
/// unset (the unit sets none), and costs one `stat` when the file has not
/// changed: glibc compares its inode and mtime before parsing anything.
///
/// The single place `tzset` is called. glibc serialises it with
/// `localtime_r` behind its own lock, so calling it from an HTTP handler
/// while the update worker reads the clock is safe.
pub fn tzset_now() {
    // SAFETY: `tzset` takes no argument and only updates libc's own time-zone
    // state, which glibc guards with a lock shared with `localtime_r`.
    unsafe { tzset() };
}

/// The device's offset from UTC at `unix_s`, in seconds east (Paris in
/// winter: 3600).
///
/// Reported by the core, not computed by the page: the browser knows its own
/// zone, not the device's, and the two differing is precisely what the page
/// wants to point out. `tzset_now` first, for the reason given there.
pub fn utc_offset_seconds(unix_s: i64) -> Option<i32> {
    tzset_now();
    // SAFETY: as in `schedule::local_now` — `localtime_r` fills the `tm` we
    // own and reads the `time_t` we hand it, nothing else.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = unix_s as libc::time_t;
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    // `tm_gmtoff` is a C `long`: 32 bits on armv7, 64 on x86_64. `try_from`
    // covers both; on armv7 it is the identity, which clippy (run on x86_64
    // only) would otherwise flag there.
    #[allow(clippy::useless_conversion)]
    i32::try_from(tm.tm_gmtoff).ok()
}

/// Is `s` a plausible IANA zone name, to be handed to timedated?
///
/// Plausible, not known: timedated checks the name against its own database
/// and refuses an unknown one with a message the route relays. This check is
/// what stands between a request body and a spawned command's arguments, so
/// it is narrow on purpose: 1 to 64 characters of `[A-Za-z0-9_+-/]`, no
/// empty segment (which also refuses a leading or trailing `/`), no `..`
/// segment, and no leading `-` — `busctl` would read `-Europe` as an option.
/// No IANA name breaks any of these.
pub fn valid_zone_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-' | b'/'))
        && !s.starts_with('-')
        && s.split('/').all(|seg| !seg.is_empty() && seg != "..")
}

/// Reads polkit's `CheckAuthorization` reply as `busctl` prints it:
/// `(bba{ss}) true false 0` — `is_authorized`, `is_challenge`, details. Only
/// the first boolean matters: a challenge (interactive authentication) is a
/// no for a service nobody can authenticate for. `None` on anything else.
pub fn parse_authorized(raw: &str) -> Option<bool> {
    let rest = raw.trim().strip_prefix("(bba{ss})")?;
    match rest.split_whitespace().next()? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// What the startup probe learned about changing the zone.
///
/// Three facts rather than one, for the reason `Metrics::logind_reachable`
/// gives for power: polkit saying no means the rule is missing or was
/// declined at install, whereas timedated not answering means there is no
/// timedated to ask (a container, an image without it), and polkit not
/// answering means there is no polkitd to ask — absent by default on DietPi,
/// or bus-activated and too slow on a cold start. All three leave the zone
/// read-only on the page, but they are not fixed by the same thing, and
/// sending the owner to rerun the installer when polkitd is missing is the
/// explanation that sends them looking in the wrong place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimezoneProbe {
    pub settable: bool,
    pub timedated_reachable: bool,
    /// Whether polkit gave a readable yes or no. `false` when the call
    /// failed, timed out, or answered something `parse_authorized` does not
    /// recognise: then a `settable` of `false` says nothing about the grant.
    pub polkit_answered: bool,
}

/// The rule for combining the two answers, apart from obtaining them, so it
/// is tested without a D-Bus. Settable only when timedated answers **and**
/// polkit says yes: polkit can grant an action whose service is absent.
pub fn probe_summary(reachable: bool, authorized: Option<bool>) -> TimezoneProbe {
    TimezoneProbe {
        settable: reachable && authorized == Some(true),
        timedated_reachable: reachable,
        polkit_answered: authorized.is_some(),
    }
}

/// Runs `busctl` with `args`, 3 s at most. `Some(stdout)` on success, `None`
/// otherwise, with an INFO line: a development machine without timedated or
/// polkit is a normal situation, and WARN lines surface on the config page.
async fn busctl_query(what: &str, args: &[&str]) -> Option<String> {
    let call = tokio::process::Command::new("busctl").args(args).kill_on_drop(true).output();
    match tokio::time::timeout(std::time::Duration::from_secs(3), call).await {
        Ok(Ok(out)) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(Ok(out)) => {
            tracing::info!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
            None
        }
        Ok(Err(e)) => {
            tracing::info!("busctl unavailable ({e}): the time zone is read-only in the UI");
            None
        }
        Err(_) => {
            tracing::info!("{what}: no response within 3s");
            None
        }
    }
}

/// Asks, once at startup, whether this process may change the time zone.
///
/// Cached for the process lifetime in `SystemInfo`, like the power probe
/// (`system::probe_capabilities`) and for the same reason: spawning `busctl`
/// twice per poll of the System tab would be absurd. Granting or declining
/// the rule therefore takes effect at the next start of the service — which
/// is what installing it does, `ritornello-install` restarting the service
/// after placing or removing it.
///
/// Two questions, joined rather than chained so a hung D-Bus delays startup
/// by one timeout, not two: does timedated answer (reading its `Timezone`
/// property needs no authorization), and would polkit grant `set-timezone`
/// to this process? The second asks polkit directly, because the only other
/// way to know would be to try a change. The subject is this process
/// (`unix-process`; a start time of 0 lets polkit look it up), and `busctl`
/// runs as the same user — polkit refuses to answer a caller about a subject
/// belonging to someone else.
pub async fn probe() -> TimezoneProbe {
    let pid = std::process::id().to_string();
    let timedated = [
        "--system",
        "get-property",
        "org.freedesktop.timedate1",
        "/org/freedesktop/timedate1",
        "org.freedesktop.timedate1",
        "Timezone",
    ];
    let polkit = [
        "--system",
        "call",
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
        "CheckAuthorization",
        "(sa{sv})sa{ss}us",
        "unix-process",
        "2",
        "pid",
        "u",
        &pid,
        "start-time",
        "t",
        "0",
        "org.freedesktop.timedate1.set-timezone",
        "0",
        "0",
        "",
    ];
    let (reachable, authorized) = tokio::join!(
        busctl_query("timedated Timezone", &timedated),
        busctl_query("polkit set-timezone", &polkit),
    );
    probe_summary(reachable.is_some(), authorized.as_deref().and_then(parse_authorized))
}

/// Not a plausible zone name (`valid_zone_name`). Same model as
/// `system::UnknownPowerAction`: the user's text through the catalog, the
/// English one through `Display` for the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidZone;

impl InvalidZone {
    pub fn message(&self, catalog: &Chain) -> String {
        catalog.get("timezone_invalid").to_string()
    }
}

impl std::fmt::Display for InvalidZone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not a time zone name")
    }
}

impl std::error::Error for InvalidZone {}

/// `busctl` failed and wrote nothing on stderr. When it did write something —
/// timedated's "Invalid or not installed time zone", polkit's "Access denied"
/// — that text is relayed instead, as `power_post` does with logind's: it
/// names what is missing, which no generic sentence could.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimezoneFailed {
    pub code: i32,
}

impl TimezoneFailed {
    pub fn message(&self, catalog: &Chain) -> String {
        catalog.get("timezone_failed").replace("{code}", &self.code.to_string())
    }
}

impl std::fmt::Display for TimezoneFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SetTimezone failed (exit code {})", self.code)
    }
}

impl std::error::Error for TimezoneFailed {}

/// `busctl` could not be started, or timedated did not answer in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimezoneUnreachable {
    pub detail: String,
}

impl TimezoneUnreachable {
    pub fn message(&self, catalog: &Chain) -> String {
        catalog.get("timezone_unreachable").replace("{detail}", &self.detail)
    }
}

impl std::fmt::Display for TimezoneUnreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timedated unreachable: {}", self.detail)
    }
}

impl std::error::Error for TimezoneUnreachable {}

#[derive(serde::Deserialize)]
pub struct TimezoneRequest {
    timezone: String,
}

/// `POST /api/system/timezone {"timezone": "Europe/Paris"}`.
///
/// The name is checked before anything is spawned (`valid_zone_name`):
/// `422` otherwise. Then timedated's `SetTimezone`, `interactive` false — a
/// service has nobody to type a password, so polkit must answer from its
/// rules alone. Awaited up to 5 s, because no route may block (the admin
/// protocol is serial) and because the answer is the point: a refusal is
/// reported in timedated's or polkit's own words. A change still running
/// after 5 s answers `502`, and `kill_on_drop` ends the stuck `busctl` rather
/// than leaving one behind per click. That does not cancel the D-Bus call
/// already sent: `systemd-timedated` may still apply the zone, so a `502` by
/// timeout is not proof that nothing changed — the next `/api/system` poll
/// re-reads the link and shows what is actually set.
///
/// `204` on success, after `tzset_now()` so the update schedule follows at
/// once (see there).
pub async fn timezone_post(
    State(state): State<crate::status::AppState>,
    Json(req): Json<TimezoneRequest>,
) -> Response {
    let name = req.timezone;
    if !valid_zone_name(&name) {
        tracing::warn!("time zone change refused: {InvalidZone}");
        let msg = InvalidZone.message(&*state.catalog.read().await);
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response();
    }
    tracing::warn!("time zone change to {name} requested from the UI");
    let call = tokio::process::Command::new(&state.system.busctl)
        .args([
            "--system",
            "call",
            "org.freedesktop.timedate1",
            "/org/freedesktop/timedate1",
            "org.freedesktop.timedate1",
            "SetTimezone",
            "sb",
            &name,
            "false",
        ])
        .kill_on_drop(true)
        .output();
    let msg = match tokio::time::timeout(std::time::Duration::from_secs(5), call).await {
        Ok(Ok(out)) if out.status.success() => {
            tzset_now();
            tracing::info!("time zone set to {name}");
            return StatusCode::NO_CONTENT.into_response();
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // Two texts for two audiences, as in `power_post`: the user's
            // sentence in the body, an English line in the log.
            if stderr.is_empty() {
                let err = TimezoneFailed { code: out.status.code().unwrap_or(-1) };
                tracing::warn!("time zone change refused: {err}");
                err.message(&*state.catalog.read().await)
            } else {
                tracing::warn!("time zone change refused: {stderr}");
                stderr
            }
        }
        Ok(Err(e)) => {
            let err = TimezoneUnreachable { detail: e.to_string() };
            tracing::warn!("time zone change failed: {err}");
            err.message(&*state.catalog.read().await)
        }
        Err(_) => {
            let err = TimezoneUnreachable { detail: "no answer within 5 s".to_string() };
            tracing::warn!("time zone change failed: {err}");
            err.message(&*state.catalog.read().await)
        }
    };
    (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": msg }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{router, AppState};
    use crate::system::SystemInfo;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    #[test]
    fn timezone_zone_from_link_strips_the_zoneinfo_prefix() {
        assert_eq!(zone_from_link("/usr/share/zoneinfo/Europe/Paris").as_deref(), Some("Europe/Paris"));
        // Debian's own link is relative: `../usr/share/zoneinfo/...`.
        assert_eq!(zone_from_link("../usr/share/zoneinfo/Europe/Paris").as_deref(), Some("Europe/Paris"));
        assert_eq!(zone_from_link("/usr/share/zoneinfo/UTC").as_deref(), Some("UTC"));
        assert_eq!(
            zone_from_link("/usr/share/zoneinfo/America/Argentina/Buenos_Aires").as_deref(),
            Some("America/Argentina/Buenos_Aires")
        );
        // A link that does not go through a zoneinfo directory names no zone.
        assert_eq!(zone_from_link("/etc/localtime.dpkg-old"), None);
        assert_eq!(zone_from_link(""), None);
        // The directory itself, with nothing after it, is not a zone either.
        assert_eq!(zone_from_link("/usr/share/zoneinfo/"), None);
    }

    #[test]
    fn timezone_read_from_the_link_then_from_etc_timezone() {
        let dir = tempfile::tempdir().unwrap();
        let localtime = dir.path().join("localtime");
        let timezone = dir.path().join("timezone");

        // Neither file: an unknown zone, not a failure.
        assert_eq!(zone_at(&localtime, &timezone), None);

        // `/etc/localtime` as a copied file rather than a link (Review Focus
        // #5): the link says nothing, `/etc/timezone` answers.
        std::fs::write(&localtime, b"TZif2 not a link").unwrap();
        assert_eq!(zone_at(&localtime, &timezone), None);
        std::fs::write(&timezone, "\n  Europe/Berlin  \nignored\n").unwrap();
        assert_eq!(zone_at(&localtime, &timezone).as_deref(), Some("Europe/Berlin"));

        // A link wins over `/etc/timezone`, which timedated does not update.
        std::fs::remove_file(&localtime).unwrap();
        std::os::unix::fs::symlink("../usr/share/zoneinfo/Europe/Paris", &localtime).unwrap();
        assert_eq!(zone_at(&localtime, &timezone).as_deref(), Some("Europe/Paris"));

        // An empty `/etc/timezone` behind a link that names nothing: unknown.
        std::fs::remove_file(&localtime).unwrap();
        std::os::unix::fs::symlink("/somewhere/else", &localtime).unwrap();
        std::fs::write(&timezone, "\n   \n").unwrap();
        assert_eq!(zone_at(&localtime, &timezone), None);
    }

    #[test]
    fn timezone_offset_is_known_on_linux() {
        // Whatever zone the machine running the suite is in, the offset is a
        // whole number of minutes within the range real zones use.
        let off = utc_offset_seconds(1_762_000_000).expect("localtime_r works on Linux");
        assert_eq!(off % 60, 0);
        assert!((-12 * 3600..=14 * 3600).contains(&off), "{off}");
    }

    #[test]
    fn timezone_valid_zone_names() {
        for ok in [
            "Europe/Paris",
            "UTC",
            "America/Argentina/Buenos_Aires",
            "America/Port-au-Prince",
            "Etc/GMT+1",
            "Etc/GMT-14",
        ] {
            assert!(valid_zone_name(ok), "{ok} must be accepted");
        }
        let long = "A".repeat(200);
        for bad in [
            // Review Focus #4.
            "../../etc/passwd",
            "",
            long.as_str(),
            "Europe/Paris;rm",
            // And the rest of the rule.
            "/Europe/Paris",
            "Europe//Paris",
            "Europe/Paris/",
            "Europe/..",
            "Europe/Pa ris",
            "Europe/Paris\n",
            "Europe/Pàris",
            "-Europe",
        ] {
            assert!(!valid_zone_name(bad), "{bad:?} must be refused");
        }
        assert!(valid_zone_name(&"A".repeat(64)));
        assert!(!valid_zone_name(&"A".repeat(65)));
    }

    #[test]
    fn timezone_polkit_answer() {
        assert_eq!(parse_authorized("(bba{ss}) true false 0\n"), Some(true));
        assert_eq!(parse_authorized("(bba{ss}) false true 0\n"), Some(false));
        // `is_challenge` true, `is_authorized` false: a challenge is a no for
        // a service that nobody can authenticate for.
        assert_eq!(parse_authorized("(bba{ss}) false false 0\n"), Some(false));
        assert_eq!(parse_authorized("(bba{ss}) false false 1 \"polkit.retains_authorization_after_challenge\" \"1\"\n"), Some(false));
        assert_eq!(parse_authorized(""), None);
        assert_eq!(parse_authorized("garbage"), None);
        assert_eq!(parse_authorized("(bba{ss}) maybe false 0"), None);
        assert_eq!(parse_authorized("s \"yes\""), None);
    }

    #[test]
    fn timezone_settable_needs_timedated_and_polkit() {
        let probe = |settable, timedated_reachable, polkit_answered| TimezoneProbe {
            settable,
            timedated_reachable,
            polkit_answered,
        };
        assert_eq!(probe_summary(true, Some(true)), probe(true, true, true));
        // timedated answers, polkit says no: the rule is missing or declined.
        assert_eq!(probe_summary(true, Some(false)), probe(false, true, true));
        // timedated answers, polkit does not: nothing is offered, and nothing
        // is known about the grant.
        assert_eq!(probe_summary(true, None), probe(false, true, false));
        // polkit would allow it, but there is nobody to ask.
        assert_eq!(probe_summary(false, Some(true)), probe(false, false, true));
    }

    /// The sentences that explain a greyed-out control send the owner where
    /// the cause is, in every catalog. polkit not answering names polkitd and
    /// never the installer (rerunning it changes nothing when polkitd is
    /// missing); the power sentence, where logind cannot tell a refusal from
    /// a missing polkitd, names both.
    #[test]
    fn power_and_polkit_sentences_name_the_right_cause() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut catalogs = vec![("en".to_string(), crate::i18n::EN.to_string())];
        for lang in ["fr", "de", "es", "it"] {
            let path = root.join(format!("../../deploy/locales/core/{lang}.toml"));
            catalogs.push((lang.to_string(), std::fs::read_to_string(&path).unwrap()));
        }
        for (lang, text) in catalogs {
            let table: toml::Table = text.parse().unwrap();
            let get = |key: &str| table[key].as_str().unwrap_or_else(|| panic!("{lang}: {key}")).to_string();
            let no_answer = get("system_time_polkit_no_answer");
            assert!(no_answer.contains("polkitd"), "{lang}: {no_answer}");
            assert!(no_answer.contains("docs/installation.md"), "{lang}: {no_answer}");
            assert!(!no_answer.contains("ritornello-install"), "{lang}: {no_answer}");
            let power = get("system_power_not_allowed");
            assert!(power.contains("polkit"), "{lang}: {power}");
            assert!(power.contains("ritornello-install"), "{lang}: {power}");
            assert!(power.contains("docs/installation.md"), "{lang}: {power}");
            let warning = get("update_hour_zone_warning");
            assert!(warning.contains("{device_time}") && !warning.contains("{device_hour}"), "{lang}: {warning}");
        }
    }

    fn app(info: SystemInfo) -> axum::Router {
        router(AppState { system: Arc::new(info), ..crate::status::tests_support::app_state() })
    }

    async fn post_timezone(app: axum::Router, body: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::post("/api/system/timezone")
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

    /// A `busctl` stand-in that records its arguments in `marker` and
    /// succeeds: its absence after a request proves nothing was spawned.
    fn stub_busctl(dir: &std::path::Path) -> (String, std::path::PathBuf) {
        let script = dir.join("busctl");
        let marker = dir.join("called");
        crate::test_exec::write_executable(
            &script,
            &format!("#!/bin/sh\necho \"$@\" >> '{}'\n", marker.display()),
        );
        (script.to_string_lossy().to_string(), marker)
    }

    #[tokio::test]
    async fn timezone_post_refuses_an_implausible_name_before_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let (busctl, marker) = stub_busctl(dir.path());
        let long = "A".repeat(200);
        for bad in ["../../etc/passwd", "", long.as_str(), "Europe/Paris;rm"] {
            let body = serde_json::json!({ "timezone": bad }).to_string();
            let (status, v) = post_timezone(app(SystemInfo { busctl: busctl.clone(), ..Default::default() }), &body).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
            assert!(v["error"].as_str().is_some_and(|m| !m.is_empty()));
        }
        assert!(!marker.exists(), "nothing may be spawned for a refused name");

        // The control: the same stub, given a valid name, does leave its mark
        // — so its absence above means something.
        let body = r#"{"timezone":"Europe/Paris"}"#;
        let (status, _) = post_timezone(app(SystemInfo { busctl, ..Default::default() }), body).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let args = std::fs::read_to_string(&marker).unwrap();
        assert_eq!(
            args.trim(),
            "--system call org.freedesktop.timedate1 /org/freedesktop/timedate1 \
             org.freedesktop.timedate1 SetTimezone sb Europe/Paris false"
        );
    }

    #[tokio::test]
    async fn timezone_post_succeeds_when_busctl_does() {
        let info = SystemInfo { busctl: "/bin/true".to_string(), ..Default::default() };
        let (status, _) = post_timezone(app(info), r#"{"timezone":"UTC"}"#).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn timezone_post_relays_a_refusal() {
        let info = SystemInfo { busctl: "/bin/false".to_string(), ..Default::default() };
        let (status, v) = post_timezone(app(info), r#"{"timezone":"UTC"}"#).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        // `/bin/false` writes nothing on stderr: the catalog's sentence.
        assert!(v["error"].as_str().is_some_and(|m| !m.is_empty() && m != "timezone_failed"), "{v}");
    }

    #[tokio::test]
    async fn timezone_post_relays_busctls_own_message() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("busctl");
        crate::test_exec::write_executable(
            &script,
            "#!/bin/sh\necho 'Call failed: Access denied' >&2\nexit 1\n",
        );
        let info = SystemInfo { busctl: script.to_string_lossy().to_string(), ..Default::default() };
        let (status, v) = post_timezone(app(info), r#"{"timezone":"UTC"}"#).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(v["error"], "Call failed: Access denied");
    }

    #[tokio::test]
    async fn timezone_post_reports_a_busctl_that_cannot_start() {
        let info = SystemInfo { busctl: "/nonexistent".to_string(), ..Default::default() };
        let (status, v) = post_timezone(app(info), r#"{"timezone":"UTC"}"#).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(v["error"].as_str().is_some_and(|m| !m.is_empty() && m != "timezone_unreachable"), "{v}");
    }

    /// The three answers of the polkit probe reach the page as three distinct
    /// JSON shapes. The regression this guards: a polkit that did not answer
    /// (polkitd absent on DietPi, or slow to bus-activate) was reported exactly
    /// like a refusal, and the page sent the owner to rerun the installer.
    #[tokio::test]
    async fn timezone_polkit_three_states_reach_the_page() {
        for (authorized, settable, answered) in
            [(Some(true), true, true), (Some(false), false, true), (None, false, false)]
        {
            let p = probe_summary(true, authorized);
            let info = SystemInfo {
                timezone_settable: p.settable,
                timedated_reachable: p.timedated_reachable,
                timezone_polkit_answered: p.polkit_answered,
                ..Default::default()
            };
            let resp = app(info).oneshot(Request::get("/api/system").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let v: serde_json::Value =
                serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
            assert_eq!(v["timedated_reachable"], true, "{authorized:?}");
            assert_eq!(v["timezone_settable"], settable, "{authorized:?}");
            assert_eq!(v["timezone_polkit_answered"], answered, "{authorized:?}");
        }
    }

    #[tokio::test]
    async fn timezone_keys_in_get_system() {
        let info = SystemInfo { timezone_settable: true, timedated_reachable: true, ..Default::default() };
        let resp = app(info).oneshot(Request::get("/api/system").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        for key in [
            "timezone",
            "utc_offset_seconds",
            "device_unix_s",
            "timezone_settable",
            "timedated_reachable",
            "timezone_polkit_answered",
        ] {
            assert!(v.get(key).is_some(), "key {key} missing");
        }
        assert_eq!(v["timezone_settable"], true);
        assert_eq!(v["timedated_reachable"], true);
        assert!(v["utc_offset_seconds"].is_i64(), "the offset is known on Linux: {v}");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!((v["device_unix_s"].as_i64().unwrap() - now).abs() < 60);
    }
}
