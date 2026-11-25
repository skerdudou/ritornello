//! The one line a plugin's `main` writes to build its runtime.

/// Builds the runtime, deriving everything the announcement must not be asked
/// for.
///
/// `env!` expands **where it is written**, so these two have to be read at the
/// call site: written inside the SDK they would report the SDK's own version
/// and repository, which coincide with the plugin's today only by the accident
/// of a shared workspace — and never for a plugin compiled outside this
/// repository.
///
/// A macro rather than two more parameters, and that is the point: the next
/// derived field costs nothing at ten call sites.
///
/// The block also carries the kinds the plugin's `Cargo.toml` declares in its
/// `[package.metadata.ritornello]` table (`include_str!` of the calling
/// crate's own manifest, read at compile time by
/// [`crate::embedded::manifest_kinds`]), so a switched-off plugin's row can
/// name its kind. Not derived from the registered halves, which exist only
/// at run time; the core's `plugin_catalogue_declaration` guard is what holds
/// that declaration to the announcement.
///
/// The two values are also embedded in the binary as a manifest block (see
/// [`crate::embedded`]), which is what lets the core read a switched-off
/// plugin's version without running it. The runtime is built *from* that
/// block, which keeps the static referenced (a `#[used]` static is kept by the
/// compiler, and the read keeps it honest) and makes the announcement and the
/// block one fact: they cannot disagree.
///
/// `option_env!` and not `env!` for the repository: a crate whose manifest
/// carries no `repository` key still compiles, which is what a minimal
/// third-party plugin looks like. The core has nothing to ask about such a
/// plugin — there is no repository to query — but it does **not** leave its
/// row alone: with nothing announced, the component is judged against the
/// core's own release. Declare `repository` in `Cargo.toml` if your plugin is
/// not ours; `docs/plugins.md` says what happens if you do not.
#[macro_export]
macro_rules! declare_runtime {
    () => {{
        #[used]
        static RITORNELLO_PLUGIN_BLOCK: [u8; $crate::embedded::BLOCK_LEN] =
            $crate::embedded::encode_with_kinds(
                env!("CARGO_PKG_VERSION"),
                option_env!("CARGO_PKG_REPOSITORY"),
                $crate::embedded::manifest_kinds(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/Cargo.toml"
                ))),
            );
        $crate::runtime::Runtime::from_args(&RITORNELLO_PLUGIN_BLOCK)
    }};
}
