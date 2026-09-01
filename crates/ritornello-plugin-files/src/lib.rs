//! What the plugin and the root mount binary have in common.
//!
//! Two `[[bin]]` of the same crate do not share their modules: this library is
//! what guarantees that the privileged side and the side that writes the
//! configuration read exactly the same grammar.
//!
//! The modules specific to the plugin (`admin`, `mount`, `state`, `store`) stay
//! declared in `main.rs`: the mount binary has no use for them, and placing them
//! here would impose dependencies that a `oneshot` launched by systemd has no
//! reason to pull in.

pub mod duration;
pub mod explore;
pub mod m3u;
pub mod mount;
pub mod mount_options;
pub mod playlist;
pub mod roots;
pub mod health;
pub mod scan;
pub mod smb;
pub mod store;
pub mod volumes;

// Only compiled under `cargo test`: nothing in this module is used at runtime
// in this crate. It is used by `build.rs` (separate compilation, via
// `include!`) and by its own tests. Compiling it continuously would trigger a
// `dead_code` that `-D warnings` would refuse.
#[cfg(test)]
mod placeholder;

/// Embedded English catalog, fallen back on when the requested locale is missing.
pub const FILES_EN: &str = include_str!("locales/en.toml");

#[cfg(test)]
mod tests {
    /// The **shipped** French pack, read from `deploy/`, and not a test copy:
    /// this very file is the one that will go onto the device.
    fn fr_pack() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/files/fr.toml");
        std::fs::read_to_string(p).expect("shipped fr pack")
    }

    #[test]
    fn key_parity_between_embedded_en_and_the_fr_pack() {
        // A key present on one side only would show English in the middle of
        // French, without warning: `Catalog::load` silently falls back on the
        // embedded one key by key.
        let en = ritornello_i18n::try_parse(crate::FILES_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&fr_pack()).unwrap();
        let mut en_keys: Vec<&String> = en.keys().collect();
        let mut fr_keys: Vec<&String> = fr.keys().collect();
        en_keys.sort();
        fr_keys.sort();
        assert_eq!(en_keys, fr_keys, "en/fr key sets diverge");
    }
}
