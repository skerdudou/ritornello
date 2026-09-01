//! A plugin's command line, as the core builds it
//! (`plugins::spawn`): `--register <path>`, `--name <name>`, and
//! `--socket-prefix <prefix>`, all mandatory.
//!
//! A single copy here rather than one per binary: the 2026-07-27 review
//! counted six variants of this parsing across the plugins, of which
//! only one properly rejected an option without a value — the other five
//! panicked with an anonymous "index out of bounds" when `--socket` was the
//! last argument.

use std::path::PathBuf;
use ritornello_proto::PluginKind;

/// Value of the `flag` option in `args` (form `--flag <value>`).
///
/// Pure function so it's testable. `None` if the option is absent; panics
/// **naming the option** if it is present without a value — these command
/// lines are built by the core, a missing value is a wiring bug to be
/// pointed out clearly.
pub fn arg_value(args: &[String], flag: &str) -> Option<PathBuf> {
    args.iter().position(|a| a == flag).map(|i| {
        let value = args
            .get(i + 1)
            .unwrap_or_else(|| panic!("{flag} requires a value (no argument after {flag})"));
        PathBuf::from(value)
    })
}

/// Path to the core's registration socket (`--register`), mandatory.
pub fn register_socket() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--register").expect("--register <path> required")
}

/// Name under which the core knows this plugin (`--name`), mandatory.
///
/// The plugin **echoes it back** in its announcement without ever inventing it:
/// it's the manifest that has authority, otherwise two binaries could claim
/// the same name and collide on their socket paths.
pub fn plugin_name() -> String {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--name")
        .expect("--name <name> required")
        .to_string_lossy()
        .into_owned()
}

/// Prefix of the sockets this plugin must bind (`--socket-prefix`).
///
/// The core keeps control of the directory and the prefix; the plugin only
/// has authority over the suffixes, which are exactly what it announces.
pub fn socket_prefix() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--socket-prefix").expect("--socket-prefix <path> required")
}

/// `{prefix}-{kind}.sock`.
pub fn socket_kind(prefix: &std::path::Path, kind: PluginKind) -> PathBuf {
    let kind = match kind {
        PluginKind::Source => "source",
        PluginKind::Display => "display",
        PluginKind::Input => "input",
        PluginKind::Metadata => "metadata",
    };
    PathBuf::from(format!("{}-{kind}.sock", prefix.display()))
}

/// `{prefix}-admin.sock`.
pub fn admin_socket(prefix: &std::path::Path) -> PathBuf {
    PathBuf::from(format!("{}-admin.sock", prefix.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extracts_the_value_following_the_flag() {
        let a = args(&["plugin", "--register", "/run/register.sock", "--name", "radio"]);
        assert_eq!(arg_value(&a, "--register"), Some(PathBuf::from("/run/register.sock")));
        assert_eq!(arg_value(&a, "--name"), Some(PathBuf::from("radio")));
        assert_eq!(arg_value(&a, "--other"), None);
    }

    #[test]
    fn a_flag_without_a_value_panics_naming_the_flag() {
        // This was the flaw of the five non-robust copies: "index out of
        // bounds" points at nothing.
        let a = args(&["plugin", "--socket-prefix"]);
        let e = std::panic::catch_unwind(|| arg_value(&a, "--socket-prefix")).unwrap_err();
        let msg = e.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("--socket-prefix"), "the message must name the option: {msg}");
    }

    #[test]
    fn extracts_the_three_options_of_the_new_setup() {
        let a = args(&[
            "plugin",
            "--register", "/run/ritornello/sockets/register.sock",
            "--name", "radio",
            "--socket-prefix", "/run/ritornello/sockets/radio",
        ]);
        assert_eq!(
            arg_value(&a, "--register"),
            Some(PathBuf::from("/run/ritornello/sockets/register.sock"))
        );
        assert_eq!(arg_value(&a, "--name"), Some(PathBuf::from("radio")));
        assert_eq!(
            arg_value(&a, "--socket-prefix"),
            Some(PathBuf::from("/run/ritornello/sockets/radio"))
        );
    }

    #[test]
    fn suffixes_a_prefix_per_kind_and_per_admin() {
        let p = PathBuf::from("/run/ritornello/sockets/radio");
        assert_eq!(
            super::socket_kind(&p, ritornello_proto::PluginKind::Source),
            PathBuf::from("/run/ritornello/sockets/radio-source.sock")
        );
        assert_eq!(
            super::admin_socket(&p),
            PathBuf::from("/run/ritornello/sockets/radio-admin.sock")
        );
    }
}
