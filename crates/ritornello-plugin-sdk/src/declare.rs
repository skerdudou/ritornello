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
/// `option_env!` and not `env!` for the repository: a crate whose manifest
/// carries no `repository` key still compiles, which is what a minimal
/// third-party plugin looks like. The core has nothing to ask about such a
/// plugin — there is no repository to query — but it does **not** leave its
/// row alone: with nothing announced, the component is judged against the
/// core's own release. Declare `repository` in `Cargo.toml` if your plugin is
/// not ours; `docs/plugins.md` says what happens if you do not.
#[macro_export]
macro_rules! declare_runtime {
    () => {
        $crate::runtime::Runtime::from_args(
            env!("CARGO_PKG_VERSION"),
            option_env!("CARGO_PKG_REPOSITORY"),
        )
    };
}
