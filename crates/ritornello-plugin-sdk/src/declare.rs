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
/// third-party plugin looks like. The core reads that absence as "nothing to
/// say" and leaves the plugin alone.
#[macro_export]
macro_rules! declare_runtime {
    () => {
        $crate::runtime::Runtime::from_args(
            env!("CARGO_PKG_VERSION"),
            option_env!("CARGO_PKG_REPOSITORY"),
        )
    };
}
