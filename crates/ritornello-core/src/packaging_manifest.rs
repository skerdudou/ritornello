//! Guard over deploy/packaging.toml: it says what each release archive
//! carries, and nothing else says it. A path that stops existing, or a plugin
//! added without an entry, must be a red test here — not an incomplete archive
//! discovered on the device weeks later.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// Typed rather than walked as a generic `toml::Value`, for two reasons:
    /// it is this repository's idiom everywhere else, and `deny_unknown_fields`
    /// turns a mistyped key into an error. Without it, `exemples = [...]`
    /// would deserialize silently into nothing and ship an archive missing the
    /// file it was meant to carry — the exact class of quiet failure this
    /// manifest exists to end.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Packaging {
        core: Component,
        plugins: HashMap<String, Component>,
    }

    #[derive(serde::Deserialize, Default)]
    #[serde(default, deny_unknown_fields)]
    struct Component {
        tree: Vec<TreeEntry>,
        extra_binaries: Vec<ExtraBinary>,
        examples: Vec<String>,
        /// Files a fresh install starts from, written by the core only when
        /// the target is absent. The same paths may also be listed in
        /// `examples`; `packaging.py` then ships one copy, under
        /// `initial-config/`.
        ///
        /// No `#[serde(default)]` of its own: the container above already
        /// carries one.
        initial_config: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TreeEntry {
        from: String,
        to: String,
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ExtraBinary {
        /// A cargo binary name, NOT a repository path — deliberately never
        /// checked against the source tree.
        #[allow(dead_code)]
        name: String,
        to: String,
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn deploy_dir() -> PathBuf {
        repo_root().join("deploy")
    }

    fn manifest() -> Packaging {
        let p = deploy_dir().join("packaging.toml");
        toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    }

    fn declared_plugins() -> Vec<String> {
        let p = deploy_dir().join("plugins.example.toml");
        let text = std::fs::read_to_string(&p).unwrap();
        text.lines()
            .filter_map(|l| l.strip_prefix("name = \""))
            .filter_map(|l| l.strip_suffix('"'))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_launched_plugin_has_a_packaging_entry() {
        // Derived from the same file deploy.sh derives its list from, so a
        // plugin added in a hurry cannot ship without an archive — and an
        // entry naming a plugin nothing launches cannot linger either.
        let m = manifest();
        let declared = declared_plugins();
        assert!(!declared.is_empty(), "read no plugin at all — the parsing is wrong, not the manifest");
        for name in &declared {
            assert!(m.plugins.contains_key(name), "no packaging entry for plugin {name}");
        }
        for name in m.plugins.keys() {
            assert!(declared.contains(name), "packaging entry {name} names a plugin nothing launches");
        }
    }

    #[test]
    fn every_path_named_by_the_manifest_exists() {
        // The failure this prevents is silent: a renamed unit file would still
        // build a perfectly valid archive, with one file missing.
        let m = manifest();
        let root = repo_root();
        let mut checked = 0;
        let mut check = |rel: &str| {
            assert!(root.join(rel).exists(), "packaging.toml names a path that does not exist: {rel}");
            checked += 1;
        };
        for c in std::iter::once(&m.core).chain(m.plugins.values()) {
            for e in &c.tree {
                check(&e.from);
            }
            for e in &c.examples {
                check(e);
            }
            for e in &c.initial_config {
                check(e);
            }
        }
        assert!(checked > 0, "checked nothing — the walk is not looking where it should");
    }

    #[test]
    fn every_privileged_file_a_release_carries_is_also_placed_by_deploy_sh() {
        // The two installation paths must agree on the privileged files, and
        // nothing else makes them: `packaging.toml` says what a release
        // archive carries, `deploy.sh` says what an SSH deployment places,
        // and they are written months apart. The auto-update work added four
        // files to the first and none to the second, which left every
        // development device checking for updates and refusing to install
        // one — `Access denied`, naming nothing.
        //
        // Only units, polkit rules and the binaries systemd runs as root are
        // held to this. Ordinary plugin binaries are derived from
        // plugins.example.toml by both sides already, and locale directories
        // are copied wholesale.
        let m = manifest();
        let deploy = std::fs::read_to_string(deploy_dir().join("deploy.sh")).unwrap();
        let mut checked = 0;
        for c in std::iter::once(&m.core).chain(m.plugins.values()) {
            for e in &c.tree {
                if !e.to.starts_with("etc/systemd/system/")
                    && !e.to.starts_with("etc/polkit-1/rules.d/")
                {
                    continue;
                }
                assert!(
                    deploy.contains(&e.from),
                    "{} ships in a release archive and deploy.sh never places it",
                    e.from
                );
                checked += 1;
            }
            for e in &c.extra_binaries {
                // The destination path, not the binary name: `ritornello-update`
                // is a substring of `ritornello-update.service`, so a name
                // search would pass on the strength of the unit alone.
                assert!(
                    deploy.contains(&format!("/{}", e.to)),
                    "{} is run as root and deploy.sh never installs it",
                    e.to
                );
                checked += 1;
            }
        }
        assert!(checked >= 9, "checked only {checked} privileged files — the walk is wrong");
    }

    #[test]
    fn a_plugin_without_locales_is_normal() {
        // Four plugins have no catalog of their own. Asserting it here stops
        // a future guard from "fixing" their absence into an error.
        for name in ["ouifm-metas", "radiofrance-metas", "nrj-metas", "console"] {
            assert!(
                !deploy_dir().join("locales").join(name).exists(),
                "{name} grew a locale directory: the packaging rule must now carry it"
            );
        }
    }
}
