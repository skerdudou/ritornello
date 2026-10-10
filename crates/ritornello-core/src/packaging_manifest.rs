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
        /// Components that ship beside a plugin (`with`), in an archive of
        /// their own, placed only by `ritornello-install`: where a plugin's
        /// privileged files live. See the comment above `[companions]` in
        /// packaging.toml.
        companions: HashMap<String, Component>,
    }

    impl Packaging {
        /// Every section, named: the core, each plugin, each companion. The
        /// walks that hold for any archive go through this, so a companion
        /// cannot escape a rule by being the newest kind of section.
        fn all(&self) -> Vec<(&str, &Component)> {
            std::iter::once(("core", &self.core))
                .chain(self.plugins.iter().map(|(k, v)| (k.as_str(), v)))
                .chain(self.companions.iter().map(|(k, v)| (k.as_str(), v)))
                .collect()
        }
    }

    /// Whether a section places a file only a privileged install can place:
    /// a root-run binary outside the plugins directory (`extra_binaries`), or
    /// a `tree` destination that is a systemd unit, a polkit rule, or a
    /// root-run location (`usr/local/bin/`, or `usr/local/lib/ritornello/`
    /// outside `plugins/`). A tree entry there would be a root-run file
    /// under another name, and a plugin archive carrying it one the web UI
    /// must refuse. The same rule as `install-inventory.py`'s `privileged`
    /// flag.
    fn places_privileged_file(c: &Component) -> bool {
        !c.extra_binaries.is_empty() || c.tree.iter().any(|e| privileged_dest(&e.to))
    }

    fn privileged_dest(to: &str) -> bool {
        to.starts_with("etc/systemd/system/")
            || to.starts_with("etc/polkit-1/rules.d/")
            || to.starts_with("usr/local/bin/")
            || (to.starts_with("usr/local/lib/ritornello/") && !to.starts_with("usr/local/lib/ritornello/plugins/"))
    }

    #[derive(serde::Deserialize, Default)]
    #[serde(default, deny_unknown_fields)]
    struct Component {
        /// The plugin a companion ships beside. Set on every
        /// `[companions.X]` and on nothing else
        /// (`with_is_set_exactly_on_companions`), so that one struct can
        /// describe all three kinds of section and every walk below reads
        /// them alike.
        with: Option<String>,
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
        // Derived from the same file package-release.sh derives its list from, so a
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
        for (_, c) in m.all() {
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

    /// The core's own list of privileged plugins
    /// (`crate::plugins::PRIVILEGED_PLUGINS`) and this manifest must agree in
    /// **both directions**. A plugin is privileged exactly when some
    /// companion declares `with = <that plugin>` and places a privileged file
    /// (an `extra_binaries` entry, or a `tree` destination under
    /// `etc/systemd/system/` or `etc/polkit-1/rules.d/`): the plugin's own
    /// archive carries none any more, but installing or removing the plugin
    /// still installs or removes its companion, which only
    /// `ritornello-install` can place.
    ///
    /// A plugin whose companion places a privileged file but which is not in
    /// the list would be uninstallable from the UI in name only — the route
    /// refusal in `plugin_status::plugin_delete` reads the list, not this
    /// file, so a plugin missing from it would still have its declaration
    /// erased and its companion's privileged parts left behind. The reverse
    /// drift is just as real: a plugin named in the list with no privileged
    /// companion would be needlessly sent to `ritornello-install` for an
    /// ordinary uninstall it could do itself.
    ///
    /// And a `[plugins.X]` table itself must never place a privileged file:
    /// that is a companion's job now. A plugin archive carrying a unit, a
    /// rule or a root-run binary is one the web UI must refuse to install as
    /// an update, which is the very situation companions exist to end.
    ///
    /// Only `m.plugins` is walked for the list, deliberately:
    /// `PRIVILEGED_PLUGINS` never names `core` (it is not a plugin, and a
    /// third-party plugin can never be privileged either — see the
    /// constant's own doc), so the core's entry would only ever be a false
    /// positive here.
    ///
    /// **[MUTATION]**, one per direction and one for the plugin-table rule:
    /// - remove `"files"` from `PRIVILEGED_PLUGINS` — red, "add \"files\"";
    /// - add `"radio"` to `PRIVILEGED_PLUGINS` — red, "remove \"radio\"";
    /// - move the unit's `tree` entry back into `[plugins.files]` — red,
    ///   "places the privileged".
    #[test]
    fn every_privileged_plugin_agrees_with_packaging_toml() {
        let m = manifest();
        let mut checked = 0;
        for (name, c) in &m.plugins {
            assert!(
                !places_privileged_file(c),
                "[plugins.{name}] places the privileged file(s) {:?} -- a plugin's own archive \
                 never carries a unit, a polkit rule or a root-run binary: move them to a \
                 [companions.X] with `with = \"{name}\"`",
                c.extra_binaries
                    .iter()
                    .map(|e| e.to.as_str())
                    .chain(c.tree.iter().map(|e| e.to.as_str()))
                    .collect::<Vec<_>>()
            );
            let places_privileged_file = m
                .companions
                .values()
                .any(|k| k.with.as_deref() == Some(name.as_str()) && places_privileged_file(k));
            let listed = crate::plugins::PRIVILEGED_PLUGINS.contains(&name.as_str());
            assert_eq!(
                listed, places_privileged_file,
                "{name}: a companion with = \"{name}\" places a privileged file = \
                 {places_privileged_file}, but PRIVILEGED_PLUGINS lists it as privileged = \
                 {listed} -- {}",
                if places_privileged_file {
                    format!(
                        "add \"{name}\" to PRIVILEGED_PLUGINS in crates/ritornello-core/src/plugins/mod.rs"
                    )
                } else {
                    format!(
                        "remove \"{name}\" from PRIVILEGED_PLUGINS, or its companion in packaging.toml \
                         is missing the privileged file that justified adding it"
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

    /// The core's own list of companions (`crate::plugins::COMPANIONS`) and
    /// this manifest's `[companions.X] with = …` must name the same pairs, in
    /// **both directions**.
    ///
    /// A companion packaged here and missing from the list is the dangerous
    /// drift: the core would not know `files` ships with one, and would update
    /// it from the web UI whatever the release did to the companion — a new
    /// plugin binary beside an old root-run helper, which is exactly what the
    /// version comparison exists to refuse. The reverse drift is a pair the
    /// core waits for and no release ever publishes, so that plugin could
    /// never be updated from the UI again.
    ///
    /// **[MUTATION]**, one per direction:
    /// - empty `COMPANIONS` — red, "add (\"files\", \"files-mount\")";
    /// - add `("radio", "radio-helper")` to `COMPANIONS` — red, "remove".
    #[test]
    fn every_companion_agrees_with_packaging_toml() {
        let m = manifest();
        let mut packaged: Vec<(String, String)> = m
            .companions
            .iter()
            .map(|(name, c)| {
                let with = c.with.clone().unwrap_or_else(|| panic!("[companions.{name}] declares no `with`"));
                (with, name.clone())
            })
            .collect();
        packaged.sort();
        let mut listed: Vec<(String, String)> =
            crate::plugins::COMPANIONS.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        listed.sort();
        for pair in &packaged {
            assert!(
                listed.contains(pair),
                "packaging.toml ships [companions.{}] with = {:?}, which COMPANIONS does not list -- \
                 add ({:?}, {:?}) to COMPANIONS in crates/ritornello-core/src/plugins/mod.rs",
                pair.1, pair.0, pair.0, pair.1
            );
        }
        for pair in &listed {
            assert!(
                packaged.contains(pair),
                "COMPANIONS lists ({:?}, {:?}), which packaging.toml does not ship as \
                 [companions.{}] with = {:?} -- remove it from COMPANIONS, or add the table",
                pair.0, pair.1, pair.1, pair.0
            );
        }
        assert!(!listed.is_empty(), "checked nothing — the files plugin's companion is gone from both");
    }

    /// `with` is what makes a section a companion, and it must name a plugin
    /// this manifest packages: a companion whose `with` names nothing (a
    /// typo, a renamed plugin) would ship an archive that no installation of
    /// any plugin ever places. And `with` anywhere else would mean nothing,
    /// so it is refused there rather than silently ignored.
    ///
    /// **[MUTATION]**: set the companion's `with = "file"` — red, naming it.
    #[test]
    fn with_is_set_exactly_on_companions() {
        let m = manifest();
        assert!(!m.companions.is_empty(), "no companion at all: the files plugin's helper has no home");
        for (name, c) in &m.companions {
            let with = c.with.as_deref().unwrap_or_else(|| panic!("[companions.{name}] declares no `with`"));
            assert!(
                m.plugins.contains_key(with),
                "[companions.{name}] ships with {with:?}, which has no [plugins.{with}] table"
            );
        }
        for (name, c) in std::iter::once(("core", &m.core)).chain(m.plugins.iter().map(|(k, v)| (k.as_str(), v))) {
            assert!(c.with.is_none(), "{name} declares `with`, which only a companion may");
        }
    }

    /// A companion is not a plugin, so the catalogue, which lists what the
    /// web UI can offer to install, must never describe one: a row for the
    /// mount helper would be an offer only `ritornello-install` can honour.
    /// `plugin-catalogue.py` reads `plugins.example.toml` and never this
    /// file; this pins that it stays so.
    #[test]
    fn the_catalogue_describes_no_companion() {
        let out = std::process::Command::new("python3")
            .arg("scripts/plugin-catalogue.py")
            .current_dir(repo_root())
            .output()
            .expect("python3 is available");
        assert!(out.status.success(), "plugin-catalogue.py failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let catalogue: serde_json::Value = serde_json::from_slice(&out.stdout).expect("catalogue.json is JSON");
        let components = catalogue["components"].as_object().expect("components is an object");
        assert!(components.contains_key("files"), "the walk is wrong: not even files is described");
        let m = manifest();
        for name in m.companions.keys() {
            for key in [name.clone(), format!("ritornello-{name}")] {
                assert!(!components.contains_key(&key), "the catalogue describes the companion {key}");
            }
        }
    }

    /// Runs `plugin-catalogue.py --shipped <names>` from the repo root and
    /// returns the process output.
    fn catalogue_shipping(names: &[&str]) -> std::process::Output {
        let dir = tempfile::tempdir().expect("a temp dir");
        let shipped = dir.path().join("changed.txt");
        std::fs::write(&shipped, names.join("\n") + "\n").expect("changed.txt is written");
        std::process::Command::new("python3")
            .arg("scripts/plugin-catalogue.py")
            .arg("--shipped")
            .arg(&shipped)
            .current_dir(repo_root())
            .output()
            .expect("python3 is available")
    }

    fn contracts_of(out: &std::process::Output) -> serde_json::Map<String, serde_json::Value> {
        assert!(out.status.success(), "plugin-catalogue.py failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let catalogue: serde_json::Value = serde_json::from_slice(&out.stdout).expect("catalogue.json is JSON");
        catalogue["contracts"].as_object().expect("contracts is an object").clone()
    }

    /// `contracts` is what this release ships and nothing else: an archive
    /// carried by an older release must never be described with the current
    /// tree's numbers. A companion and a language pack speak no wire.
    #[test]
    fn the_catalogue_publishes_contracts_only_for_what_the_release_ships() {
        let out = catalogue_shipping(&[
            "ritornello-core",
            "ritornello-plugin-radio",
            "ritornello-files-mount",
            "ritornello-lang-de",
        ]);
        let contracts = contracts_of(&out);
        let mut keys: Vec<&str> = contracts.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["core", "radio"]);
    }

    /// The published numbers are the code's: the core entry is exactly what
    /// `Speaks::this_core()` serializes to, and each plugin's is what its
    /// own `[package.metadata.ritornello]` implies with the current
    /// constants (one contract per kind, plus `admin` iff it declares it).
    #[test]
    fn the_catalogue_s_contracts_are_the_code_s() {
        use crate::compat::Speaks;
        use ritornello_proto::{Contract, PluginKind};

        let m = manifest();
        let archives: Vec<String> = std::iter::once("ritornello-core".to_string())
            .chain(m.plugins.keys().map(|p| format!("ritornello-plugin-{p}")))
            .collect();
        let refs: Vec<&str> = archives.iter().map(String::as_str).collect();
        let contracts = contracts_of(&catalogue_shipping(&refs));
        assert_eq!(contracts.len(), archives.len(), "one entry per shipped component");

        assert_eq!(contracts["core"], serde_json::to_value(Speaks::this_core()).unwrap());

        for name in m.plugins.keys() {
            let manifest_path = repo_root().join("crates").join(format!("ritornello-plugin-{name}")).join("Cargo.toml");
            let cargo: toml::Value = toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
            let meta = &cargo["package"]["metadata"]["ritornello"];
            let mut expected = std::collections::BTreeMap::new();
            for kind in meta["kinds"].as_array().expect("kinds is an array") {
                let kind: PluginKind = serde_json::from_value(serde_json::json!(kind.as_str().unwrap())).unwrap();
                let c = Contract::of_kind(kind);
                expected.insert(c, c.current());
            }
            if meta.get("admin").and_then(toml::Value::as_bool).unwrap_or(false) {
                expected.insert(Contract::Admin, Contract::Admin.current());
            }
            let expected = Speaks { protocol: ritornello_proto::PROTOCOL_VERSION, contracts: expected };
            assert_eq!(contracts[name.as_str()], serde_json::to_value(expected).unwrap(), "contracts of {name}");
        }
    }

    /// A name the script cannot place is a release it must refuse to
    /// describe, not one it silently skips.
    #[test]
    fn an_unknown_shipped_name_fails_the_catalogue() {
        let out = catalogue_shipping(&["ritornello-core", "ritornello-plugin-no-such-plugin"]);
        assert!(!out.status.success(), "an unknown shipped name was accepted");
        assert!(String::from_utf8_lossy(&out.stderr).contains("no-such-plugin"));
    }

    /// A wire constant the script cannot read is a release it must refuse
    /// to describe: a catalogue with a guessed number would be believed.
    #[test]
    fn an_unreadable_constant_fails_the_catalogue() {
        let src = tempfile::tempdir().expect("a temp dir");
        let proto = repo_root().join("crates/ritornello-proto/src");
        let lib = std::fs::read_to_string(proto.join("lib.rs")).unwrap();
        std::fs::write(src.path().join("lib.rs"), lib).unwrap();
        let contract = std::fs::read_to_string(proto.join("contract.rs")).unwrap();
        let mangled = contract.replace("pub const INPUT_CONTRACT", "pub const INPUT_CONTRACT_GONE");
        assert_ne!(contract, mangled, "the mutation changed nothing");
        std::fs::write(src.path().join("contract.rs"), mangled).unwrap();
        let shipped = src.path().join("changed.txt");
        std::fs::write(&shipped, "ritornello-core\n").unwrap();
        let out = std::process::Command::new("python3")
            .arg("scripts/plugin-catalogue.py")
            .arg("--shipped")
            .arg(&shipped)
            .arg("--proto-src")
            .arg(src.path())
            .current_dir(repo_root())
            .output()
            .expect("python3 is available");
        assert!(!out.status.success(), "an unreadable constant was accepted");
        assert!(String::from_utf8_lossy(&out.stderr).contains("INPUT_CONTRACT"));
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
        for (name, c) in m.all() {
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
        for (name, c) in m.all() {
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
    /// the reference order, the files-mount companion's unit and mount root
    /// (and the files plugin's entry holding none of it), the presets
    /// expanded file by file, and no privileged file anywhere an update
    /// could reach.
    #[test]
    fn the_install_inventory_says_what_the_installer_needs() {
        let out = run_install_inventory(&[]);
        assert!(out.status.success(), "install-inventory.py failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let inv: serde_json::Value = serde_json::from_slice(&out.stdout).expect("inventory.json is JSON");
        assert_eq!(inv["format"], 2);
        // Every privileged file of the core says which content it is, in the
        // two shapes the core compares: the hash of a unit or a rule as the
        // repository holds it, the updater's own number for its binary.
        for f in inv["core"]["files"].as_array().unwrap() {
            let dest = f["dest"].as_str().unwrap();
            let identity = f.get("identity").and_then(|i| i.as_str());
            if f["privileged"] != true {
                assert_eq!(identity, None, "{dest}: only a privileged file carries an identity");
                continue;
            }
            let want = if dest == "/usr/local/lib/ritornello/ritornello-update" {
                let cargo: toml::Value = toml::from_str(
                    &std::fs::read_to_string(repo_root().join("crates/ritornello-updater/Cargo.toml")).unwrap(),
                )
                .unwrap();
                format!("version:{}", cargo["package"]["version"].as_str().expect("the updater declares its own version"))
            } else {
                let source = deploy_dir().join(dest.rsplit('/').next().unwrap());
                // As git stores it: the checkout of a Windows machine may
                // write CRLF, the release is built from LF.
                let bytes = std::fs::read(&source).unwrap_or_else(|e| panic!("{}: {e}", source.display()));
                let lf: Vec<u8> = bytes.into_iter().filter(|b| *b != b'\r').collect();
                format!("sha256:{}", crate::update::download::digest_hex(&lf))
            };
            assert_eq!(identity, Some(want.as_str()), "{dest}");
        }
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
        let companions = inv["companions"].as_array().expect("companions is an array");
        let companion = |name: &str| {
            companions
                .iter()
                .find(|c| c["name"] == name)
                .unwrap_or_else(|| panic!("no companion {name} in the inventory"))
        };
        let mount = companion("files-mount");
        assert_eq!(mount["with"], "files");
        assert_eq!(mount["mount_root"], "/mnt/ritornello");
        assert_eq!(mount["enable"], serde_json::json!(["ritornello-media-mount.service"]));
        assert_eq!(mount["block"], serde_json::Value::Null);
        assert_eq!(mount["initial_config"], serde_json::json!([]));
        // Named after its own crate and its own version, as
        // package-release.sh builds it.
        let cargo: toml::Value = toml::from_str(
            &std::fs::read_to_string(repo_root().join("crates/ritornello-files-mount/Cargo.toml")).unwrap(),
        )
        .unwrap();
        let version = cargo["package"]["version"].as_str().expect("the companion declares its own version");
        assert_eq!(mount["archive"], format!("ritornello-files-mount-{version}-{{arch}}.tar.gz"));
        // The files plugin's own entry: its binary, and nothing privileged,
        // nothing to enable, nothing to unmount.
        let files = plugin("files");
        assert_eq!(files["mount_root"], serde_json::Value::Null);
        assert_eq!(files["enable"], serde_json::json!([]));
        let files_files: Vec<&str> =
            files["files"].as_array().unwrap().iter().map(|f| f["dest"].as_str().unwrap()).collect();
        assert_eq!(files_files, vec!["/usr/local/lib/ritornello/plugins/ritornello-plugin-files"]);

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

        // R22: where the inventory says the main binary lands is where the
        // updater and the archive reader look for it. Both sides name these
        // paths from `update::archive`; the inventory is generated apart, in
        // Python, so nothing else makes them agree.
        let core_binary = format!("/{}", crate::update::archive::CORE_BINARY);
        assert!(
            inv["core"]["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["dest"] == core_binary.as_str()),
            "the core's inventory does not place its binary at {core_binary}"
        );
        let plugins_prefix = format!("/{}", crate::update::archive::PLUGINS_PREFIX);
        for p in plugins {
            let main = format!("{plugins_prefix}ritornello-plugin-{}", p["name"].as_str().unwrap());
            assert!(
                p["files"].as_array().unwrap().iter().any(|f| f["dest"] == main.as_str()),
                "{}: the inventory does not place its binary at {main}",
                p["name"]
            );
        }

        // The root-run binaries (the updater and the like) are the one
        // privileged thing a plugin name does not predict: each one the
        // manifest declares must be placed by the inventory, at its
        // declared destination.
        let m = manifest();
        let mut extra = 0;
        for (name, c) in m.all() {
            let placed = if name == "core" {
                &inv["core"]
            } else if m.companions.contains_key(name) {
                companion(name)
            } else {
                plugin(name)
            };
            for e in &c.extra_binaries {
                let dest = format!("/{}", e.to);
                assert!(
                    placed["files"].as_array().unwrap().iter().any(|f| f["dest"] == dest.as_str()),
                    "{name}: the inventory does not place the extra binary {dest}"
                );
                extra += 1;
            }
        }
        assert!(extra > 0, "saw no extra binary — the walk is wrong");

        let mut privileged = 0;
        for c in std::iter::once(&inv["core"]).chain(plugins.iter()).chain(companions.iter()) {
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
        // No plugin's own entry is privileged: that is what lets the web UI
        // update a plugin archive, and its companion's job instead.
        for p in plugins {
            for f in p["files"].as_array().unwrap() {
                assert_ne!(f["privileged"], true, "{}: places the privileged {}", p["name"], f["dest"]);
            }
        }
    }

    /// Fix round 1, R14: `deploy/mpd.example.toml` carried a literal
    /// `</content>` line — an editor artefact, not TOML — that would have
    /// been copied verbatim into `mpd`'s own data directory by `ritornello-install`
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
