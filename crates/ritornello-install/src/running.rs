//! Which version of Ritornello the device runs, said right after the survey.
//!
//! **Asked of the running core**, which is the one that knows: it serves its
//! own version on `/api/update` (the core row's `installed`), from the
//! workstation's side, the way the operator's browser reaches the page. The
//! two records the survey reads could not say it alone: the installer's
//! registry falls behind every update made from the page (measured on the
//! living-room Pi: it recorded 0.2.0-beta.4 while 0.2.0-beta.6 ran), and the
//! page's own `placed.json` keeps a version a rollback has since undone.
//!
//! Only said, never decided on: nothing here reaches the plan, which judges
//! on root's registry alone (`plan::is_current`). A core that does not answer
//! — stopped, another port, a firewall — leaves the records, named for what
//! they are.

use std::io::Read;
use std::time::Duration;

use crate::device::DeviceState;

/// The core's port when nothing configures it (`RITORNELLO_HTTP` in its
/// unit). A device that moved it is answered from the records instead.
const PORT: u16 = 8080;

/// Long enough for a Pi 2 on a home network, short enough not to hold the
/// run when nothing answers.
const DEADLINE: Duration = Duration::from_secs(3);

/// The most read of the answer: the page's update state is a few kilobytes.
const CAP: u64 = 1024 * 1024;

/// The `/api/update` address of the device `host` names (`account@address`
/// or `address`). An IPv6 literal gets its brackets.
pub fn update_url(host: &str) -> String {
    let address = host.rsplit('@').next().unwrap_or(host);
    let address = if address.contains(':') && !address.starts_with('[') {
        format!("[{address}]")
    } else {
        address.to_string()
    };
    format!("http://{address}:{PORT}/api/update")
}

/// The running core's own version, from the body of `/api/update`: the core
/// row's `installed`. `None` for anything else.
pub fn core_version_in(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v["components"]
        .as_array()?
        .iter()
        .find(|c| c["kind"] == "core")?["installed"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Asks the core at `url`; `None` on any failure, which is said as such.
pub fn ask(url: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder().timeout(DEADLINE).build().ok()?;
    let response = client.get(url).send().ok()?.error_for_status().ok()?;
    let mut body = String::new();
    response.take(CAP).read_to_string(&mut body).ok()?;
    core_version_in(&body)
}

/// The sentence printed after the survey.
pub fn describe(dev: &DeviceState, running: Option<&str>) -> String {
    if !dev.core_present {
        return "Ritornello is not installed on this device.".to_string();
    }
    if let Some(version) = running {
        return format!("Ritornello is installed: its core runs version {version}.");
    }
    let recorded = dev.registry.as_ref().and_then(|r| r.components.get("core")).map(|r| r.version.as_str());
    let placed = dev.updater_placed.get("core").map(String::as_str);
    let mut s = format!("Ritornello is installed; its core did not answer on port {PORT}, so its version is ");
    match (recorded, placed) {
        (Some(r), Some(p)) if p != r => s.push_str(&format!(
            "not known for sure: ritornello-install last placed core {r}, and the web page placed {p} since."
        )),
        (Some(r), _) => s.push_str(&format!("not known for sure: ritornello-install last placed core {r}.")),
        (None, Some(p)) => s.push_str(&format!("not known for sure: the web page last placed core {p}.")),
        (None, None) => s.push_str("unknown."),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Recorded, Registry};
    use std::collections::BTreeMap;

    /// The core row as the living-room Pi served it (0.2.0-beta.6), cut to
    /// the core row and one other.
    const CAPTURE: &str = r#"{"components":[{"name":"radio","kind":"plugin","installed":"0.2.0-beta.5"},{"name":"core","kind":"core","declared":true,"binary_present":true,"installed":"0.2.0-beta.6","offered":null,"availability":"unknown"}]}"#;

    #[test]
    fn the_core_row_says_which_version_runs() {
        assert_eq!(core_version_in(CAPTURE).as_deref(), Some("0.2.0-beta.6"));
        assert_eq!(core_version_in(r#"{"components":[]}"#), None);
        assert_eq!(core_version_in("<html>not the page</html>"), None);
    }

    #[test]
    fn the_address_is_the_host_without_its_account() {
        assert_eq!(update_url("dietpi@192.168.0.57"), "http://192.168.0.57:8080/api/update");
        assert_eq!(update_url("dietpi.local"), "http://dietpi.local:8080/api/update");
        assert_eq!(update_url("pi@fe80::1"), "http://[fe80::1]:8080/api/update");
    }

    /// A real request against a local server answering the capture, and one
    /// against a port nothing listens on.
    #[test]
    fn the_running_core_is_asked_over_http() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::Write;
            if let Ok((mut socket, _)) = listener.accept() {
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", CAPTURE.len());
                let _ = socket.write_all(head.as_bytes());
                let _ = socket.write_all(CAPTURE.as_bytes());
            }
        });
        assert_eq!(ask(&format!("http://127.0.0.1:{port}/api/update")).as_deref(), Some("0.2.0-beta.6"));

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        assert_eq!(ask(&format!("http://127.0.0.1:{port}/api/update")), None);
    }

    fn device(core: bool, recorded: Option<&str>, placed: Option<&str>) -> DeviceState {
        let mut dev = crate::plan::tests::dev(&[], None, &[], &[]);
        dev.core_present = core;
        dev.registry = recorded.map(|v| Registry {
            format: 1,
            components: [(
                "core".to_string(),
                Recorded { version: v.to_string(), privileged: vec![], identity: BTreeMap::new() },
            )]
            .into(),
        });
        dev.updater_placed = placed.map(|v| [("core".to_string(), v.to_string())].into()).unwrap_or_default();
        dev
    }

    #[test]
    fn the_running_version_wins_over_every_record() {
        let dev = device(true, Some("0.2.0-beta.4"), Some("0.2.0-beta.7"));
        assert_eq!(describe(&dev, Some("0.2.0-beta.6")), "Ritornello is installed: its core runs version 0.2.0-beta.6.");
    }

    /// Without an answer, each record is named for what it is, never passed
    /// off as the running version.
    #[test]
    fn without_an_answer_the_records_are_named_for_what_they_are() {
        let both = describe(&device(true, Some("0.2.0-beta.4"), Some("0.2.0-beta.6")), None);
        assert!(both.contains("ritornello-install last placed core 0.2.0-beta.4"), "{both}");
        assert!(both.contains("the web page placed 0.2.0-beta.6 since"), "{both}");
        assert!(both.contains("not known for sure"), "{both}");
        let same = describe(&device(true, Some("0.2.0-beta.6"), Some("0.2.0-beta.6")), None);
        assert!(!same.contains("web page"), "{same}");
        assert!(describe(&device(true, None, None), None).ends_with("unknown."));
        assert_eq!(describe(&device(false, None, None), None), "Ritornello is not installed on this device.");
    }
}
