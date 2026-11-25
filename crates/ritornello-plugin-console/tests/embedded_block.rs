use ritornello_plugin_sdk::embedded::{scan, Embedded};
use std::fs::File;

/// The real-binary proof that the block's static survives linking and appears
/// exactly once (`scan` answers `None` on two markers). The other plugins are
/// covered by `plugin_catalogue_declaration.rs`, which already requires a
/// `declare_runtime!` call in every plugin's `main.rs`.
#[test]
fn the_built_binary_carries_its_own_version_and_repository() {
    let f = File::open(env!("CARGO_BIN_EXE_ritornello-plugin-console")).unwrap();
    assert_eq!(
        scan(f, u64::MAX),
        Some(Embedded {
            version: env!("CARGO_PKG_VERSION").into(),
            repository: option_env!("CARGO_PKG_REPOSITORY").map(Into::into),
            // From this crate's `[package.metadata.ritornello] kinds`, read
            // at compile time by `declare_runtime!`: what a switched-off
            // console's row shows as its kind.
            kinds: vec!["display".to_string()],
        })
    );
}
