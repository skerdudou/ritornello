//! The MPD server's listen address and port.
//!
//! An absent or unreadable file falls back to the defaults while logging:
//! that is the policy of `Stations::load` on the radio side, and it holds here
//! for the same reason — a plugin that refuses to start over a malformed file
//! vanishes from the status page instead of explaining its problem there.

use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_listen() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    6600
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Listen address. `0.0.0.0` by default, like the device's web server:
    /// the same surface, already exposed.
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self { listen: default_listen(), port: default_port() }
    }
}

impl Config {
    /// Loads the config from `path`, or falls back to the defaults while
    /// logging if the file is absent, unreadable, or invalid once parsed.
    /// Never returns an error: a plugin that refuses to start over a malformed
    /// file vanishes from the status page instead of explaining its problem
    /// there.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::info!("no config at {}: {e}; using defaults", path.display());
                return Self::default();
            }
        };
        match toml::from_str::<Self>(&text) {
            Ok(c) => match c.validate() {
                Ok(()) => c,
                Err(reason) => {
                    tracing::warn!("invalid config at {}: {reason}; using defaults", path.display());
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!("unreadable config at {}: {e}; using defaults", path.display());
                Self::default()
            }
        }
    }

    /// Returns a catalog **key**, not a sentence: the admin page translates it
    /// (Task 9). Same convention as the radio's rejections.
    pub fn validate(&self) -> Result<(), String> {
        if self.listen.trim().is_empty() {
            return Err("listen_empty".into());
        }
        if self.port == 0 {
            return Err("port_zero".into());
        }
        Ok(())
    }

    /// Saves the config to disk, rejecting it first if it is invalid. The
    /// error returned is, like `validate`, a catalog key — never a sentence
    /// nor a raw I/O message: the admin page translates it.
    ///
    /// Called by `admin.rs` (Task 9), which resolves the key returned on
    /// failure into a catalog sentence before replying.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let text = toml::to_string_pretty(self).map_err(|_| "save_failed".to_string())?;
        // Temporary file then rename: the rename is atomic on the same file
        // system, so no interruption leaves a truncated toml in place of the
        // good one.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|_| "save_failed".to_string())?;
        std::fs::rename(&tmp, path).map_err(|_| "save_failed".to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_config_gives_the_defaults() {
        // A missing file is not an error: the plugin must start listening on
        // 0.0.0.0:6600 without anything having been provisioned.
        let c = Config::load(std::path::Path::new("/does/not/exist.toml"));
        assert_eq!(c.listen, "0.0.0.0");
        assert_eq!(c.port, 6600);
    }

    #[test]
    fn a_partial_config_is_completed_by_the_defaults() {
        let c: Config = toml::from_str("port = 6601").unwrap();
        assert_eq!(c.listen, "0.0.0.0");
        assert_eq!(c.port, 6601);
    }

    #[test]
    fn port_zero_is_rejected() {
        // 0 would ask the kernel for a free port: the client would not know which one.
        let c = Config { listen: "0.0.0.0".into(), port: 0 };
        assert!(c.validate().is_err());
    }

    #[test]
    fn an_empty_address_is_rejected() {
        let c = Config { listen: String::new(), port: 6600 };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_returns_catalog_keys_and_not_sentences() {
        // Repo convention: the admin page (Task 9) translates the key. A
        // ready-made sentence here could not be relocalized, and would quietly
        // break into English in a French page.
        let empty = Config { listen: String::new(), port: 6600 };
        assert_eq!(empty.validate().unwrap_err(), "listen_empty");
        let zero = Config { listen: "0.0.0.0".into(), port: 0 };
        assert_eq!(zero.validate().unwrap_err(), "port_zero");
    }

    #[test]
    fn the_save_is_atomic_and_readable_back() {
        // Write through a temporary file then rename: a power cut never
        // leaves a truncated toml in place of the good one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        let c = Config { listen: "127.0.0.1".into(), port: 6601 };
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path), c);
        assert!(!dir.path().join("mpd.toml.tmp").exists(), "the temporary file does not survive");
    }

    #[test]
    fn the_save_rejects_an_invalid_config_without_touching_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        let invalid = Config { listen: "0.0.0.0".into(), port: 0 };
        assert_eq!(invalid.save(&path).unwrap_err(), "port_zero");
        assert!(!path.exists(), "nothing must be written when validation rejects");
    }

    #[test]
    fn an_unreadable_toml_does_not_fail_startup() {
        // Same policy as the radio's stations: fall back to the defaults while
        // logging, rather than refusing to start.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        std::fs::write(&path, "this is not toml =").unwrap();
        assert_eq!(Config::load(&path), Config::default());
    }

    #[test]
    fn a_syntactically_valid_but_rejected_config_also_falls_back_to_the_defaults() {
        // Distinct from the previous test: here the toml parses, but
        // `validate()` rejects the content (port at 0). `load` must fall back
        // to the defaults in this case too, not only on a parse error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        std::fs::write(&path, "port = 0\n").unwrap();
        assert_eq!(Config::load(&path), Config::default());
    }
}
