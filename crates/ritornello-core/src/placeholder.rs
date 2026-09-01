// Page served when the UI has not been built.
//
// This file is included **textually** by `build.rs` (`include!`) as well as
// compiled as a module of the crate: this is what allows testing the
// fabrication of the placeholder with `cargo test`, whereas Cargo never runs
// the tests of a build script. It must therefore depend on **no** external
// crate.
//
// Note: this module comment is deliberately an ordinary comment (`//`) and
// not an inner doc comment (`//!`). An inner doc comment triggers
// `E0753: expected outer doc comment` once this file is included as is by
// `build.rs` via `include!` — the compiler's restriction concerns the position
// in the host file's token stream, not the source file as read here.

/// Recognizable marker in the placeholder page.
pub const MARKER: &str = "ritornello-ihm-non-construite";

/// Minimal, dependency-free HTML that explains what to run. Better than an
/// `include_str!` macro error on a fresh clone: `cargo build` and `cargo test`
/// stay green without Node installed, and the message is explicit.
pub fn placeholder_html(command: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>ritornello</title></head><body id=\"{MARKER}\">\
         <h1>ritornello</h1>\
         <p>Web interface not built. Run:</p><pre>{command}</pre>\
         </body></html>"
    )
}

/// True if `content` is the placeholder page rather than a real deliverable.
///
/// A **pure** function of the content, hence testable here like the rest of
/// this file, whereas Cargo never runs the tests of a build script.
///
/// It exists for `build.rs`: `cargo::warning` was only emitted at the
/// **creation** of the placeholder. Realistic sequence: fresh clone → bare
/// `cargo build` (placeholder created, warning shown once) → `cross build
/// --release --target armv7…`. Build scripts are replayed per target, but
/// `index.html` now exists — it is the placeholder — so the function returned
/// early and **no warning** was emitted: the release binary silently embedded
/// a "Web interface not built" page.
#[allow(dead_code)] // consumed by build.rs (via `include!`) and by the tests
pub fn is_placeholder(content: &str) -> bool {
    content.contains(MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_is_an_html_page_inviting_to_build_the_ui() {
        let html = placeholder_html("npm run build --workspaces");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("npm run build --workspaces"));
        // No false positive: the placeholder must be recognized for sure.
        assert!(html.contains(MARKER));
    }

    #[test]
    fn is_placeholder_recognizes_the_placeholder_and_not_a_real_deliverable() {
        assert!(is_placeholder(&placeholder_html("npm ci && npm run build --workspaces")));
        // Shape of an `index.html` actually produced by Vite (import map,
        // mount point): no marker, hence no warning.
        let real = "<!doctype html><html><head><script type=\"importmap\">\
                    {\"imports\":{\"vue\":\"/assets/vue.js\"}}</script></head>\
                    <body><div id=\"app\"></div></body></html>";
        assert!(!is_placeholder(real));
        assert!(!is_placeholder(""));
    }
}
