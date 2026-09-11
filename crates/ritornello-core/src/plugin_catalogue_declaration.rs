//! Guard over the per-crate catalogue declaration: each shipped plugin
//! declares, in its own `Cargo.toml`, the kinds it serves and a one-line
//! English description. The release catalogue is built from these, and the
//! installables dialog is what reads it.
//!
//! Why a guard is the first thing written here: a declaration that drifts
//! from what the binary announces does not break anything — it lies. The
//! previous chantier's lesson is explicit about this class, and about when to
//! write the test that links two files describing one set: the day you add to
//! the first one.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn deploy_dir() -> PathBuf {
        repo_root().join("deploy")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    /// Same source `packaging_manifest.rs` and `version_coherence.rs` derive
    /// their own plugin lists from.
    fn declared_plugins() -> Vec<String> {
        let text = read(&deploy_dir().join("plugins.example.toml"));
        text.lines()
            .filter_map(|l| {
                l.trim_end_matches('\r')
                    .strip_prefix("name = \"")?
                    .strip_suffix('"')
            })
            .map(str::to_string)
            .collect()
    }

    /// The `[package.metadata.ritornello]` table a plugin's `Cargo.toml`
    /// carries. Absent entirely, this is the all-empty default, which the
    /// first test below turns into a named failure rather than a panic.
    #[derive(serde::Deserialize, Default)]
    #[serde(deny_unknown_fields)]
    struct Declaration {
        #[serde(default)]
        kinds: Vec<String>,
        #[serde(default)]
        description: String,
    }

    #[derive(serde::Deserialize)]
    struct Manifest {
        package: Package,
    }

    #[derive(serde::Deserialize)]
    struct Package {
        #[serde(default)]
        metadata: Option<Metadata>,
    }

    #[derive(serde::Deserialize)]
    struct Metadata {
        #[serde(default)]
        ritornello: Option<Declaration>,
    }

    fn declaration(name: &str) -> Declaration {
        let path = repo_root()
            .join("crates")
            .join(format!("ritornello-plugin-{name}"))
            .join("Cargo.toml");
        let m: Manifest = toml::from_str(&read(&path))
            .unwrap_or_else(|e| panic!("{} does not parse as TOML: {e}", path.display()));
        m.package.metadata.and_then(|md| md.ritornello).unwrap_or_default()
    }

    /// The slice of `main.rs` that actually registers the plugin's halves:
    /// from the `declare_runtime!()` call up to and including its `.run()`.
    ///
    /// Scoped deliberately, rather than scanning the whole file, because
    /// `.display()` is also an everyday call on a `Path` — `files` alone
    /// calls it seven times for logging, none of them a plugin half. A
    /// whole-file scan would credit `files` and every `metadata` plugin that
    /// logs a path with a `display` kind they do not announce, and the second
    /// test below would then hold a false declaration as correct.
    ///
    /// A single `.find` for `.run(` is enough even though the chain spans
    /// several lines in `mpd` and `musicbrainz`: `&str::find` searches text,
    /// not lines.
    fn registration_chain(text: &str) -> &str {
        let start = text
            .find("declare_runtime!")
            .expect("no declare_runtime! call found");
        let after_start = &text[start..];
        let run_at = after_start
            .find(".run(")
            .expect("declare_runtime! call never reaches .run()");
        &after_start[..run_at + ".run(".len()]
    }

    /// The kinds the registration chain actually calls. `.admin(...)` is not
    /// one of the four needles below, so an admin half already falls out on
    /// its own — no special-casing needed.
    fn announced_kinds(name: &str) -> BTreeSet<String> {
        let path = repo_root()
            .join("crates")
            .join(format!("ritornello-plugin-{name}"))
            .join("src")
            .join("main.rs");
        let chain_source = read(&path);
        let chain = registration_chain(&chain_source);
        [
            (".source(", "source"),
            (".display(", "display"),
            (".input(", "input"),
            (".metadata(", "metadata"),
        ]
        .into_iter()
        .filter(|(needle, _)| chain.contains(needle))
        .map(|(_, kind)| kind.to_string())
        .collect()
    }

    /// Every shipped plugin declares its kinds and a description.
    #[test]
    fn every_shipped_plugin_declares_its_kinds_and_description() {
        for name in declared_plugins() {
            let d = declaration(&name);
            assert!(!d.kinds.is_empty(), "{name} declares no kind");
            assert!(!d.description.trim().is_empty(), "{name} declares no description");
            for k in &d.kinds {
                assert!(
                    ["source", "display", "input", "metadata"].contains(&k.as_str()),
                    "{name} declares the kind {k:?}, which is not one PluginKind serializes to"
                );
            }
        }
    }

    /// The declared kinds are the kinds the binary actually announces.
    ///
    /// Read from the plugin's own source: the announcement is derived from the
    /// builders it calls on its `Runtime` (`.source()`, `.display()`,
    /// `.input()`, `.metadata()`), so those call sites are what a declaration
    /// has to agree with. A crate that grows a second half and forgets this
    /// line would publish a catalogue describing something it is not.
    #[test]
    fn the_declared_kinds_are_the_ones_the_binary_announces() {
        for name in declared_plugins() {
            let declared: BTreeSet<String> = declaration(&name).kinds.into_iter().collect();
            let announced = announced_kinds(&name);
            assert_eq!(
                declared, announced,
                "{name} declares {declared:?} and announces {announced:?}"
            );
        }
    }
}
