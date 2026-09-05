// ESM module served as long as the plugin's UI has not been built.
//
// Included **textually** by `build.rs` (`include!`) as well as compiled as a
// module of the crate: that is what makes it possible to test the making of
// the placeholder with `cargo test`, whereas Cargo never runs the tests of a
// build script. No external dependency allowed here.
//
// Note: this module comment is deliberately an ordinary comment (`//`) and not
// an inner doc comment (`//!`). An inner doc comment triggers
// `E0753: expected outer doc comment` once this file is included as is by
// `build.rs` via `include!` — the compiler's restriction bears on the position
// in the host file's token stream, not on the source file as read here (see
// `ritornello-core/src/placeholder.rs`).

/// Recognizable marker in both placeholder assets. Equivalent of the core's
/// `MARKER` (`ritornello-core/src/placeholder.rs`), which lets `build.rs` tell
/// an already-present placeholder asset from a real deliverable.
pub const MARKER: &str = "ritornello-ihm-plugin-non-construite";

/// Deliberately invalid contract: the shell then shows its "plugin to be
/// rebuilt" message, which describes the situation exactly.
pub fn ui_placeholder_js(command: &str) -> String {
    format!(
        "// {MARKER}\n// IHM non construite. Lancer : {command}\nexport const contract = -1;\n"
    )
}

/// Placeholder style sheet, carrying the same marker: a placeholder `ui.css`
/// left behind a rebuilt `ui.js` would give a UI with no style at all, another
/// silent degradation.
pub fn ui_placeholder_css() -> String {
    format!("/* {MARKER} : IHM non construite */\n")
}

/// True if `content` is a placeholder asset rather than a real deliverable.
///
/// **Pure** function of the content, hence testable here whereas Cargo never
/// runs the tests of a build script. It exists because `cargo::warning` was
/// only emitted at the **creation** of the placeholder: a bare `cargo build`
/// (placeholder created, warning shown once) followed by a
/// `cross build --release --target armv7…` — build scripts are replayed per
/// target, but `ui/dist/ui.js` now exists — said nothing any more, and the
/// release binary embedded the placeholder in silence.
pub fn is_placeholder(content: &str) -> bool {
    content.contains(MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_exports_an_invalid_contract_and_invites_to_build_the_ui() {
        let js = ui_placeholder_js("npm ci && npm run build --workspaces");
        assert!(js.contains("npm ci && npm run build --workspaces"));
        assert!(js.contains("export const contract = -1"));
        // The marker is carried by a JS comment: the module stays valid and
        // loadable, it only announces an invalid contract.
        assert!(js.starts_with("// "));
    }

    #[test]
    fn is_placeholder_recognizes_both_placeholder_assets() {
        assert!(is_placeholder(&ui_placeholder_js("npm ci")));
        assert!(is_placeholder(&ui_placeholder_css()));
    }

    #[test]
    fn is_placeholder_does_not_trigger_on_a_real_deliverable() {
        // Shape of a `ui.js` actually produced by Vite: external imports
        // resolved by the shell's import map, valid contract, default export.
        // No marker, hence no warning.
        let real_js = "import{defineComponent as e}from\"vue\";\
                       import{api as t}from\"@ritornello/ui\";\
                       const o=e({});export const contract=1;export default o;\n";
        assert!(!is_placeholder(real_js));
        let real_css = ".space-y-6>:not([hidden])~:not([hidden]){margin-top:1.5rem}\n";
        assert!(!is_placeholder(real_css));
        assert!(!is_placeholder(""));
    }
}
