// Guarantees that the directory embedded by `rust-embed` exists and contains at
// least one `index.html`. The npm build is **never** invoked here: the
// cross-compilation by `cross` runs in a Docker image without Node, and the
// deliverable is already present on disk there (see `deploy/build.sh`).
include!("src/placeholder.rs");

const DIST: &str = "../../web/app/dist";
const WARNING: &str = "web UI not built: embedding the placeholder instead";

fn main() {
    // We watch here the directory this script sometimes writes into itself
    // (the placeholder, below). Cargo's documentation generally advises
    // against watching a path one modifies oneself, but it is deliberate and
    // safe here: Cargo captures the directory's fingerprint AFTER the complete
    // execution of this script, so the write that follows does not loop back
    // on itself immediately — at worst, the next `cargo build` invocation will
    // notice the content changed (the placeholder just written) and recompile
    // once more, without ever looping indefinitely (once the placeholder is
    // present, `index.html` exists and the function returns before writing
    // anything).
    // Watching only `dist/index.html` rather than the whole directory would be
    // a functional step backwards: it is this wider watch that detects the
    // arrival of a later real npm build (new files under `assets/`) and
    // recompiles accordingly.
    println!("cargo::rerun-if-changed={DIST}");
    println!("cargo::rerun-if-changed=src/placeholder.rs");
    let dist = std::path::Path::new(DIST);
    let index = dist.join("index.html");
    if index.exists() {
        // The file is there, but is it a deliverable or the placeholder written
        // by a previous invocation of this script? The warning was only emitted
        // at the **creation** of the placeholder: a bare `cargo build`
        // (placeholder created, warning shown once) followed by a `cross build
        // --release --target armv7…` — build scripts are replayed per target,
        // but `index.html` now exists — said nothing more, and the release
        // binary silently embedded "Web interface not built". So we re-read the
        // file to re-emit the warning at **every** build as long as the real
        // deliverable is not there.
        if std::fs::read_to_string(&index).is_ok_and(|c| is_placeholder(&c)) {
            println!("cargo::warning={WARNING}");
        }
        return;
    }
    println!("cargo::warning={WARNING}");
    std::fs::create_dir_all(dist).expect("creating web/app/dist");
    std::fs::write(&index, placeholder_html("npm ci && npm run build --workspaces"))
        .expect("writing the placeholder");
}
