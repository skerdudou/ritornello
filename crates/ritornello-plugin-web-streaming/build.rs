// Guarantees the existence of `ui/dist/{ui.js,ui.css}` embedded by
// `include_str!`. The npm build is **never** invoked here (see
// `deploy/build.sh`): cross-compilation runs in an image without Node.
include!("src/placeholder.rs");

// Writes `path` with `content` if it is absent; if it is already there, rereads
// it and re-emits the warning as long as it is still the placeholder.
//
// This is the point that was fixed: `cargo::warning` used to be emitted only at
// the placeholder's **creation**. Fresh clone -> bare `cargo build` (placeholder
// created, warning shown once) -> `cross build --release --target armv7...`:
// build scripts are replayed per target, but `ui/dist/ui.js` now exists
// (it's the placeholder), so no warning shows **at all** anymore -- and the
// release binary silently embedded the placeholder. The warning names the
// file: two placeholder activations give two distinct, actionable lines.
fn ensure(path: &std::path::Path, content: impl FnOnce() -> String) {
    let warn = || {
        println!(
            "cargo::warning=plugin UI not built: embedding the placeholder instead ({})",
            path.display()
        );
    };
    match std::fs::read_to_string(path) {
        Ok(existing) => {
            if is_placeholder(&existing) {
                warn();
            }
        }
        Err(_) => {
            warn();
            std::fs::write(path, content()).unwrap();
        }
    }
}

fn main() {
    println!("cargo::rerun-if-changed=ui/dist");
    println!("cargo::rerun-if-changed=src/placeholder.rs");
    let dist = std::path::Path::new("ui/dist");
    std::fs::create_dir_all(dist).expect("creating ui/dist");
    ensure(&dist.join("ui.js"), || {
        ui_placeholder_js("npm ci && npm run build --workspaces")
    });
    ensure(&dist.join("ui.css"), ui_placeholder_css);
}
