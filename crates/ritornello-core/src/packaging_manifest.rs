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
        /// Units the installer enables after placing them. Each must be one
        /// of this component's own `tree` destinations under
        /// `etc/systemd/system/`.
        enable: Vec<String>,
        /// Where the component mounts things (the files plugin's shares).
        mount_root: Option<String>,
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

    /// The core's own list of privileged plugins
    /// (`crate::plugins::PRIVILEGED_PLUGINS`) and this manifest must agree in
    /// **both directions**: a plugin whose entry places a privileged file
    /// (an `extra_binaries` entry, or a `tree` destination under
    /// `etc/systemd/system/` or `etc/polkit-1/rules.d/`) but is not in the
    /// list would be uninstallable from the UI in name only — the route
    /// refusal in `plugin_status::plugin_delete` reads the list, not this
    /// file, so a plugin missing from it would still have its declaration
    /// erased and its privileged parts left behind, the exact defect this
    /// whole change closes. The reverse drift is just as real: a plugin
    /// named in the list that places nothing privileged would be needlessly
    /// sent to `ritornello-install` for an ordinary uninstall it could do
    /// itself.
    ///
    /// Only `m.plugins` is walked, deliberately: `PRIVILEGED_PLUGINS` never
    /// names `core` (it is not a plugin, and a third-party plugin can never
    /// be privileged either — see the constant's own doc), so the core's
    /// entry would only ever be a false positive here.
    #[test]
    fn every_privileged_plugin_agrees_with_packaging_toml() {
        let m = manifest();
        let mut checked = 0;
        for (name, c) in &m.plugins {
            let places_privileged_file = !c.extra_binaries.is_empty()
                || c.tree.iter().any(|e| {
                    e.to.starts_with("etc/systemd/system/") || e.to.starts_with("etc/polkit-1/rules.d/")
                });
            let listed = crate::plugins::PRIVILEGED_PLUGINS.contains(&name.as_str());
            assert_eq!(
                listed, places_privileged_file,
                "{name}: packaging.toml places a privileged file = {places_privileged_file}, but \
                 PRIVILEGED_PLUGINS lists it as privileged = {listed} -- {}",
                if places_privileged_file {
                    format!(
                        "add \"{name}\" to PRIVILEGED_PLUGINS in crates/ritornello-core/src/plugins/mod.rs"
                    )
                } else {
                    format!(
                        "remove \"{name}\" from PRIVILEGED_PLUGINS, or its packaging.toml entry is \
                         missing the privileged file that justified adding it"
                    )
                }
            );
            checked += 1;
        }
        // Fix round 1, I2: the loop above only ever visits a name that
        // already has a `[plugins.X]` table, so a stale or mistyped entry in
        // `PRIVILEGED_PLUGINS` — `files` renamed or removed from
        // `packaging.toml` while a correct new entry is added elsewhere, or
        // a typo such as `"file"` appended next to `"files"` — was never
        // visited and the test stayed green. Walking `PRIVILEGED_PLUGINS`
        // itself and asserting each name is a key of `m.plugins` is what
        // catches that: the direction the loop above cannot reach, because
        // it has nothing to iterate over for a name with no table at all.
        for name in crate::plugins::PRIVILEGED_PLUGINS {
            assert!(
                m.plugins.contains_key(*name),
                "PRIVILEGED_PLUGINS names {name:?}, which has no [plugins.{name}] table in \
                 packaging.toml at all -- remove {name:?} from PRIVILEGED_PLUGINS in \
                 crates/ritornello-core/src/plugins/mod.rs, or add its packaging.toml entry"
            );
            checked += 1;
        }
        assert!(checked > 0, "checked nothing — the walk is not looking where it should");
    }

    /// **No component archive carries translated text any more.**
    ///
    /// The rule with no list to keep: a component that shipped its own
    /// `fr.toml` AND a language pack that ships the same file would fight
    /// over one path on disk, and reinstalling that component would move a
    /// translation *backwards*. Refusing the shape outright is what makes
    /// that impossible rather than merely unlikely.
    #[test]
    fn no_component_archive_carries_a_locale_directory() {
        let m = manifest();
        let mut checked = 0;
        for (name, c) in std::iter::once(("core".to_string(), &m.core))
            .chain(m.plugins.iter().map(|(k, v)| (k.clone(), v)))
        {
            for entry in &c.tree {
                assert!(
                    !entry.from.starts_with("deploy/locales"),
                    "{name} still ships {} -- translated text belongs to a language pack now",
                    entry.from
                );
                assert!(
                    !entry.to.contains("etc/ritornello/locales"),
                    "{name} still writes into etc/ritornello/locales -- no component may ever ship \
                     translated text there: {}",
                    entry.to
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "checked nothing — the walk is not looking where it should");
    }

    /// A unit named in `enable` that the same component does not place would
    /// have the installer run `systemctl enable` on a unit that is absent —
    /// or, worse, on one another component owns, which removing this one
    /// would then leave enabled. And a relative `mount_root` would be
    /// resolved against whatever directory the installer happens to run in,
    /// the one path it unmounts and removes under.
    #[test]
    fn every_enabled_unit_is_placed_by_its_own_component() {
        let m = manifest();
        let mut checked = 0;
        for (name, c) in std::iter::once(("core", &m.core)).chain(m.plugins.iter().map(|(k, v)| (k.as_str(), v))) {
            for unit in &c.enable {
                assert!(
                    c.tree.iter().any(|e| e.to == format!("etc/systemd/system/{unit}")),
                    "{name} enables {unit}, which its own tree does not place under etc/systemd/system/"
                );
                checked += 1;
            }
            if let Some(root) = &c.mount_root {
                assert!(
                    root.starts_with('/') && root.len() > 1 && !root.contains(".."),
                    "{name}: mount_root {root:?} must be an absolute path other than /"
                );
                checked += 1;
            }
        }
        assert!(checked >= 3, "checked only {checked} enable/mount_root entries — the walk is wrong");
    }

    fn run_install_inventory(args: &[&str]) -> std::process::Output {
        std::process::Command::new("python3")
            .arg("scripts/install-inventory.py")
            .args(args)
            .current_dir(repo_root())
            .output()
            .expect("python3 is available: package-release.sh already needs it, here and in CI")
    }

    /// The inventory published with each release (`inventory.json`) and the
    /// archives it describes are produced from the same `entries()` of
    /// `scripts/packaging.py`; the script's self-test stages every component
    /// and compares. Run here because the script's only other exercise is
    /// the `publish` job, which fires on a tag.
    #[test]
    fn the_install_inventory_agrees_with_what_the_archives_carry() {
        let out = run_install_inventory(&["--self-test"]);
        assert!(
            out.status.success(),
            "install-inventory.py --self-test failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// What `ritornello-install` will act on, read the way it will read it:
    /// the reference order, the files plugin's unit and mount root, the
    /// presets expanded file by file, and no privileged file anywhere an
    /// update could reach.
    #[test]
    fn the_install_inventory_says_what_the_installer_needs() {
        let out = run_install_inventory(&[]);
        assert!(out.status.success(), "install-inventory.py failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let inv: serde_json::Value = serde_json::from_slice(&out.stdout).expect("inventory.json is JSON");
        assert_eq!(inv["format"], 1);
        let order: Vec<String> = inv["reference_order"]
            .as_array()
            .expect("reference_order is an array")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(order, declared_plugins());
        let plugins = inv["plugins"].as_array().expect("plugins is an array");
        let plugin = |name: &str| {
            plugins
                .iter()
                .find(|p| p["name"] == name)
                .unwrap_or_else(|| panic!("no {name} in the inventory"))
        };
        let files = plugin("files");
        assert_eq!(files["mount_root"], "/mnt/ritornello");
        assert_eq!(files["enable"], serde_json::json!(["ritornello-media-mount.service"]));

        // Every preset, one entry per file, owned by the unprivileged core
        // that rewrites them on update.
        let presets_dir = deploy_dir().join("input-presets");
        let gi_files = plugin("generic-input")["files"].as_array().unwrap();
        let mut presets = 0;
        for entry in std::fs::read_dir(&presets_dir).unwrap().flatten() {
            let dest = format!("/etc/ritornello/input-presets/{}", entry.file_name().to_string_lossy());
            let f = gi_files
                .iter()
                .find(|f| f["dest"] == dest.as_str())
                .unwrap_or_else(|| panic!("generic-input's inventory does not place {dest}"));
            assert_eq!(f["owner"], "ritornello:ritornello", "{dest}");
            presets += 1;
        }
        assert!(presets > 0, "read no preset at all — the walk is wrong");

        let mut privileged = 0;
        for c in std::iter::once(&inv["core"]).chain(plugins.iter()) {
            for f in c["files"].as_array().unwrap() {
                let dest = f["dest"].as_str().unwrap();
                assert!(
                    !dest.starts_with("/etc/ritornello/locales"),
                    "{}: {dest} — translated text belongs to a language pack",
                    c["name"]
                );
                if f["privileged"] != true {
                    continue;
                }
                let root_run_binary = f["mode"] == "0755"
                    && dest.starts_with("/usr/local/lib/ritornello/")
                    && !dest.starts_with("/usr/local/lib/ritornello/plugins/");
                assert!(
                    dest.starts_with("/etc/systemd/system/")
                        || dest.starts_with("/etc/polkit-1/rules.d/")
                        || root_run_binary,
                    "{}: {dest} is privileged but is neither a unit, a polkit rule, nor a root-run \
                     binary outside the plugins directory",
                    c["name"]
                );
                privileged += 1;
            }
        }
        assert!(privileged >= 9, "saw only {privileged} privileged files — the walk is wrong");
    }

    /// Fix round 1, R14: `deploy/mpd.example.toml` carried a literal
    /// `</content>` line — an editor artefact, not TOML — that would have
    /// been copied verbatim into `mpd`'s own data directory by `deploy.sh`
    /// on a fresh install, or by an update installing the plugin for the
    /// first time (`initial_config`), leaving the plugin unable to parse its
    /// own configuration. Every `deploy/*.example.toml` must parse as TOML;
    /// `toml::Value` rather than a typed struct, since each file has its own
    /// shape and this guard only cares that the file is well-formed, not
    /// what it means.
    ///
    /// **[MUTATION]**: put the `</content>` line back at the end of
    /// `deploy/mpd.example.toml` — this test fails, naming that file.
    #[test]
    fn every_example_toml_parses() {
        let dir = deploy_dir();
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let is_example_toml = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".example.toml"));
            if !is_example_toml {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            toml::from_str::<toml::Value>(&text)
                .unwrap_or_else(|e| panic!("{} does not parse as TOML: {e}", path.display()));
            checked += 1;
        }
        assert!(checked > 0, "checked no *.example.toml file at all — the walk is wrong");
    }
}
